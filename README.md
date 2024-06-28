# viewercount

Just a small executable I made for myself using
[rust-nostr](https://rust-nostr.org) to update the viewer count on
[zap.stream](https://zap.stream) live streams.

If you self-host your own stream (by providing a m3u8 URL), then by default,
zap.stream always shows '0 viewers' because the stream is not hosted by
zap.stream's servers, so they can't possibly know the number of viewers by
themselves.

Fortunately, [nostr NIP53 Live Activies
replaceable events](https://github.com/nostr-protocol/nips/blob/master/53.md)
may contain a special `current_participants` tag that signals to nostr clients
the number of live stream viewers.

There was no previously existing software that allowed to change that tag.

I made `viewercount` so that you can run it on the same machine that is hosting
the m3u8 live stream and periodically update the `current_participants` tag of
your live stream NIP53 events.

## Install

```bash
cargo install viewercount
```

The `viewercount` binary will be in `~/.cargo/bin/viewercount`

Or manually:

```bash
git clone https://github.com/sommerfelddev/viewercount
cd viewercount
cargo build --release
```

The `viewercount` binary will be in `target/release/viewercount`.


## Usage

```
Usage: viewercount [OPTIONS] [NADDRS]...

Arguments:
  [NADDRS]...  specific naddrs of Live Events to update, if none, all user authored Live Events that are 'live' will be updated

Options:
  -i, --interval <INTERVAL>  watch interval in seconds [default: 60]
      --reset-nip46          remove previously cached NIP46 signer credentials and ask for new ones
  -h, --help                 Print help
  -V, --version              Print version
```

You can just run it without any arguments. It will ask for a [NIP46 nsecbunker
URI](https://github.com/nostr-protocol/nips/blob/master/46.md) which you can
easily obtain if using [Amber](https://github.com/greenart7c3/Amber),
[Keystache](https://github.com/Resolvr-io/Keystache),
[nsec.app](https://nsec.app/) or [nsecbunker.com](https://nsecbunker.com/).

You just need to copy paste the URI on the first run, and it will then use your
[NIP65 outbox relays](https://github.com/nostr-protocol/nips/blob/master/65.md)
in combination with [zap.stream's default relays](https://github.com/v0l/zap.stream/blob/f369faf9c0242f0dd7f6cfff52547f86e20127fc/src/const.ts#L27-L32).

The daemon will measure the number of live stream HLS connections by checking
for system TCP sockets on source port 443 with an established state that lasts
longer than 1 second.
