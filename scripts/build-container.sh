#!/bin/sh
set -eu
umask 077
# Separate invocations keep the client's human-ui feature out of the core graph.
sh /workspace/scripts/cargo.sh build --release --locked --offline -j 2 -p agent-runtime --bin agent-runtime "$@"
sh /workspace/scripts/cargo.sh build --release --locked --offline -j 2 -p runtime-operator --bin agentctl
mkdir -p /cache/bin
cp /cache/target/release/agent-runtime /cache/bin/agent-runtime.new
mv /cache/bin/agent-runtime.new /cache/bin/agent-runtime
cp /cache/target/release/agentctl /cache/bin/agentctl.new
mv /cache/bin/agentctl.new /cache/bin/agentctl
