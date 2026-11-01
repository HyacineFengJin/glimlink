#!/bin/sh
# SPDX-License-Identifier: MIT
# Install an already-extracted, verified release. No root, login, model or service start.
set -eu
umask 077
release_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
prefix=${1:-"$HOME/.local"}
case "$prefix" in /*) ;; *) echo 'Use an absolute installation prefix.' >&2; exit 2;; esac
mkdir -p "$prefix/bin" "$prefix/share/glimlink" "$HOME/.config/systemd/user" "$HOME/.local/state/glimlink/config"
chmod 700 "$HOME/.local/state/glimlink" "$HOME/.local/state/glimlink/config"
for name in agent-runtime agentctl; do
  install -m 755 "$release_dir/bin/$name" "$prefix/bin/$name.new"
  mv -f "$prefix/bin/$name.new" "$prefix/bin/$name"
done
cp -R "$release_dir/share/glimlink/." "$prefix/share/glimlink/"
chmod -R go+rX "$prefix/share/glimlink"
for file in LICENSE NOTICE COPYING.AGPL-3.0 COPYING.GPL-3.0 DISTRIBUTION.txt BUILD.json DEPLOYMENT.md engine-login.md deployment-docker.md control-console.md; do
  install -m 644 "$release_dir/$file" "$prefix/share/glimlink/$file"
done
for file in profile-codex.json profile-antigravity.json browsing.json persona.txt role.txt; do
  test -e "$HOME/.local/state/glimlink/config/$file" || install -m 600 "$release_dir/share/glimlink/config/$file" "$HOME/.local/state/glimlink/config/$file"
done
# The supplied service uses the default user-local prefix; a custom prefix is manual.
if [ "$prefix" = "$HOME/.local" ]; then
  install -m 600 "$release_dir/share/glimlink/glimlink.service" "$HOME/.config/systemd/user/glimlink.service"
fi
echo "Installed into $prefix. No service or login was started."
echo "Next: $prefix/bin/agent-runtime doctor"
