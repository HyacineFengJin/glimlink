#!/bin/sh
# Explicit Google subscription login in a dedicated Docker HOME. No host keyring.
set -eu
umask 077
if [ ! -d /state ]; then
  echo 'Login needs the isolated run profile with /state mounted.' >&2
  exit 2
fi
mkdir -p /state/antigravity/.gemini/antigravity-cli /state/antigravity/cache /state/antigravity/app-data /state/antigravity-login-workspace
chmod 700 /state/antigravity /state/antigravity/.gemini /state/antigravity/.gemini/antigravity-cli /state/antigravity/cache /state/antigravity/app-data /state/antigravity-login-workspace
cd /state/antigravity-login-workspace
exec 9>/state/antigravity/.agent-runtime-engine.lock
chmod 600 /state/antigravity/.agent-runtime-engine.lock
flock --nonblock 9 || { echo 'Antigravity credential home is busy; finish the active model/login process first.' >&2; exit 1; }
# SSH mode uses Google's authorization URL/code flow, with the browser on the
# operator's computer. It does not attach a display or desktop D-Bus socket.
exec env -i HOME=/state/antigravity \
  XDG_CONFIG_HOME=/state/antigravity/.gemini \
  XDG_DATA_HOME=/state/antigravity/app-data XDG_CACHE_HOME=/state/antigravity/cache \
  PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 TERM=xterm-256color \
  BROWSER=/usr/bin/false SSH_CONNECTION='runtime 0 vps 0' \
  AGY_CLI_DISABLE_AUTO_UPDATE=true \
  /cache/bin/antigravity-native --disable-slash-commands --settings '{"toolPermission":"strict","permissions":{"deny":["command(*)","read_url(*)","execute_url(*)","write_file(*)"],"allow":[],"ask":[]}}' "$@"
