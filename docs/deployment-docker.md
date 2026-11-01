# Docker VPS deployment

This complements [the native Alpha guide](deployment.md). Docker runs only the
headless Runtime and on-demand native engines on the VPS. The desktop operator,
verification windows and system browser remain on the operator computer.

Install Docker Engine and Compose from Docker's official Debian repository.
Build `scripts/release/docker/Dockerfile` with an extracted, verified Linux
release bundle as the build context. The image contains neither account state
nor native engines. Copy the corresponding-source release asset and notices
with any redistributed image; original MIT and retained third-party licenses
still apply.

The supplied layout is:

- `/opt/glimlink/compose.yaml`: the Compose template.
- `/opt/glimlink/runtime`: an executable copy of `scripts/release/docker/runtime.sh`.
- `/opt/glimlink/engines`: pinned upstream native binaries, installed separately
  with `install-engines.sh` and mounted read-only.
- `/var/lib/glimlink/state`: private persistent state, mode 0700, UID/GID 10001.

The Compose template runs as UID 10001, drops capabilities, sets
`no-new-privileges`, makes the root filesystem read-only, and publishes no ports.
Each container is limited to 512 MiB including native engine children, no swap,
one CPU, 128 processes and a 32 MiB temporary filesystem. Logs rotate at 5 MiB
with two files. Docker/containerd and the operating system consume memory outside
that per-container limit; leave room for them and other services.

On the operator computer, configure an SSH key and verify/pin the server's host
key using normal OpenSSH. The terminal client then uses:

```sh
agentctl --ssh configured-vps --runtime /opt/glimlink/runtime \
  --state /state --account main bilibili login
```

The desktop accepts the same four connection flags. They prefill visible
connection metadata without starting SSH or login. Credentials are never launcher
arguments. If using `main` for platform logins, change each starter profile's
`default_accounts.bilibili` from `bili-main` to `main` as well.

Initialize persona, role and profiles using `/opt/glimlink/runtime --state /state`
followed by the native guide's commands. Files supplied as Runtime arguments must
be under its mounted `/state` directory. Keep autonomous activity disabled until
the selected engine and platform are logged in. Afterwards, start the service:

```sh
cd /opt/glimlink
docker compose up -d runtime
docker compose logs --tail 30 runtime
docker stats --no-stream glimlink-runtime
```

The current Alpha cannot attach an operator or standalone MCP server to the
daemon's state lock. Before using either, `docker compose stop runtime`; restart
with `docker compose up -d runtime` after the control process exits. The transport
wrapper never silently stops an active agent. Only one process should use this
state at a time. Container restart cannot automatically resume an interrupted
autonomous activity; inspect and explicitly wake it when appropriate.

Back up the private host state directory while all Runtime/control containers
are stopped. Compose/image replacement preserves the bind-mounted data. No
Docker socket, host SSH key, browser profile or host desktop belongs inside the
Runtime container.
