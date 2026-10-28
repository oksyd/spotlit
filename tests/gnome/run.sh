#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == --compositor ]]; then
    shift
    args=("$@")
    for ((i = 1; i < ${#args[@]}; i++)); do
        if [[ "${args[i - 1]}" == --virtual-monitor ]]; then
            args[i]="${SPOTLIT_TEST_MONITOR:-1280x720}"
        fi
    done
    exec "${args[@]}"
fi

: "${SPOTLIT_TEST_WALLPAPER:?Set SPOTLIT_TEST_WALLPAPER to a local test image}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"
bash extensions/gnome-shell/package.sh
test_root="$(mktemp -d "$repo_root/target/gnome-info-test-XXXXXX")"
trap 'rm -rf "$test_root"' EXIT
mkdir -m 700 "$test_root/runtime"
export XDG_RUNTIME_DIR="$test_root/runtime"
export XDG_CONFIG_HOME="$test_root/config"
export XDG_DATA_HOME="$test_root/data"
export XDG_CACHE_HOME="$test_root/cache"
export XDG_STATE_HOME="$test_root/state"
export GIO_USE_VFS=local
export SPOTLIT_TEST_OUTPUT="$repo_root/target/gnome-info-tests/${SPOTLIT_TEST_MONITOR:-1280x720}"

# Both buses and all writable desktop state belong to this test session.
timeout 45s dbus-run-session --config-file tests/gnome/session-bus.conf -- \
    bash -c 'export DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"; exec "$@"' -- \
    gnome-shell-test-tool --headless --wrap 'bash tests/gnome/run.sh --compositor' \
    --extension target/gnome-extension/lock-screen@spotlit.app.shell-extension.zip \
    "${SPOTLIT_TEST_SCRIPT:-tests/gnome/wallpaper-info.js}"
