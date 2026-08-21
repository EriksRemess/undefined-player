#!/bin/sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
prefix=${PREFIX:-"$HOME/.local"}

install -Dm755 "$script_dir/bin/undefined-player" \
    "$prefix/bin/undefined-player"
install -Dm644 "$script_dir/share/applications/undefined-player.desktop" \
    "$prefix/share/applications/undefined-player.desktop"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$prefix/share/applications"
fi

printf 'Installed undefined-player under %s\n' "$prefix"
