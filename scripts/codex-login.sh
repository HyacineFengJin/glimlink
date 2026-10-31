#!/bin/sh
# Explicit user-invoked subscription login; never called by build/probe/smoke.
set -eu
umask 077
if [ ! -d /state ]; then
  echo 'Login needs the isolated run profile with /state mounted.' >&2
  exit 2
fi
mkdir -p /state/codex
chmod 700 /state/codex
exec 9>/state/codex/.agent-runtime-engine.lock
chmod 600 /state/codex/.agent-runtime-engine.lock
flock --nonblock 9 || { echo 'Codex credential home is busy; finish the active model/login process first.' >&2; exit 1; }
exec env -i HOME=/state/codex CODEX_HOME=/state/codex \
  PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 \
  /cache/bin/codex-native -c 'cli_auth_credentials_store="file"' login --device-auth "$@"
