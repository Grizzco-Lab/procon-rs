#!/usr/bin/env bash
# Cross-compile procon-pi on this host, copy it with pi.toml to the Pi, and restart it there
#
# Usage: scripts/deploy.sh [ssh-host]   (default: pi4)
#
# The first run copies the Pi's C libraries into target/pi-sysroot so clang and
# Rust's bundled lld can link against them; nothing needs installing on either side.

set -euo pipefail

HOST=${1:-pi4}
DEST=procon
TARGET=aarch64-unknown-linux-gnu

WORKDIR=$(git rev-parse --show-toplevel)
cd "$WORKDIR"
SYSROOT="$WORKDIR/target/pi-sysroot"

if [ ! -d "$SYSROOT" ]; then
    echo "Copying the Pi's system libraries into $SYSROOT (first run only)..."
    mkdir -p "$SYSROOT/usr/lib/aarch64-linux-gnu"
    rsync -a "$HOST:/usr/include" "$SYSROOT/usr/"
    # Kernel headers that /usr/include/aarch64-linux-gnu/asm links into
    rsync -a "$HOST:/usr/lib/linux" "$SYSROOT/usr/lib/"
    rsync -a "$HOST:/usr/lib/gcc" "$SYSROOT/usr/lib/"
    rsync -a "$HOST:/usr/lib/ld-linux-aarch64.so.1" "$SYSROOT/usr/lib/"
    rsync -a \
        --include='*crt*.o' --include='libc.so*' --include='libc_nonshared.a' \
        --include='libm.so*' --include='libmvec.so*' --include='libpthread*' \
        --include='libdl*' --include='librt*' --include='libutil*' \
        --include='libgcc_s*' --include='libudev.so*' --include='ld-linux-aarch64.so*' \
        --include='pkgconfig/' --include='pkgconfig/libudev.pc' --exclude='*' \
        "$HOST:/usr/lib/aarch64-linux-gnu/" "$SYSROOT/usr/lib/aarch64-linux-gnu/"
    # /lib is a symlink to usr/lib on the Pi, and glibc's linker scripts use it
    ln -sfn usr/lib "$SYSROOT/lib"
    # Links like libm.so -> /lib/... must point into the sysroot, not this host
    find "$SYSROOT" -type l -lname '/*' | while read -r link; do
        ln -sfn "$SYSROOT$(readlink "$link")" "$link"
    done
fi

# clang compiles hidapi's C code and drives the link; lld comes with Rust
FLAGS="--target=aarch64-linux-gnu --sysroot=$SYSROOT"
LLD_DIR="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld"
export CC_aarch64_unknown_linux_gnu=clang
export CFLAGS_aarch64_unknown_linux_gnu="$FLAGS"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=clang
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-Clink-arg=--target=aarch64-linux-gnu -Clink-arg=--sysroot=$SYSROOT -Clink-arg=-fuse-ld=lld -Clink-arg=-B$LLD_DIR"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
export PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/aarch64-linux-gnu/pkgconfig"
cargo build --release --target "$TARGET" --bin procon-pi

echo "Copying to $HOST:$DEST..."
ssh "$HOST" "mkdir -p $DEST"
rsync -avP "target/$TARGET/release/procon-pi" pi.toml "$HOST:$DEST/"

echo "Restarting procon-pi on $HOST..."
# procon was the binary's old name; setsid and nohup keep it alive after ssh leaves
ssh -t "$HOST" "cd $DEST && sudo sh -c 'pkill -x procon-pi; pkill -x procon; sleep 1; \
    RUST_LOG=info setsid nohup ./procon-pi --config pi.toml > procon-pi.log 2>&1 < /dev/null &'"
sleep 3
ssh "$HOST" "tail -n 5 $DEST/procon-pi.log"
