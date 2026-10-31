#!/bin/sh
set -eu
umask 077
mkdir -p /cache/weixin-work
cp -pR /workspace/crates/platform-weixin/src /cache/weixin-work/
cp -p /workspace/crates/platform-weixin/Cargo.toml /cache/weixin-work/Cargo.toml
if [ -d /workspace/crates/platform-weixin/examples ]; then cp -pR /workspace/crates/platform-weixin/examples /cache/weixin-work/; fi
printf '\n[workspace]\n' >> /cache/weixin-work/Cargo.toml
cd /cache/weixin-work
exec cargo "$@"
