#!/usr/bin/env bash
# Build and run the studio on the capture host

set -euo pipefail

WORKDIR=$(git rev-parse --show-toplevel)
cd "$WORKDIR"

cargo run --release --bin procon -- --config config.toml
