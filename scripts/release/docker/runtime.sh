#!/bin/sh
# SPDX-License-Identifier: MIT
# Host-side SSH transport entry point; all Runtime arguments stay inside Docker.
# Stop the daemon before login/MCP, as these commands share its state lock.
set -eu
exec /usr/bin/docker compose --project-directory /opt/glimlink \
    --file /opt/glimlink/compose.yaml run --rm --no-deps -T \
    --name "glimlink-control-$$" control "$@"
