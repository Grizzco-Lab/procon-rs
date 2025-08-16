#!/usr/bin/env bash

set -euo pipefail

WORKDIR=$(git rev-parse --show-toplevel)
cd $WORKDIR

# build
cargo build --release

# run
# sudo RUST_LOG=info taskset -c 0 chrt -f 50 ionice -c 1 -n 0 ./target/release/procon
sudo RUST_LOG=info ./target/release/procon
