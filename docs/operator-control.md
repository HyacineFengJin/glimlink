<!-- SPDX-License-Identifier: MIT -->
# Operator login: local client, local or SSH core

`agentctl PLATFORM login` is the human login entry point. The same bounded,
versioned JSONL protocol travels through a local child process or system OpenSSH
stdio. The core owns credentials and authentication state; only the client reads
human input, displays QR codes, opens authorization links or serves a temporary
human-verification page. These controls are separate from model-facing MCP tools.

```sh
agentctl --state /path/to/private-state --account main bili login
agentctl --state /path/to/private-state --account main tieba login --method qr
agentctl --state /path/to/private-state --account main qq login --method password
agentctl --state /path/to/private-state --account main qq login --method password --secret-file /private/qq-password.json
agentctl --state /path/to/private-state --account main github login --secret-file /private/github-token.txt
agentctl --state /path/to/private-state --account main codex login --method device
agentctl --state /path/to/private-state --account main antigravity login --method browser
agentctl --ssh my-vps --runtime /opt/agent-runtime --state /var/lib/agent-runtime --account main bili login
```

Actual supported methods come from `agentctl PLATFORM capabilities`. An example
above does not grant a backend a capability: an explicitly unsupported method is
rejected before starting login. Personal WeChat login remains paused; its
capabilities report no methods and `paused: true`, and login performs no account
operation. Codex and Antigravity retain subscription login rather than being
converted to API-key-only adapters. Their current backend profiles each have one
subscription home per engine; arbitrary account aliases do not create isolated
multiple Google/OpenAI subscription homes. Capabilities describe this boundary.

The client asks for capabilities, then sends exactly one `login` request. The
core checks its saved login first. It only starts a new flow after an explicit
`signed_out`, `needs_auth` or `expired` result. A network error, rate limit, risk
check or unknown failure stops the flow; the client does not try a different
method or generate a QR code after an arbitrary failure. A working saved login
returns the verified identity without requesting another password or scan.

The supported method names are `auto`, `qr`, `password`, `device` and `browser`.
`auto` delegates selection to that platform's core adapter, within the advertised
capabilities. Existing session import is also explicitly available through
`import-login`; it is not guessed from an unknown platform or input format.

## Private input and continuation

Bilibili, Tieba, X, Pixiv, Reddit and Discord prefer the saved Runtime session. If a new login is
needed, `browser` asks for `browser_session` on the same private control flow.
The operator launches an installed Chrome/Chromium/Edge/Brave with a temporary
0700 non-default profile and an inherited debugging pipe. No debugging TCP port
is opened. Google/Apple sign-in stays inside that real browser; only cookies
needed by the selected destination platform are retained (Discord uses its own user-session token). Neither the full
profile nor third-party sign-in cookies are transferred.

The browser session travels directly from Rust through local/SSH stdio. It is
absent from Tauri responses/events, AI context and activity logs. Runtime makes
an independent account request and checks the alias binding before saving. A
browser landing or anonymous Cookie is not proof of login. Positive authenticated-page readiness is required before handing off; the owned browser stays open until Runtime verification ends. Cancel/expiry closes the
owned browser and removes its temporary profile; existing Runtime credentials
remain. This path requires an installed supported browser on the operator,
not on the VPS. Bilibili/Tieba retain explicit `--method qr`; private session
files remain available via `--secret-file` or `import-login`. Native QQ and
personal WeChat login are unchanged.

Source input files stay on the operator's computer. A `--secret-file` must be a
regular private file (`0600`), not a symlink, and at most 256 KiB. A platform may
impose a smaller limit. The client sends its contents through the private pipe,
or SSH's encrypted stdin; secrets are never process arguments. The core may
adapt input to an existing management API using one random temporary `0600`
file, removed on success and error. Interrupted files live under the Runtime's
cleared work directory rather than its persistent account namespace.

For an initial QQ password the private JSON shape is
`{"uin":12345,"password":"…"}`, optionally with the explicitly selected
`app_profile`. With no file, the client asks for the UIN and a hidden password on
`/dev/tty`, constructing the JSON in memory. It never reads a password from the
client's ordinary stdin. GitHub tokens and OAuth authorization codes also use a
private file or hidden terminal prompt. Empty hidden input cancels. A session
bundle requires a private file. No interactive value is saved to a client file.

When input is needed, even before the first password request, the core creates
an opaque `login_id`. All subsequent input, polling and cancellation uses that
same ID, bound to the platform, account alias and current control connection.
The client does not reopen the Runtime or perform a second live-login probe.

For QQ CAPTCHA, the core retains its exchange continuation and native connection.
The client receives only the challenge ID, expiry and official challenge URL.
It opens a random local page in the human's external browser; the human completes
Tencent's official component. Widget application ID and native session ID remain
separate: the widget uses URL `aid`; native resume uses the original, still
encoded URL `sid` as `session_id`. The next `login_poll` contains only
`{"captcha":{"ticket":"…","randstr":"…","session_id":"…"}}` in its private
`secret` field. It does not retransmit the original password. The core validates
the current challenge before submission. No model tool sees a proof or password.

For subscription OAuth, the client displays `authorization` events locally and
opens their HTTPS link in the human's external browser. Device codes appear only
in the operator's terminal. Where the provider requires a one-time authorization
code, `login_poll.secret` contains `{"authorization_code":"…"}`; a private input
file may hold that JSON or just the plain code. Provider polling is asynchronous,
uses the same flow ID and respects the core's `poll_after_ms`. There is no VPS
browser UI or implicit remote browser launch. Only methods actually implemented
by that backend may appear in capabilities.

`qr` and `password-login` are convenience aliases for the unified driver.
Verification completes inside `qq login`; a separate `password-resume` command
cannot attach to the former process's native connection and is no longer needed.
The wire protocol retains older management actions for existing callers.

## Human UI and stopping a flow

Bilibili's QR URL is encoded on the client. QQ and Tieba images supplied by their
services are forwarded as bounded inline data. No core filesystem path is needed
by the client. QR and CAPTCHA pages normally bind only `127.0.0.1:0`, with a
random page path, exact Host/Origin checks and bounded requests. `--ui auto`
tries an adjacent optional system-WebKitGTK helper for these local pages when a
graphical display is available, with an explicit browser fallback on startup
failure. `--ui browser` selects the system browser; `--ui webview` requires the
helper and stops on failure without silently falling back. HTTPS provider OAuth
always uses the system browser. The headless client and Runtime embed no browser
engine, V8, Blink or Node; the separately installed GUI helper belongs only to
the human operator machine.

The local pages include a cancel button and send a cancellation when closed.
`Ctrl-C`, page cancellation and an expired deadline send `cancel_login` for the
active flow and close the local page. `--timeout-secs` bounds the overall login
(default 900; allowed 1–3600). Server or challenge expiry can shorten that bound.
A stalled stdio response is also bounded. External provider pages do not expose
reliable browser-tab-close notifications; use `Ctrl-C` to cancel those flows.

Exit codes are 0 for authenticated, 1 for failure, 124 for deadline expiry and 130
for cancellation. A pending challenge is not reported as authenticated. Terminal
JSON omits authorization URLs, device/authorization codes, tokens, cookies,
passwords, proofs and inline QR data. Necessary links and codes appear only in
the human operator's stderr/UI.

`--no-open` leaves browser opening to the operator and prints the local page or
authorization link. `--no-ui` disables local pages and hidden terminal prompts;
a private file can still supply input. When required human input is unavailable,
the client stops and cancels the flow rather than claiming success.

For isolated **client** UI testing only, `scripts/isolate.sh operator-ui` can
publish host loopback port 18799 to the client container. The UI accepts this
one exact test configuration only: `AGENTCTL_TEST_UI=1`,
`AGENTCTL_UI_LISTEN_ADDR=0.0.0.0:18799`,
`AGENTCTL_UI_ORIGIN=http://127.0.0.1:18799`. Other overrides, ports, origins, or
an override without the test flag are rejected. Host/Origin and random-path
checks remain active. This is not a core/VPS listener or a deployment setting.

## Transport, ownership and protocol

Local use starts the adjacent `agent-runtime`; `--runtime` selects an executable.
Remote use executes a fixed `operator serve` command over the system `ssh`, with
strictly quoted data arguments and the operator's normal SSH authentication and
host-key policy. It installs nothing remotely and disables no host checks. No
VPS HTTP port or browser forwarding is needed. The spawn-based operator core
cannot share the same state directory with a different process already holding
its `runtime.lock`; this transport does not attach to an existing daemon.

Protocol version 1 is one JSON object per newline, at most 1 MiB including the
newline. A request contains `version`, positive `id`, `platform`, explicit
`account`, `action`, object `params`, and optional base64 `secret`. Base64 is
framing, not encryption; only trusted pipes or authenticated SSH may carry it.
Replies are `response` or `event`, with the matching version and request ID.
Malformed/oversized frames, unknown request fields and unsupported parameters
are rejected. There is no arbitrary method, shell command, callback URL or
server-side input-file path. No raw native error string is exposed.

The common actions are:

- `capabilities`: supported methods, saved-login reuse, import support and pause status.
- `login`: `params` may contain `method` and QQ `app_profile`; private initial input is optional.
- `login_poll`: `params.login_id` is required; `challenge_id` may pin the CAPTCHA. Proofs/codes only belong in `secret`.
- `cancel_login`: requires the current `login_id` and accepts no secret.

The typed common result uses `authenticated`, `pending`,
`verification_required`, `failed` or `cancelled`, with optional identity,
`login_id`, method, reuse flag, expiry, polling interval, authorization data,
QR presentation, challenge, requested input or public error. Authentication
requires a non-public account alias; capabilities/status allow public inspection.

`runtime-control` has no human UI in its default features. `agentctl` and the
optional desktop enable `human-ui`. The operator client depends on serialization and small OS helpers,
not the Runtime host, platform protocol SDKs, signer, model engines or media
libraries. This protocol, client, UI and host glue are original MIT code.
Third-party implementations retain their own licenses; these boundaries do not
relicense a platform dependency.

## Optional desktop

X uses normal saved web-session identity reuse, then explicit private Cookie
import when needed: `agentctl --account x-main x login`. The client-only nonce
page accepts a single pasted Cookie header or minimal `{auth_token,ct0}` JSON;
the two-field form remains available. Unneeded Cookie fields, including `twid`,
are discarded on the operator before transport. No external browser profile
directory is read. Private 0600 files and a single hidden terminal input remain
alternatives; auto mode reads X input only after reuse actually requires new
credentials. Cookies never become model tool parameters.

`agentctl --account x-main x login --method browser` explicitly requests private
session refresh without first checking the old session. This is a human-selected
action; auto mode does not reinterpret a 403/rate limit as session expiry. Old
saved state and its stable identity binding are retained until successful
verification of the replacement. The desktop offers a fixed-URL system-browser
entry first, with a two-value import guide on return; ordinary navigation cannot
transfer that independent browser's HttpOnly cookies to the operator. Its
optional separate client-only X login window can instead forward the two
required cookies directly from Rust through the same local/SSH pipe. It neither embeds a browser into the
Runtime nor transfers Cookie values through frontend events. Normal X WebView
login requires its own real-platform acceptance; synthetic Cookie tests do not
establish that X will accept a particular device or network.

X semantic read failures include bounded `last_request` metadata: a fixed stage,
HTTP status, response kind and at most eight numeric provider codes. They contain
no URL, headers, body or Cookie. Bare HTTP 403 maps to `request_denied`; explicit
account-access redirects/provider verification codes map to
`verification_required`. The diagnostic describes the last HTTP request, not a
guessed account restriction or root cause. It does not enable automatic retries.

The Runtime verifies the current authenticated account before committing
`session.json`; identity bindings prevent replacing another person under the
same alias. An unverified candidate is private, is never counted as a login, and
can be explicitly retried for 15 minutes with `agent-runtime --account x-main x
resume-import` after a protocol fix. Success or a confirmed authentication
rejection removes it; an expired explicit retry removes the stale candidate.
No retry timer or account rotation runs in the background.

The independently built [Tauri + Rust desktop](../operator-desktop/README.md)
reuses `runtime-operator`'s login driver and bounded inspection API. Its packaged
static frontend has no Node build or remote assets. Only fixed language/theme/density
choices persist; account, connection and private input stay session-local.
SSH management requires existing key/agent authentication and known_hosts,
without opening a terminal or disabling host checks from a GUI worker.

The adjacent `agent-login-window` is an optional Linux system-WebKitGTK helper
for nonce-bound local QR/CAPTCHA pages. A private stdin lifecycle channel owns
the ephemeral window; owner EOF, cancellation and expiry close it. Remote
verification frames have no application IPC bridge. Google subscription OAuth
stays in the system browser, following the provider's embedded-user-agent policy.
The helper's default feature set has no GUI, and the desktop's separate Cargo
workspace prevents Tauri/Wry/GTK from entering the Runtime's dependency graph.

Synthetic display tests use `client-test operator-webview`: a separate 1 GiB
container limit and no account-state mount. This does not increase the VPS core
budget. The test image's WebKit is development/client infrastructure, not a
Runtime component or a browser bundled with a VPS release.

Conversation, profile, activity and scoped chat-command management use the separate
[control endpoint](control-console.md), without the login endpoint’s kernel lock.


GitHub defaults to the project's registered public Glimlink application and the
provider's device authorization flow; deployment does not require app registration.
`github login --method token` and explicit private files remain optional import
paths. Browser authorization exposes only the verification URL and user code;
device codes, access tokens and refresh tokens stay in the Runtime/control pipe.
The same account alias cannot be rebound to another GitHub identity. Expiring
credentials rotate in one private atomic session file while that Runtime owns
state; no background refresh service is added.
