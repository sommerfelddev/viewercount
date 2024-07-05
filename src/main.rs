use std::{
    collections::HashMap,
    fs::{rename, File},
    io::{stdin, BufReader, Write},
    net::IpAddr,
    path::PathBuf,
    process::exit,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use clap::{command, Parser};
use futures::future::join_all;
use human_panic::setup_panic;
use log::{error, info, warn};
use nostr_relay_pool::RelaySendOptions;
use nostr_sdk::{
    nips::{
        nip01::Coordinate,
        nip46::NostrConnectURI,
        nip53::LiveEventStatus,
        nip65::{self, RelayMetadata},
    },
    Client, Event, EventBuilder, Filter, Keys, Kind, Tag, TagKind, TagStandard, ToBech32,
};
use nostr_signer::{Nip46Signer, NostrSigner};
#[cfg(any(target_os = "linux", target_os = "android"))]
use procfs::net::TcpState;
use serde::{Deserialize, Serialize};
use serde_json::{from_reader, to_string};
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};
use tokio::{spawn, time::sleep};

const DEFAULT_WATCH_INTERVAL_SEC: u64 = 60;
const NIP46_TIMEOUT_SEC: u64 = 60;
const USERKIND_RELAY: &str = "wss://purplepag.es";
// Ref: https://github.com/v0l/zap.stream/blob/f369faf9c0242f0dd7f6cfff52547f86e20127fc/src/const.ts#L27-L32
const ZAP_STREAM_RELAYS: &[&str] = &[
    "wss://relay.snort.social",
    "wss://nos.lol",
    "wss://relay.damus.io",
    // This one is a paid relay so it should not be in the default list
    //"wss://nostr.wine",
    // This last one is not in zap.stream's default list, but it's still useful to make sure the
    // updates get spread out as much as possible
    "wss://nostr.mutinywallet.com",
];
const TCP_CHECK_DELAY_SEC: u64 = 1;

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// watch interval in seconds
    #[arg(short, long, default_value_t = DEFAULT_WATCH_INTERVAL_SEC)]
    interval: u64,
    /// remove previously cached NIP46 signer credentials and ask for new ones
    #[arg(long)]
    reset_nip46: bool,
    /// specific naddrs of Live Events to update, if none, all user authored Live Events that are
    /// 'live' will be updated
    naddrs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ClientData {
    client_nsec: String,
    nip46_uri: String,
}

impl ClientData {
    async fn generate() -> Result<Self> {
        let keys = Keys::generate();
        println!("Paste NSECBUNKER URI (this only needs to be done once):");
        let mut line = String::new();
        stdin().read_line(&mut line)?;
        let nip46_uri = line.trim_end().to_string();

        Ok(ClientData {
            client_nsec: keys.secret_key()?.to_bech32()?,
            nip46_uri,
        })
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = do_main().await {
        error!("{:#}", e);
        exit(1);
    }
}

async fn do_main() -> Result<()> {
    setup_panic!();
    let args = Args::parse();
    set_logger();
    set_signal_handlers().context("failed to setup a signal termination handler")?;

    let client = setup_nostr_client(
        args.reset_nip46,
        Duration::from_secs(NIP46_TIMEOUT_SEC),
        ZAP_STREAM_RELAYS,
    )
    .await?;
    info!("nostr client connected");

    watch_count(client, &args.naddrs, Duration::from_secs(args.interval)).await?;
    Ok(())
}

fn set_logger() {
    let mut builder = pretty_env_logger::formatted_timed_builder();
    match std::env::var("RUST_LOG") {
        Ok(_) => builder.parse_default_env(),
        Err(_) => builder.filter_module(env!("CARGO_PKG_NAME"), log::LevelFilter::Info),
    };
    builder.init();
}

fn set_signal_handlers() -> Result<()> {
    tokio::spawn(async move {
        if let Err(e) = tokio::signal::ctrl_c().await {
            return e;
        }
        warn!("received ctrl-c signal. Exiting...");
        exit(0)
    });
    #[cfg(unix)]
    tokio::spawn(async move {
        let mut stream = match signal(SignalKind::terminate()) {
            Err(e) => return e,
            Ok(s) => s,
        };
        stream.recv().await;
        warn!("received process termination signal. Exiting...");
        exit(0)
    });
    #[cfg(unix)]
    tokio::spawn(async move {
        let mut stream = match signal(SignalKind::hangup()) {
            Err(e) => return e,
            Ok(s) => s,
        };
        stream.recv().await;
        warn!("received process hangup signal. Exiting...");
        exit(0)
    });
    Ok(())
}

async fn setup_nostr_client(
    reset_nip46: bool,
    nip46_timeout: Duration,
    client_relays: &[&str],
) -> Result<Client> {
    let signer = create_signer(reset_nip46, nip46_timeout).await?;
    let signer_pubkey = signer.signer_public_key();
    let client = Client::new(NostrSigner::from(signer));

    setup_client_relays(&client, signer_pubkey, client_relays).await?;

    info!("connecting to client");
    client.connect().await;
    Ok(client)
}

async fn create_signer(reset_nip46: bool, nip46_timeout: Duration) -> Result<Nip46Signer> {
    if reset_nip46 {
        let path = get_client_data_path();
        let path_str = path
            .to_str()
            .ok_or(anyhow!("Cannot convert path to string"))?;
        let _ = rename(path_str, format!("{}.bak", path_str));
    }
    let client_data = get_or_generate_client_data().await?;
    let client_keys = Keys::parse(client_data.client_nsec)?;
    info!("setting up NIP46 signer");
    let signer = Nip46Signer::new(
        NostrConnectURI::parse(client_data.nip46_uri)?,
        client_keys,
        nip46_timeout,
        None,
    )
    .await?;
    Ok(signer)
}

async fn get_or_generate_client_data() -> Result<ClientData> {
    let path = get_client_data_path();
    match File::open(&path) {
        Ok(file) => {
            let reader = BufReader::new(file);
            from_reader(reader)
                .with_context(|| format!("cannot read client data from '{}'", path.display()))
        }
        Err(_) => {
            let nostr_data = ClientData::generate().await?;
            let mut file = File::create(&path)?;
            file.write_all(to_string(&nostr_data)?.as_bytes())
                .with_context(|| format!("could not write client data to '{}'", path.display()))?;
            Ok(nostr_data)
        }
    }
}

fn get_client_data_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or(PathBuf::from("data"))
        .join(format!("{}.json", env!("CARGO_PKG_NAME")))
}

async fn setup_client_relays(
    client: &Client,
    signer_pubkey: nostr_sdk::PublicKey,
    client_relays: &[&str],
) -> Result<(), anyhow::Error> {
    client.add_relay(USERKIND_RELAY).await?;
    client.connect().await;
    let filter = Filter::new().author(signer_pubkey).kind(Kind::RelayList);
    let events: Vec<Event> = client
        .get_events_of(vec![filter], Some(Duration::from_secs(10)))
        .await?;
    let event = events.first().ok_or(anyhow!("no matching events"))?;
    info!("using NIP65 user outbox relays");
    let nip65_relays = nip65::extract_relay_list(event)
        .iter()
        .filter_map(|x| {
            let url = x.0;
            let meta = x.1;
            if meta.as_ref().is_some_and(|m| *m == RelayMetadata::Write) || meta.is_none() {
                return Some(url.to_string());
            }
            None
        })
        .collect::<Vec<_>>();
    let mut all_relays = nip65_relays
        .iter()
        .map(|r| r.trim_end_matches('/'))
        .collect::<Vec<_>>();
    all_relays.extend_from_slice(client_relays);
    all_relays.sort_unstable();
    all_relays.dedup();
    info!("using relays {:?}", all_relays);
    client.remove_relay(USERKIND_RELAY).await?;
    client.add_relays(all_relays).await?;
    Ok(())
}

async fn watch_count(client: Client, naddrs: &[String], interval: Duration) -> Result<()> {
    let filters = create_filters(naddrs, &client).await?;
    loop {
        let handle = spawn(sleep(interval));
        let count = get_count().await.context("failed ot read count")?;
        info!("count: {}", count);

        update_count(&client, &filters, count).await?;

        info!(" waiting for new cycle in {:#?}", interval);
        // wait the rest of the watch interval
        handle.await?;
    }
}

async fn create_filters(naddrs: &[String], client: &Client) -> Result<Vec<Filter>> {
    Ok(match naddrs.is_empty() {
        true => vec![Filter::new()
            .author(client.signer().await?.public_key().await?)
            .kind(Kind::LiveEvent)],
        false => naddrs
            .iter()
            .map(|naddr| {
                Filter::from(Coordinate::parse(naddr).unwrap_or_else(|_| {
                    error!("{naddr} is not a valid replaceable event coordinate");
                    exit(1);
                }))
            })
            .collect::<Vec<_>>(),
    })
}

async fn get_count() -> Result<u64> {
    if cfg!(any(target_os = "linux", target_os = "android")) {
        return get_count_from_procfs().await;
    }
    bail!("unsupported OS")
}

#[cfg(any(target_os = "linux", target_os = "android"))]
async fn get_count_from_procfs() -> Result<u64> {
    let mut tcp = get_https_connected_ips()?;
    sleep(Duration::from_secs(TCP_CHECK_DELAY_SEC)).await;
    tcp.append(&mut get_https_connected_ips()?);
    tcp.sort_unstable();
    tcp.dedup();
    Ok(tcp.len() as u64)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn get_https_connected_ips() -> Result<Vec<IpAddr>> {
    Ok(procfs::net::tcp()?
        .into_iter()
        .chain(procfs::net::tcp6()?)
        .filter_map(|t| {
            if t.local_address.port() == 443 && t.state == TcpState::Established {
                return Some(t.remote_address.ip());
            }
            None
        })
        .collect::<Vec<_>>())
}

async fn update_count(client: &Client, filters: &[Filter], count: u64) -> Result<()> {
    let events = get_relevant_events(client, filters).await?;
    if events.is_empty() {
        warn!("no live events");
        return Ok(());
    }

    let new_events = get_updated_events(events, client, count).await;
    if new_events.is_empty() {
        return Ok(());
    }

    info!("broadcasting {} updated events", new_events.len());
    client
        .batch_event(new_events, RelaySendOptions::new())
        .await?;
    Ok(())
}

async fn get_relevant_events(client: &Client, filters: &[Filter]) -> Result<Vec<Event>> {
    let events = client.get_events_of(filters.to_vec(), None).await?;

    let mut events_by_naddr: HashMap<String, Event> = Default::default();
    for event in events {
        let naddr = get_naddr(&event).to_string();
        if let Some(dup_event) = events_by_naddr.get_mut(&naddr) {
            if event.created_at() > dup_event.created_at() {
                *dup_event = event;
            }
        } else {
            events_by_naddr.insert(naddr, event);
        }
    }

    Ok(events_by_naddr
        .into_values()
        .filter(|e| e.tags().iter().any(is_live_event_live))
        .collect())
}

fn is_live_event_live(t: &Tag) -> bool {
    t.as_standardized().is_some_and(|ts| match ts {
        TagStandard::LiveEventStatus(s) => *s == LiveEventStatus::Live,
        _ => false,
    })
}

async fn get_updated_events(events: Vec<Event>, client: &Client, count: u64) -> Vec<Event> {
    join_all(
        events
            .iter()
            .map(|e| get_event_with_updated_count(e, client, count)),
    )
    .await
    .into_iter()
    .filter_map(|event| match event {
        Ok(ev) => Some(ev),
        Err(e) => {
            error!("event could not be updated: {}", e);
            None
        }
    })
    .collect::<Vec<_>>()
}

async fn get_event_with_updated_count(event: &Event, client: &Client, count: u64) -> Result<Event> {
    let naddr = get_naddr(event).to_bech32()?;
    let new_event = client
        .sign_event_builder(get_event_builder_with_updated_count(event, count))
        .await
        .context(format!("cannot sign {naddr}"))?;
    info!("updating {naddr}");
    Ok(new_event)
}

fn get_event_builder_with_updated_count(event: &Event, count: u64) -> EventBuilder {
    let mut tags: Vec<Tag> = event
        .clone()
        .into_iter_tags()
        .filter(|t| t.kind() != TagKind::CurrentParticipants)
        .collect();
    tags.push(TagStandard::CurrentParticipants(count).into());
    let event_builder = EventBuilder::new(event.kind(), event.content(), tags);
    event_builder
}

fn get_naddr(event: &Event) -> Coordinate {
    Coordinate::new(event.kind(), event.pubkey).identifier(event.identifier().unwrap_or_default())
}
