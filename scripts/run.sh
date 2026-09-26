#!/usr/bin/env bash
# Build and run the studio on the capture host

set -euo pipefail

WORKDIR=$(git rev-parse --show-toplevel)
cd "$WORKDIR"

# After an update the build takes a while (about 15-45 s for the code, over
# a minute when dependencies change); the dashboard answers once it is done
echo "Building the studio (cargo build --release)..."
cargo build --release --bin procon
exec target/release/procon --config config.toml
