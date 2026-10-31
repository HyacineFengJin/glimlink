# Public validation summary

This summary contains no account identifiers, messages, cookies, tickets,
QR codes or private observations. Historical real-account reports and local
experiments are excluded from the source-release candidate list.

The project targets a 512 MiB VPS. Development compilation has no memory cap;
runtime acceptance uses Docker with a 384 MiB memory/swap limit, two CPUs,
read-only root filesystem and no host networking. Network-disabled protocol
tests check local behavior; platform acceptance uses explicit online runs.

The 2026-10-31 Docker workspace run completed **590 tests passed, 0 failed,
5 ignored**, across 63 suites. Standalone upstream/vendor suites are separate
from this count.

The Glimlink publication check repeated that workspace run using only the public
source manifest, without account mounts or network access. All 590 tests passed;
the separate desktop passed 9 Rust tests and 24 frontend cases. Gitleaks scanned
the reviewed source snapshot with zero findings after synthetic QR fixtures were
normalized. Root live-account acceptance harnesses, account state, experimental
work, optional brand image files and native binaries are excluded. These source
checks do not change the platform acceptance boundaries below.

## Login control acceptance

The local/SSH control protocol now owns complete login flows: saved-session
verification, private input, opaque login IDs, polling, human verification,
cancellation and expiry. Real Docker runs through `agentctl` and a separate core
process reused Bilibili, Tieba, Codex and Antigravity logins successfully; all
four returned `authenticated` with `reused_session=true`, with no messages sent.
New engine OAuth uses synthetic end-to-end fixtures; existing real subscription
reuse is tested separately from a new user authorization. WeChat remains paused.

The final optimized operator/core release also passed an empty-account status
smoke in network-disabled Docker: the client and separate core together reached
7,516,160 bytes (~7.2 MiB) cgroup memory peak. This covers status inspection,
not native OAuth/model processes, active QQ signing or platform traffic. The
verification-widget code is present in the operator binary and absent from the
core binary; the core is built without the human-ui feature.

## Optional operator desktop acceptance

The separately locked Tauri + Rust desktop built in optimized release mode and
passed nine Rust tests, including bounded memory parsers and the async command
with a real child forked from a background thread. Its frontend passed 24 DOM cases
in the real optional system-WebKit window,
covering login/event ownership, private-input clearing, serialized inspection,
paused WeChat, connection edits, fixed appearance enums, persistence and keyboard
behavior, role-based grouping, bounded memory history, partial/unavailable samples,
pause and sampling recovery. Three expressive color themes and two density
settings use local CSS/SVG, without runtime asset downloads or a frontend framework.
The public source uses provider text marks; optional third-party logo files stay local.
The memory panel shows live local operator-tree measurements, not design budgets
or remote VPS statistics.

X login now offers a fixed-URL system-browser action first and an optional
separate incognito system-WebView. Manual input accepts one pasted Cookie header
or the two required fields; only those fields travel through the private local/
SSH pipe. The system browser has no automatic Cookie callback. A separate native
fixture verified the real Tauri command and a stub browser opener, HttpOnly/Secure
Cookie capture into the Rust private sender, rejection of remote management IPC,
popup/navigation restrictions, fresh-context isolation, duplicate/stale input
rejection, and cancellation/timeout/manual-submission closure. Cookies on login
or challenge routes did not trigger handoff before the normal landing page.
That fixture used loopback HTML and synthetic cookies only, no accounts or X
requests, under a 1 GiB client ceiling. It reached approximately 494 MiB peak
process-tree PSS and 377 MiB container peak without OOM; summed RSS was about
806 MiB and can count shared mappings repeatedly. These figures do not establish
real X page memory or WebView login acceptance. An independently running system
browser can also fall outside the operator process-tree memory panel.

The release was exercised in Docker with a private Xvfb display and D-Bus,
network disabled, fresh synthetic state and **no account-state mount**. Actual
mouse input invoked the packaged frontend's Tauri IPC, which started the real
Runtime and inspected account status. All eligible empty accounts remained
signed out; no login or model turn was initiated. Window closure exited cleanly.

With a **1 GiB client container limit**, the final 1180×1220 grouped desktop
fixture measured approximately 421 MiB peak PSS for the idle application tree,
541 MiB peak PSS during status inspection including native Codex/Antigravity
probes, and 656 MiB whole-container peak, with no OOM kills. Summed process-tree
RSS peaked around 655 MiB and can double-count shared mappings. The preceding
1180×1160 grouped fixture reached approximately 822 MiB cgroup peak without OOM;
the smaller final cgroup figure is not a claim of a matching memory optimization. Container accounting and PSS cover different
resources and depend on shared-file/cache charging. These measurements are for
this Linux/WebKitGTK/Xvfb fixture; active remote CAPTCHA/OAuth and a complete
cross-platform distribution remain separate acceptance scopes.

The optional verification helper also passed real system-WebKit HTML and script
execution against a synthetic loopback page, owner-EOF closure, expiry, explicit
missing-display failure and rejection of embedded remote OAuth. No platform
requests or account state were used. This is a client-only WebView, absent from
the Runtime's dependency graph and loaded libraries.

An optimized empty-account Runtime smoke measured **9,326,592 bytes (~8.9 MiB)
peak RSS**, below its initial 32 MiB startup/status regression gate. Loaded maps
contained no GUI/browser libraries. This is not the budget for media, native
model-engine children or the full 512 MiB VPS. See [memory policy](memory-policy.md).

## Verified boundaries

- X now has a Rust web-session SDK and private local/SSH Cookie import, integrated
  into the existing 32 semantic tools. SDK tests cover current web shapes,
  identity selection, cursor/pin/media behavior and an independently calculated
  transaction golden. A separate optional test parsed the current real public
  homepage/scripts offline and located all four read operations, without accounts
  or JavaScript execution. Frontend Cookie-input acceptance passed 20 cases, and
  control/operator tests passed 40. A separate online Docker run verified the
  user's privately imported existing session, bound its returned stable identity
  and saved the session. Five shared Runtime tool calls read the current profile,
  own publications, a public
  creator profile, three creator posts and an exact post through its returned
  target reference. Text, author, likes, replies, reposts and view counts were
  returned; no social writes occurred. The import process reached 27,648 KiB
  peak RSS, which does not establish a full media/model/VPS memory budget.
  A subsequent expansion initially returned the adapter's coarse
  `verification_required`. Bounded diagnostics identified an HTML HTTP 403 on
  a redundant user-ID profile lookup, without a provider verification code;
  bare 403 now reports `request_denied` rather than asserting an account
  challenge. Returned opaque targets already bind a verified creator identity,
  so creator expansion now skips that repeated profile/source-post request.
  A fresh online Docker run then read two real pages of 20 posts each through
  a returned continuation, with zero cross-page duplicates, and lazily fetched
  and decoded one photo to 640×399 via `media.read`. All three semantic calls
  succeeded using the existing session, without login or social writes. The
  Runtime process reached 23,452 KiB peak RSS for this read/decode workload.
  Prepared image input is verified; no model observation or visual
  interpretation is claimed by that fixture. The earlier three-item page was
  explicitly truncated and withheld its unsafe cursor; it was not reported as
  provider end. Smaller overfull pages still require a larger bounded request;
  they do not yet preserve pending posts for lossless continuation.
  The modern timeline is original posts; the authenticated legacy path reports
  the broader `posts` kind. Platform writes and automatic fresh-device login
  remain disabled.

- Rust media tests cover actual image/GIF handling, H.264 window sampling,
  AMR encoding/decoding, SILK decoding and window bounds. New original AMR
  fixtures and their independent oracle provenance are in
  [amr-original](../crates/runtime-media/tests/vectors/amr-original/README.md).
- Native Codex and Antigravity subscription sessions have both completed real
  Runtime image-reading and tool-driven speech-transcription turns. They share
  persona/memory and run serially. A combined speech run reached the 384 MiB
  cgroup limit without OOM; this is not evidence of spare memory headroom.
- Bilibili video metadata, creator content, discussions, own publications,
  notifications, favorites and danmaku have real read-path acceptance.
  Sampling is reported as sampled coverage, never complete watching.
- Tieba thread/discussion/image, own-publication and notification workflows
  have read-path acceptance; new search returned actual paginated results.
- New Bilibili/Tieba/GitHub write workflows have protocol/Outbox coverage.
  That does not establish real-account acceptance for every new write action.
- QQ's Rust directory/history/rich-message and four attachment paths have
  protocol coverage. Existing session reuse is currently rejected, and the
  new password/CAPTCHA flow is not yet accepted by the live server. The operator client completed a real human CAPTCHA and submitted proof on
  the held native TCP; the provider rejected the final login with
  `140022017 / LOGIN_ERROR_SEC_BEAT`. Its precise cause is unconfirmed.
  Session-ID and connection fixes alone do not establish login acceptance. The pending four self-attachment sends have not been reported as sent.
- WeChat real-account testing is paused. The default build excludes its live
  client; login, polling and account requests remain disabled.
- GitHub public repository/PR reads work; new authenticated write acceptance
  remains separate from protocol tests.

## Reproduction

```sh
bash scripts/isolate.sh build -- sh /workspace/scripts/cargo.sh test \
  --locked --offline --workspace --exclude platform-weixin \
  --features agent-host-tests -j4 --quiet
python3 scripts/source-release-audit.py

# Optional desktop fixture: 1 GiB client budget, no account mount or network.
bash scripts/isolate.sh client-test operator-webview -- dbus-run-session -- \
  xvfb-run -a -s '-screen 0 1180x1220x24' python3 \
  /workspace/scripts/validate-operator-desktop.py /cache/bin/agent-operator-desktop \
  --window-size 1180 1220 --inspect-click 1007 602

# Synthetic frontend flow assertions in the real optional WebKit window.
bash scripts/isolate.sh client-test operator-webview -- dbus-run-session -- \
  xvfb-run -a python3 /workspace/scripts/validate-operator-frontend.py
```

The source-release audit checks candidate paths and origin policy. It does not
read private files, publish anything, clear unresolved license questions, or
replace a final review of public source contents. See
[source and license boundaries](OPEN_SOURCE.md).
