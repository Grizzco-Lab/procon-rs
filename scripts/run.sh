#!/usr/bin/env bash
# Build and run the studio on the capture host

set -euo pipefail

WORKDIR=$(git rev-parse --show-toplevel)
cd "$WORKDIR"

# Secrets such as ANTHROPIC_API_KEY come from an env file, never from this
# script or config.toml: $PROCON_ENV, else ~/.config/procon/env (outside the
# repo and Dropbox), else a git-ignored .env here. Lines are KEY=value;
# variables already set in the shell win.
load_env() {
    local file=$1
    [ -f "$file" ] || return 0
    if [ "$(stat -c %a "$file")" != 600 ]; then
        echo "warning: $file is readable by others; run: chmod 600 $file" >&2
    fi
    local line key value
    while IFS= read -r line || [ -n "$line" ]; do
        line=${line%$'\r'}
        case $line in '' | '#'*) continue ;; esac
        line=${line#export }
        key=${line%%=*}
        [[ $key =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
        value=${line#*=}
        # Strip one pair of surrounding quotes
        if [[ $value =~ ^\"(.*)\"$ || $value =~ ^\'(.*)\'$ ]]; then
            value=${BASH_REMATCH[1]}
        fi
        [ -n "${!key+set}" ] || export "$key=$value"
    done <"$file"
    echo "Loaded environment from $file"
}
if [ -n "${PROCON_ENV:-}" ]; then
    load_env "$PROCON_ENV"
elif [ -f "$HOME/.config/procon/env" ]; then
    load_env "$HOME/.config/procon/env"
else
    load_env .env
fi

# After an update the build takes a while (about 15-45 s for the code, over
# a minute when dependencies change); the dashboard answers once it is done
echo "Building the studio (cargo build --release)..."
cargo build --release --bin procon
exec target/release/procon --config config.toml
