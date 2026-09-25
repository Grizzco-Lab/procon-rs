#!/usr/bin/env bash
# Build and run the Pi proxy on the Pi itself (scripts/deploy.sh does it from the host)

set -euo pipefail

WORKDIR=$(git rev-parse --show-toplevel)
cd "$WORKDIR"

# build
cargo build --release --bin procon-pi

# run
# sudo RUST_LOG=info taskset -c 0 chrt -f 50 ionice -c 1 -n 0 ./target/release/procon-pi --config pi.toml
sudo RUST_LOG=info ./target/release/procon-pi --config pi.toml
