# Linux VPS deployment (Alpha)

Supported release target: x86_64 GNU/Linux, Debian 12 or newer (also Ubuntu
24.04). No browser, Docker, Node, Python or Rust compiler is required on the VPS.
The optional desktop is separate; this release contains the Runtime and terminal
operator. QQ live login is still blocked; Weixin is paused. Start with Bilibili.
For a container-isolated host, use [the Docker layout](deployment-docker.md).

## Install

Download the Linux archive and `SHA256SUMS` from the same Glimlink GitHub release.
Verify its checksum, extract it, enter the extracted directory, then run:

```sh
release_url=https://github.com/HyacineFengJin/glimlink/releases/download/v0.1.0-alpha.2
curl -fLO "$release_url/glimlink-0.1.0-alpha.2-linux-x86_64.tar.gz"
curl -fLO "$release_url/SHA256SUMS"
sha256sum --ignore-missing -c SHA256SUMS
tar -xzf glimlink-0.1.0-alpha.2-linux-x86_64.tar.gz
cd glimlink-0.1.0-alpha.2-linux-x86_64
```

```sh
sh install.sh
sh install-engines.sh codex
# Optional second subscription engine:
sh install-engines.sh antigravity
export PATH="$HOME/.local/bin:$PATH"
agent-runtime doctor
```

The explicit engine installers use pinned upstream native releases and SHA256,
not npm launchers. They do not start login. Required installation tools are
`curl`, `tar`, `sha256sum` and standard Unix utilities. On Debian: install
`ca-certificates curl` if absent. The Runtime, agents and speech model need about
600 MiB of disk together; live media cache uses additional bounded disk space.
If the VPS cannot download GitHub assets, download the exact pinned upstream
archive on your computer, transfer it privately and pass its path as the third
installer argument. The same checksum is required. Staging uses the destination
disk rather than a RAM-backed `/tmp`.

## Login from your computer

Copy `bin/agentctl` from the same Linux archive to the local operator computer.
Use the remote user's **absolute** Runtime/state paths:

```sh
./agentctl --ssh user@vps --runtime /home/user/.local/bin/agent-runtime \
  --state /home/user/.local/state/glimlink --account main codex login
./agentctl --ssh user@vps --runtime /home/user/.local/bin/agent-runtime \
  --state /home/user/.local/state/glimlink --account bili-main bilibili login
```

Replace `user@vps` and both `/home/user` paths. QR and authorization pages appear
on your computer. Credentials stay in the VPS's private state. For Antigravity,
use `--account main antigravity login`. Both engines preserve subscription login;
no API key is required by this integration. See [engine login](engine-login.md).

The current operator transport starts a process that owns the state lock. Before
logging in again or running a standalone MCP server on that state, stop
`glimlink.service`; restart it after the other process exits.
It cannot yet attach login/MCP to a running daemon. Basic queue/status commands
remain usable while the daemon runs.

## Set up autonomous reading

On the VPS, edit the files under `~/.local/state/glimlink/config` first. The starter
profile is Bilibili-only, read-only, at most 24 autonomous wakes per rolling day,
with at least ten minutes between wakes. It permits private memory updates.

```sh
state="$HOME/.local/state/glimlink"
agent-runtime --state "$state" mind persona main "$state/config/persona.txt" 0
agent-runtime --state "$state" mind role main "$state/config/role.txt" 0
agent-runtime --state "$state" agent profile-set main "$state/config/profile-codex.json"
agent-runtime --state "$state" agent activity-set "$state/config/browsing.json"
# Optional local Rust speech transcription (explicit 42 MiB download):
agent-runtime --state "$state" speech install
agent-runtime --state "$state" agent run --once --timeout-seconds 360
```

The two `0` revisions are for a new persona/role only; existing state must use
its actual revision, rather than overwriting it. Select `profile-antigravity.json`
instead of `profile-codex.json` to switch engines while retaining persona/memory.
The model runs only when work is due. If it chose to rest, `--once` may have no
due model job; inspect `agent activities` for the next wake.
Choose a model available to your own subscription in the selected profile.

## Keep running

```sh
systemctl --user daemon-reload
systemctl --user enable --now glimlink.service
journalctl --user -u glimlink.service -f
```

An administrator can run `loginctl enable-linger USER` to keep this user service
alive after SSH logout. Use an ordinary SSH user with a systemd user manager;
minimal Debian installations may need `dbus-user-session` and a fresh login.
The supplied unit uses the default `~/.local` installation,
`MemoryHigh=384M`, `MemoryMax=512M`, no swap and one model at a time. Verify the
limits are available in your VPS's user cgroup. These are enforced limits, not
a promise that every workload fits: one measured comment/ASR case hit 512 MiB
without OOM. A 2 GiB VPS still needs room for its OS and your other services.

Failures/expired login/interrupted autonomous jobs stop that activity; inspect
`agent status` / `agent activities`, then explicitly `agent activity-wake browsing`
when appropriate. Unknown public writes are not automatically repeated.
No public writes are enabled by the starter configuration.

## Update, backup and remove

Stop the service, back up the private state directory with mode 0700, verify and
install the new archive, then start the service. Keep the previous archive for
binary rollback; pair it with a matching state backup when formats change.
The installer preserves your state/config and does not start or enable services.
Backups contain credentials: transfer them privately and never publish them.
Native CLI transcript/cache retention is not yet managed by Glimlink's bounded
job/media cleanup. Monitor state-directory disk use during longer Alpha runs;
do not delete a native credential home as a cache-cleanup shortcut.
Remove the user service and installed binaries to uninstall; remove state only
if you deliberately want to delete accounts and memory.

Original files remain MIT. The combined Runtime includes retained AGPL/GPL and
other third-party code; it is not MIT-only. Each binary release must be accompanied
by its corresponding source asset, including the vendored registry dependencies
and retained notices. Native engines, signers, downloaded models and account data
are not included in Glimlink's archives.
