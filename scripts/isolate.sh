#!/usr/bin/env bash
set -euo pipefail
umask 077
runtime_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
runtime_image=rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f
runtime_build=false
runtime_account_free=false
runtime_client_test=false
runtime_network=none
runtime_stdin=()
runtime_qq_import=false
runtime_operator_ui=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    build) runtime_build=true ;;
    run) runtime_build=false ;;
    test) runtime_build=false; runtime_account_free=true ;;
    client-test) runtime_build=false; runtime_client_test=true ;;
    online) runtime_network=bridge ;;
    stdio) runtime_stdin=(--interactive) ;;
    terminal) runtime_stdin=(--interactive --tty) ;;
    qq-import) runtime_qq_import=true ;;
    operator-ui) runtime_operator_ui=true ;;
    operator-webview) runtime_image=agent-runtime-operator-webview:dev ;;
    --) shift; break ;;
    *) break ;;
  esac
  shift
done
if [[ $# == 0 ]]; then
  echo 'Usage: bash scripts/isolate.sh [build|run|test|client-test] [operator-webview] [online] [stdio|terminal] -- COMMAND [ARG ...]' >&2
  exit 2
fi
mkdir -p "$runtime_root/.runtime"/{cache,state,reports}
chmod 700 "$runtime_root/.runtime" "$runtime_root/.runtime/"{cache,state,reports}
runtime_limits=(--memory 384m --memory-swap 384m --cpus 2)
runtime_pids=(--pids-limit 512)
runtime_state=(--mount "type=bind,src=$runtime_root/.runtime/state,dst=/state")
runtime_legacy=()
runtime_native=()
runtime_operator_publish=()
if $runtime_operator_ui; then
  if $runtime_build || [[ "$runtime_network" != bridge ]]; then
    echo 'Operator-client UI requires run online; the Runtime core has no HTTP listener.' >&2
    exit 2
  fi
  runtime_operator_publish=(--publish 127.0.0.1:18799:18799 --env AGENTCTL_TEST_UI=1 --env AGENTCTL_UI_LISTEN_ADDR=0.0.0.0:18799 --env AGENTCTL_UI_ORIGIN=http://127.0.0.1:18799)
fi
if ! $runtime_build && [[ -d "$runtime_root/.runtime/cache/qq-native" ]]; then
  runtime_native=(--mount "type=bind,src=$runtime_root/.runtime/cache/qq-native,dst=/cache/qq-native,readonly")
fi
if $runtime_qq_import; then
  if $runtime_build || [[ "$runtime_network" != none ]]; then
    echo 'QQ import is an offline management action; build/online are invalid.' >&2
    exit 2
  fi
  exec 8>"$runtime_root/experiments/qq-lab/.state/client.lock"
  flock --nonblock 8
  runtime_legacy=(--mount "type=bind,src=$runtime_root/experiments/qq-lab/.state,dst=/legacy-qq,readonly")
fi
if $runtime_build; then
  runtime_limits=()
  runtime_pids=()
  runtime_state=()
elif $runtime_client_test; then
  # The desktop has its own budget; synthetic GUI checks never mount accounts.
  runtime_limits=(--memory 1g --memory-swap 1g --cpus 2)
  runtime_state=()
elif $runtime_account_free; then
  runtime_state=(--tmpfs "/state:rw,nosuid,nodev,noexec,mode=0700,uid=$(id -u),gid=$(id -g),size=64m")
fi
exec docker run --rm --init \
  "${runtime_stdin[@]}" \
  --network "$runtime_network" \
  --read-only --cap-drop ALL --security-opt no-new-privileges:true \
  --user "$(id -u):$(id -g)" \
  "${runtime_pids[@]}" "${runtime_limits[@]}" \
  --ulimit core=0 --ulimit nofile=1024:1024 \
  --tmpfs /tmp:rw,nosuid,nodev,size=128m \
  --mount "type=bind,src=$runtime_root,dst=/workspace,readonly" \
  --tmpfs /workspace/experiments:ro,nosuid,nodev,noexec,size=64k,mode=000 \
  --tmpfs /workspace/.runtime:ro,nosuid,nodev,noexec,size=64k,mode=000 \
  --mount "type=bind,src=$runtime_root/.runtime/cache,dst=/cache" \
  --mount "type=bind,src=$runtime_root/.runtime/reports,dst=/reports" \
  "${runtime_state[@]}" \
  "${runtime_legacy[@]}" \
  "${runtime_native[@]}" \
  "${runtime_operator_publish[@]}" \
  --env HOME=/tmp --env CARGO_HOME=/cache/cargo \
  --env CARGO_TARGET_DIR=/cache/target --env TMPDIR=/tmp \
  --env AGENT_RUNTIME_STATE=/state \
  --workdir /workspace "$runtime_image" "$@"
