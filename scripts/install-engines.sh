#!/bin/sh
# SPDX-License-Identifier: MIT
# Explicit native-engine install from upstream, outside Glimlink's distributed binary.
set -eu
umask 077
engine=${1:?Usage: install-engines.sh codex|antigravity [ABSOLUTE_BIN_DIR]}
destination=${2:-"$HOME/.local/bin"}
case "$destination" in /*) ;; *) echo 'Destination must be absolute.' >&2; exit 2;; esac
[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] || { echo 'This installer supports Linux x86_64.' >&2; exit 2; }
case "$engine" in
  codex)
    url=https://github.com/openai/codex/releases/download/rust-v0.165.0/codex-x86_64-unknown-linux-musl.tar.gz
    expected=be635d3486a5bd060d0fff582ae6d30160280c31344dd85719707fa920e187f0
    member=codex-x86_64-unknown-linux-musl
    installed=codex-native
    ;;
  antigravity)
    url=https://github.com/google-antigravity/antigravity-cli/releases/download/1.4.11/agy_cli_linux_x64.tar.gz
    expected=d17c426977326361f3d81f3ea22820e967164285923de020247e460dcb9d15c3
    member=antigravity
    installed=antigravity-native
    ;;
  *) echo 'Engine must be codex or antigravity.' >&2; exit 2;;
esac
mkdir -p "$destination"
# Native executables exceed many /tmp tmpfs limits. Stage on the target disk.
work=$(mktemp -d "$destination/.glimlink-engine.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
if [ -n "${3:-}" ]; then
  cp -- "$3" "$work/engine.tar.gz"
else
  curl --fail --location --proto '=https' --connect-timeout 20 --max-time 600 "$url" -o "$work/engine.tar.gz"
fi
printf '%s  %s\n' "$expected" "$work/engine.tar.gz" | sha256sum -c -
tar -xzf "$work/engine.tar.gz" -C "$work" "$member"
[ -f "$work/$member" ] && [ ! -L "$work/$member" ] || { echo 'Unexpected upstream archive.' >&2; exit 1; }
chmod 755 "$work/$member"
mv -f "$work/$member" "$destination/$installed.new"
mv -f "$destination/$installed.new" "$destination/$installed"
echo "Installed pinned $engine at $destination/$installed. No login was started."
