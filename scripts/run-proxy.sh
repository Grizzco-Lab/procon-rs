#!/usr/bin/env bash
# Build and run the USB proxy on the Pi itself (scripts/deploy.sh does it from the host)

set -euo pipefail

WORKDIR=$(git rev-parse --show-toplevel)
cd "$WORKDIR"

# build
cargo build --release --bin procon-proxy

# run
# sudo RUST_LOG=info taskset -c 0 chrt -f 50 ionice -c 1 -n 0 ./target/release/procon-proxy --config proxy.toml
sudo RUST_LOG=info ./target/release/procon-proxy --config proxy.toml
