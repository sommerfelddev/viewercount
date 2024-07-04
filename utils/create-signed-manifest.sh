#|/usr/bin/env sh

set -e

script_dir=$(dirname -- "$(readlink -f "$0")")

tag=$(git -C "$script_dir" describe --tags --abbrev=0)

cwd=$(pwd)
cd "$(mktemp -d)"

wget "https://github.com/sommerfelddev/viewercount/releases/download/$tag/viewercount-$tag-linux-x86_64.tar.gz"
wget "https://github.com/sommerfelddev/viewercount/releases/download/$tag/viewercount-$tag-linux-aarch64.tar.gz"

sha256sum -b -- * > viewercount-"$tag"-manifest.txt

sha256sum --check viewercount-"$tag"-manifest.txt

gpg -b --armor viewercount-"$tag"-manifest.txt

gpg --verify viewercount-"$tag"-manifest.txt.asc

pwd

cd "$cwd"
