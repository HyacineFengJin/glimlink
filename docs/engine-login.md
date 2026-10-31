# Native subscription login through the operator

Original login orchestration is MIT. Codex and Antigravity native programs,
their account storage, their subscription terms and their OAuth implementations
remain separate. No browser engine, V8 or replacement OAuth/token exchange is
included in the Runtime.

The human operator uses the same private local/SSH control channel as platform
login. Device URLs and one-time codes are delivered only to that client; they
are not model tools, log events or ordinary job context. Credentials remain in
the dedicated `state/codex` and `state/antigravity` profiles already used by jobs.
Existing native subscription login is checked before starting a new flow.

Codex uses the pinned native app-server's `account/login/start` with
`type=chatgptDeviceCode`, its device verification URL/user code, correlated
`account/login/completed` notifications and `account/login/cancel`. The operator
opens the official page locally. The Runtime starts no callback listener and
never reads or reimplements the native token file. OpenAI's
[headless authentication documentation](https://learn.chatgpt.com/docs/auth)
recommends the native device flow; availability can depend on account/workspace
settings. Browser callback login is not advertised as supported by this remote
adapter.

The pinned Antigravity 1.4.11 ACP `authenticate` explicitly rejects browser
callbacks in SSH mode. Its native interactive CLI, however, provides the
existing remote Google OAuth/manual authorization-code flow. A small Rust PTY
adapter runs that login UI on demand in a private empty workspace, sets SSH mode
and disables browser launching. It selects only the native Google OAuth login
option and extracts only a complete official Google authorization URL. The
human signs in on their own device and enters the callback's one-time code on
the operator client. The client sends that code over the private control pipe;
the native CLI handles PKCE and token exchange. No account password is entered
into Runtime UI.

The parser validates exact official endpoints, unique query keys and the fixed
Antigravity callback. It rejects lookalike hosts, duplicate fields, fragments,
truncated terminal markup and ambiguous redirect parameters. Arbitrary native
terminal text, account identities and errors are discarded, not forwarded.
Only one authorization code can be submitted to each native process. A native
success marker is followed by an independent cached-account probe, without a
model prompt, before reporting authenticated.

One native subscription login is active at a time. The ten-minute expiry is a
local operation deadline; the provider can expire or reject a challenge sooner.
Cancel, expiry, disconnect and errors kill/wait for the native process group.
A one-shot 64 KiB-stack watchdog enforces expiry even if a connected operator
stops polling; it is stopped before reaping the child to avoid PID-reuse races.
Cancellation retains saved native login rather than signing the account out.
A private credential-home lock is shared with model clients, preventing
simultaneous login and model processes from mutating the same home.

Synthetic tests use compiled native ELF fixtures and temporary private homes.
They cover device completion before its response, exact login correlation,
private error redaction, native PTY code submission, independent completion
verification, session reuse, cancellation, lock release and process cleanup.
An explicit ignored test can inspect the real Antigravity executable with an
empty home in a **network-disabled build container**. It generates a URL,
submits no code and never claims that URL generation proves account login.
