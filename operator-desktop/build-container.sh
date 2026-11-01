#!/bin/sh
# SPDX-License-Identifier: MIT
# Run through scripts/isolate.sh build operator-webview. Deliberately independent
# from the headless Runtime workspace and Cargo.lock.
set -eu
umask 077
exec 9>/cache/operator-desktop-build.lock
flock 9
operator_work=/cache/operator-desktop-workspace
mkdir -p "$operator_work/crates" "$operator_work/operator-desktop"
rm -rf "$operator_work/crates/runtime-control" "$operator_work/crates/runtime-operator"
cp -pR /workspace/crates/runtime-control /workspace/crates/runtime-operator "$operator_work/crates/"
for source in src frontend capabilities icons; do
  rm -rf "$operator_work/operator-desktop/$source"
  cp -pR "/workspace/operator-desktop/$source" "$operator_work/operator-desktop/"
done
cp -p /workspace/operator-desktop/Cargo.toml /workspace/operator-desktop/Cargo.lock \
  /workspace/operator-desktop/build.rs /workspace/operator-desktop/tauri.conf.json \
  "$operator_work/operator-desktop/"
# Explicit private-workstation artwork only. Original asset files remain ignored
# and retain their provider ownership; public/source-only builds keep text marks.
if [ "${GLIMLINK_OPERATOR_LOCAL_BRANDS:-0}" = 1 ]; then
  for asset in bilibili.ico qq.png tieba.jpg weixin.png github.svg codex.png antigravity.svg; do
    test -f "$operator_work/operator-desktop/frontend/brands/$asset"
  done
  sed -i 's|const brandAssets = Object.freeze({});|const brandAssets = Object.freeze({bilibili:"brands/bilibili.ico",qq:"brands/qq.png",tieba:"brands/tieba.jpg",weixin:"brands/weixin.png",github:"brands/github.svg",codex:"brands/codex.png",antigravity:"brands/antigravity.svg"});|' "$operator_work/operator-desktop/frontend/app.js"
fi
cd "$operator_work/operator-desktop"
export CARGO_TARGET_DIR=/cache/operator-desktop-target
if [ "$#" = 0 ]; then
  cargo build --release --locked --offline -j 4
  mkdir -p /cache/bin
  cp "$CARGO_TARGET_DIR/release/agent-operator-desktop" /cache/bin/agent-operator-desktop.new
  mv /cache/bin/agent-operator-desktop.new /cache/bin/agent-operator-desktop
else
  cargo "$@"
fi
