#!/bin/sh
# SPDX-License-Identifier: MIT
# Host-side SSH transport entry point; all Runtime arguments stay inside Docker.
# Stop the daemon before login/MCP, as these commands share its state lock.
set -eu
if [ "$#" -eq 4 ] && [ "$1" = --state ] && [ "$2" = /state ] && [ "$3" = operator ]; then
    case "$4" in
      worker-memory)
        # Fixed host-side cgroup reads: no extra container, monitor or Docker socket.
        runtime_info=$(/usr/bin/docker inspect --format '{{.State.Running}} {{.State.Pid}} {{.HostConfig.Memory}}' glimlink-runtime 2>/dev/null || true)
        set -- $runtime_info
        if [ "$#" -ne 3 ]; then
          printf '{"ok":true,"available":false,"scope":"runtime_container","error":{"code":"worker_not_found"}}\n'
          exit 0
        fi
        runtime_pid=$2
        runtime_limit=$3
        case "$runtime_pid:$runtime_limit" in *[!0-9:]*|:*) exit 1 ;; esac
        runtime_time=$(date +%s%3N)
        if [ "$1" != true ] || [ "$runtime_pid" -eq 0 ]; then
          printf '{"ok":true,"available":true,"running":false,"scope":"runtime_container","current_bytes":0,"peak_bytes":null,"limit_bytes":%s,"sampled_at_unix_ms":%s}\n' "$runtime_limit" "$runtime_time"
          exit 0
        fi
        runtime_group=$(awk -F: '$1 == "0" {print $3}' "/proc/$runtime_pid/cgroup")
        case "$runtime_group" in /*) ;; *) exit 1 ;; esac
        case "$runtime_group" in *..*) exit 1 ;; esac
        runtime_cgroup="/sys/fs/cgroup$runtime_group"
        if [ ! -r "$runtime_cgroup/memory.current" ]; then
          printf '{"ok":true,"available":false,"scope":"runtime_container","error":{"code":"cgroup_unavailable"}}\n'
          exit 0
        fi
        runtime_current=$(cat "$runtime_cgroup/memory.current")
        runtime_peak=null
        if [ -r "$runtime_cgroup/memory.peak" ]; then runtime_peak=$(cat "$runtime_cgroup/memory.peak"); fi
        case "$runtime_current" in ''|*[!0-9]*) exit 1 ;; esac
        case "$runtime_peak" in null) ;; ''|*[!0-9]*) exit 1 ;; esac
        # Discard a sample if the container restarted while it was being read.
        runtime_after=$(/usr/bin/docker inspect --format '{{.State.Running}} {{.State.Pid}} {{.HostConfig.Memory}}' glimlink-runtime 2>/dev/null || true)
        if [ "$runtime_after" != "$runtime_info" ]; then exit 1; fi
        printf '{"ok":true,"available":true,"running":true,"scope":"runtime_container","current_bytes":%s,"peak_bytes":%s,"limit_bytes":%s,"sampled_at_unix_ms":%s}\n' "$runtime_current" "$runtime_peak" "$runtime_limit" "$runtime_time"
        exit 0 ;;
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
