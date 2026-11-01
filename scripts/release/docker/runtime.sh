#!/bin/sh
# SPDX-License-Identifier: MIT
# Host-side SSH transport entry point; all Runtime arguments stay inside Docker.
# Stop the daemon before login/MCP, as these commands share its state lock.
set -eu
if [ "$#" -eq 4 ] && [ "$1" = --state ] && [ "$2" = /state ] && [ "$3" = operator ]; then
    case "$4" in
      worker-status)
        runtime_running=$(/usr/bin/docker inspect --format '{{.State.Running}}' glimlink-runtime 2>/dev/null || true)
        if [ "$runtime_running" = true ]; then printf '{"ok":true,"running":true}\n'; else printf '{"ok":true,"running":false}\n'; fi
        exit 0 ;;
      worker-start)
        /usr/bin/docker compose --project-directory /opt/glimlink --file /opt/glimlink/compose.yaml up -d runtime >&2
        printf '{"ok":true,"running":true}\n'
        exit 0 ;;
      worker-stop)
        /usr/bin/docker compose --project-directory /opt/glimlink --file /opt/glimlink/compose.yaml stop runtime >&2
        printf '{"ok":true,"running":false}\n'
        exit 0 ;;
    esac
fi
runtime_service=control
if [ "$#" -eq 4 ] && [ "$1" = --state ] && [ "$2" = /state ] && [ "$3" = operator ] && [ "$4" = manage ]; then
    runtime_service=management
fi
exec /usr/bin/docker compose --project-directory /opt/glimlink \
    --file /opt/glimlink/compose.yaml run --rm --no-deps -T \
    --name "glimlink-control-$$" "$runtime_service" "$@"
