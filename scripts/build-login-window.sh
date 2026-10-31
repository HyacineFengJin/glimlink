#!/bin/sh
# SPDX-License-Identifier: MIT
# Run with the development operator-webview image, never the VPS image.
set -eu
umask 077
sh /workspace/scripts/cargo.sh build --release --locked --offline -j 2 \
  -p runtime-login-window --features native-webview --bin agent-login-window
mkdir -p /cache/bin
cp /cache/target/release/agent-login-window /cache/bin/agent-login-window.new
mv /cache/bin/agent-login-window.new /cache/bin/agent-login-window
