#!/usr/bin/env bash
# Cross-compile procon-proxy on this host, copy it with proxy.toml to the Pi, and restart it there
#
# Usage: scripts/deploy.sh [ssh-host]   (default: pi4)
#
# The proxy is a static musl binary linked by Rust's bundled lld (see
# .cargo/config.toml), so it needs no C toolchain here and no libraries there.

set -euo pipefail

HOST=${1:-pi4}
DEST=procon
TARGET=aarch64-unknown-linux-musl

cd "$(git rev-parse --show-toplevel)"
cargo build --release --target "$TARGET" --bin procon-proxy

echo "Copying to $HOST:$DEST..."
ssh "$HOST" "mkdir -p $DEST"
rsync -avP "target/$TARGET/release/procon-proxy" proxy.toml "$HOST:$DEST/"

echo "Restarting procon-proxy on $HOST..."
# procon was its old name; setsid and nohup keep it alive after ssh leaves
ssh -t "$HOST" "cd $DEST && sudo sh -c 'pkill -x procon-proxy; pkill -x procon; sleep 1; \
    RUST_LOG=info setsid nohup ./procon-proxy --config proxy.toml > procon-proxy.log 2>&1 < /dev/null &'"
sleep 3
ssh "$HOST" "tail -n 5 $DEST/procon-proxy.log"
