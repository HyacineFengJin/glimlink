# Glimlink Operator

An original MIT Tauri 2 + Rust human control console with a static HTML/CSS/JS
frontend. It uses Material 3 / expressive visual principles and a Bento layout,
without a Node frontend build or a JavaScript application framework. Tauri and
its system WebView retain their own licenses. The small application glyph is
original artwork; see `icons/README.md`. Public builds use text marks for provider
identification. Optional third-party logo files stay outside the repository;
see [their local provenance record](frontend/brands/README.md).

This is a **separate Cargo workspace and lockfile**. Tauri, GTK and WebKit do not
enter the headless Runtime dependency graph. The client release memory target is
below 1 GiB, followed by optimization; there is no invented current-RAM value in
the UI. Runtime memory minimization remains a separate, stricter design rule.

The interface groups **Agent engines**, **social platforms**, and **applications
and collaboration**. Codex and Antigravity are engines. GitHub has its own
application section; X belongs to social platforms. The unimplemented Gmail
adapter has no fake login card.
The Settings memory card measures the connected Runtime, not the operator.
Remote Docker deployments read the fixed worker's cgroup current/peak/limit over
SSH every five seconds without a resident monitor or an extra container. Local
native deployments sample the verified Runtime PID and native-engine children
(RSS/PSS) every three seconds. Stopped and unavailable are distinct states;
failures never fall back to operator memory. Hidden pages stop scheduling, and
only 24 samples stay in memory. Language/theme controls live only in Settings.
Select menus use small keyboard-accessible rounded lists rather than native
WebKit/GTK popups. The main window uses original themed chrome with native mouse
dragging, double-click maximize/restore, and close controls. Unsupported minimize
is hidden on Niri; no Runtime IPC is involved in window management.

Sign-in QR/CAPTCHA pages now open in the operator's system browser by default;
provider OAuth pages already do so. Completion still arrives through the private
local/SSH login driver. Existing sessions are reused first. A running worker can
return `state_busy` before any page opens; the UI explains this and provides an
explicit stop-and-retry button for an idle worker. It refuses to silently stop a
running model turn. The worker remains stopped after login until restarted in
Control. GitHub opens its token settings and still requires private token input;
X retains explicit private Cookie import. Opening a provider page alone is not
successful Runtime authentication.

## Build

On Linux the operator uses system WebKitGTK 4.1 and GTK3. The repository's
optional development image provides their headers, Xvfb and a private D-Bus for
isolated rendering tests. No host desktop, credential directory, D-Bus or browser
profile needs to be mounted for a synthetic test.

```sh
docker build --build-arg OPERATOR_UID="$(id -u)" \
  -t agent-runtime-operator-webview:dev -f scripts/Dockerfile.operator-webview .
bash scripts/isolate.sh build operator-webview -- sh /workspace/operator-desktop/build-container.sh
bash scripts/isolate.sh build operator-webview -- sh /workspace/operator-desktop/build-container.sh test --locked --offline -j 4
bash scripts/isolate.sh build operator-webview -- sh /workspace/scripts/build-login-window.sh
```

The release executable is installed beside `agent-runtime` and the optional
`agent-login-window` in the development cache's `bin` directory. Build/install
the headless core and optional verification window independently. Alternatively,
use `cargo build --manifest-path operator-desktop/Cargo.toml --release --locked`
on a workstation with the system development libraries installed. Static assets
are embedded through Tauri's custom protocol; no development HTTP server is
needed in a release.

Linux is the currently validated desktop target. Other Tauri targets and native
verification-window backends have not been accepted. The desktop uses three
fixed color themes and two density settings, an expressive headline, strong
filled actions and asymmetric Bento surfaces. Original layout CSS/decorative SVG and short state
transitions provide the visuals; no animation loop, remote font, asset CDN or
frontend framework is involved. Reduced-motion preferences are respected.
For a private workstation with the separately sourced provider artwork present,
pass `env GLIMLINK_OPERATOR_LOCAL_BRANDS=1` before `sh` in the isolated build
command. This opts into existing local assets without downloading, recoloring or
publishing them. Their provenance and provider rights remain separate from MIT
code; the public build still works with text marks when assets are absent.

The independent [dependency inventory](dependency-licenses.json) retains the
license metadata of 398 external Cargo packages, including build/target-specific
packages. It does not relicense those packages or include the system WebKit/GTK
license texts; it is not a final binary SBOM.

## Isolated frontend acceptance

```sh
bash scripts/isolate.sh client-test operator-webview -- dbus-run-session -- \
  xvfb-run -a python3 /workspace/scripts/validate-operator-frontend.py
```

This uses the real optional system-WebKit window with a synthetic local bridge,
not real accounts. The native packaged desktop and real IPC have a separate
fixture in `scripts/validate-operator-desktop.py`; neither mounts account state.

## Login behavior

The interface calls bounded public operator APIs. Status/capability checks are
serialized because a Runtime state directory has an exclusive lock. A single
background login worker reuses the same semantic login driver as `agentctl`;
it does not reproduce platform APIs or transport a credential via argv.

The GUI shows unknown, saved, pending, verification-required and failed states
without inventing an online result. Already-saved credentials are verified before
new login. Input is requested only when the backend asks for it. Passwords, tokens
and one-time codes stay in memory and the private control channel; frontend
fields are cleared on submission and termination. The input queue holds one
bounded value. Session-file import, when requested, accepts only an explicit
local private regular file through the existing operator validation.

QR/CAPTCHA presentation requires the adjacent client-only `agent-login-window`
helper. It fails explicitly if the helper or graphical display is unavailable,
without printing a private verification URL. It is separate from the Tauri main WebView, so remote
verification HTML has no application IPC bridge. OAuth opens the provider in
the system browser, including Google's subscription authorization; full URLs,
Cookie values and proof data do not enter normal UI events or activity history.
Closing the application or cancelling a flow signals the shared login driver,
which cleans up its private control connection and native helper.

### X web login

The primary X action uses `auto` to verify and reuse the Runtime's saved session.
An explicit **更新网页登录态** action uses `browser` to request a new private
Cookie input. A rejected saved session does not silently start a fresh login or
discard the old credentials; the shared Runtime driver owns identity checks and
the final result.

When the Runtime asks for Cookies, **在系统浏览器打开 X** is the first option.
It sends the fixed `https://x.com/i/flow/login` URL to the system opener, after
checking the current X login ID and requested input type. Users return to the
operator and paste a complete Cookie request header once; separate `auth_token`
and `ct0` fields remain available. The external browser has no automatic callback
or Cookie capture. The operator does not open its profile files, request an
extension, or send credentials through chat. A reused external browser process
runs on the operator computer and is excluded from the Runtime memory card.

The optional built-in X window uses the existing system WebView in a separate
incognito context. It accepts top-level navigation only to exact HTTPS `x.com`,
blocks popups/downloads/permissions, and has no management IPC capability.
Google and other external sign-ins are not embedded; use the system browser and
private import for those. After a normal `/` or `/home` landing, a background
Rust thread reads `cookies_for_url(https://x.com/)`, extracts only the Secure
root-path `auth_token` and `ct0` cookies for `x.com`, and passes them directly to
the current private input sender. Login/challenge routes cannot trigger this
handoff. No Cookie crosses a Tauri response, event, activity log or localStorage.
Runtime must still verify the submitted session before the UI shows a login.

One native login window may exist at a time. It is bound to the exact pending
input, expires after five minutes, and closes when cancelled, submitted manually,
completed, timed out, or when the main application exits. Its ephemeral browser
data is cleared and the window destroyed. All management commands additionally
require the packaged `main` WebView and local document origin; login events are
sent only to that window. The pinned Tauri 2.12.5 source documents HttpOnly/Secure
Cookie access and the need to read Cookies off the UI thread; capability rules
follow the [Tauri security documentation](https://v2.tauri.app/security/capabilities/).

The native acceptance executable is compiled only with an explicit test feature:

```sh
bash scripts/isolate.sh build operator-webview -- sh /workspace/operator-desktop/build-container.sh \
  build --features x-login-fixture --bin x-login-fixture --locked --offline -j 4
bash scripts/isolate.sh client-test operator-webview -- dbus-run-session -- \
  xvfb-run -a python3 /workspace/scripts/validate-x-login-window.py
```

This fixture uses loopback HTML, Rust-injected synthetic HttpOnly/Secure Cookies,
fresh temporary client directories and a **stub system browser opener**. It never
requests real X or mounts account profiles. Its sub-1-GiB measurements validate
only that synthetic application process tree/container; they do not establish
the peak memory or login compatibility of a real X page or an external browser.
Real X WebView login and its page memory peak remain unaccepted until separately
tested with an authorized real account.

SSH is deliberately noninteractive in the GUI: use configured keys/ssh-agent and
existing known_hosts, and confirm a new host in a terminal first. The GUI does
not disable host-key verification, offer an SSH-password dialogue or pass SSH
credentials through the Runtime protocol. Local mode controls the adjacent core
executable; an explicit Runtime path and state directory can be configured.
An explicit desktop launcher may prefill connection metadata with `--ssh HOST
--runtime /opt/glimlink/runtime --state /state --account main`. These arguments
do not contain credentials, persist account data or start a login. Connections
may read cached human-only account labels over SSH, even while the worker runs.
The same main-window IPC origin checks protect these read-only requests.

Personal WeChat testing remains paused, including in this UI. QQ's current live
password result remains `140022017 / SEC_BEAT`, with cause unconfirmed. A new
window does not itself fix native-protocol compatibility or establish device
trust. Existing real Bilibili/Tieba/Codex/Antigravity login reuse and new OAuth
synthetic tests remain separate validation scopes.

## Security surface

Only packaged local frontend content can navigate in the main window. The CSP
allows local assets and Tauri IPC, with no remote frames, fonts or script CDN.
Capabilities grant only event listen/unlisten. There are no generic shell,
filesystem, HTTP or opener plugins; the registered commands are specific to
connection inspection, login and typed conversation/profile/command management. Raw native/transport errors
are replaced by public messages. No model-facing tool exposes these controls.

## Control pages

Connections, Control/conversations, Commands and Settings are separate pages with
English and Simplified Chinese UI. Messages are queued through the shared Rust
management API; model/profile/activity edits apply to future turns. Conversation
management remains available while the worker owns the kernel. The Docker host
wrapper provides typed worker start/stop/status without giving Runtime a Docker
socket. See [the management boundary](../docs/control-console.md).

Only fixed language/theme/density values persist in browser storage. Cached
provider account names stay in private Runtime state, separate from credentials
and model context. The public build retains text marks; private workstation
builds may opt into locally sourced provider artwork as documented above.

## Chat input and delivery

Enter sends; Shift+Enter inserts a newline. Composition, key code 229, and the
commit Enter immediately after composition-end are excluded from submission.
A local outgoing bubble appears and the composer clears before any SSH request
settles. New drafts remain independent of acknowledgements, retries and other
conversations. Uncertain delivery keeps the original message and operation ID;
manual retry reuses that ID, and canonical history reconciles a lost response.
Only bounded in-memory drafts/outgoing messages are retained, never localStorage.
Unchanged message nodes stay attached across polls, and visual polling updates
are deferred while the IME is composing.

On native Niri/Wayland, an inherited `GTK_IM_MODULE=fcitx` is replaced for this
operator process with GTK's `wayland` input module, before GTK initialization.
This lets the compositor position the candidate popup; Fcitx documents GTK3's
client-side show/hide limitation in its [Wayland guide](https://fcitx-im.org/wiki/Using_Fcitx_5_on_Wayland#Popup_candidate_window).
Other applications and desktop settings are unchanged. Set
`GLIMLINK_OPERATOR_IM_MODULE=inherit` to preserve the inherited module, or choose
`wayland`, `fcitx`, `ibus`, `xim` or `simple` explicitly for this operator.
