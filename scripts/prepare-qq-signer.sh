#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Install a user-provided module; no bundled binary, download or process.
set -euo pipefail
umask 077
if [[ $# != 1 ]]; then
  echo 'Usage: bash scripts/prepare-qq-signer.sh /absolute/path/to/wrapper.core.node' >&2
  echo 'Supply your own compatible signing module; this project does not distribute it.' >&2
  exit 2
fi
runtime_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
runtime_source=$1
[[ "$runtime_source" = /* ]] || { echo 'Source must be an absolute file path.' >&2; exit 2; }
runtime_destination="$runtime_root/.runtime/cache/qq-native"
runtime_hash=9a39a5c6de727a3def1c6736f9c3fe8cb9147e5bee22cb77538511a4016b5566
[[ -f "$runtime_source" && ! -L "$runtime_source" ]]
[[ "$(sha256sum "$runtime_source" | cut -d ' ' -f 1)" == "$runtime_hash" ]]
mkdir -p "$runtime_destination" "$runtime_root/.runtime/state"
chmod 700 "$runtime_destination" "$runtime_root/.runtime/state"
exec 9>"$runtime_root/.runtime/state/runtime.lock"
flock --nonblock 9
runtime_temporary=$(mktemp "$runtime_destination/.signer-XXXXXXXX")
trap 'rm -f "$runtime_temporary"' EXIT
cp --reflink=auto -- "$runtime_source" "$runtime_temporary"
chmod 600 "$runtime_temporary"
[[ "$(sha256sum "$runtime_temporary" | cut -d ' ' -f 1)" == "$runtime_hash" ]]
mv -- "$runtime_temporary" "$runtime_destination/wrapper.core.node"
runtime_config=$(mktemp "$runtime_root/.runtime/state/.qq-config-XXXXXXXX")
printf '%s\n' '{"schema":1,"signer_module":"/cache/qq-native/wrapper.core.node"}' >"$runtime_config"
chmod 600 "$runtime_config"
mv -- "$runtime_config" "$runtime_root/.runtime/state/qq-config.json"
printf '%s\n' 'QQ research signer prepared; native signing is still not a Rust implementation.'
