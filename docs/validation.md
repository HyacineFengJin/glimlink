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

### Named-model Agent use (2026-10-31)

Docker live tasks explicitly selected `gpt-6.1-terra` and
`gemini-3.8-flash-medium` using the existing native subscription homes. The
models chose Runtime tools from ordinary Chinese task descriptions, without
HTTP/shell tools or manually supplied account aliases. QQ and Weixin were
excluded; no platform write or login was performed.

Both produced Bilibili/Tieba/X/GitHub reading briefs, read an actual comment
image and transcribed a ten-second public speech sample. Terra's brief used
12 successful calls; Flash used 14 calls, recovering from one unsupported
GitHub file reference. Their 384 MiB container peaks were approximately
234 MiB and 337 MiB. New X default-account support was exercised through a
frozen execution profile.

Both also read one fresh, opaque random-color image and four GIF frames, with
all 30 color positions correct for each model. However, Flash's first real
video reply described text and a character absent from the actual frames.
The source-frame hashes matched Terra's input. A repeated Flash video task
described the visuals correctly but overclaimed music/sound effects from an
ASR result containing only an ellipsis. These are retained quality failures;
successful image delivery and a completed model turn do not prove accurate
video/audio interpretation. Terra's two video replies matched the samples
and explicitly withheld environmental-audio claims.

Video and speech tasks completed at the 384 MiB cgroup ceiling with reclaim
events and no OOM. A 512 MiB video repeat reached approximately 425 MiB for
Terra and 499 MiB for Flash, including native children and file cache. This
excludes the host OS and operator desktop, and is not evidence of spare
memory on a 512 MiB machine. Measurements are individual tasks, not a latency
distribution or a long-running deployment acceptance. Raw results and model
answers remain in the private, ignored `.runtime` directory.

### Platform and protocol evidence

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

## Lean speech and autonomous homepage reading (2026-10-31)

The signed-in Bilibili homepage recommendation route now has SDK and Runtime
acceptance, separate from the popular list. Source-preserving cached-tail and
cross-source continuation tests passed. Live Agent tasks supplied no video URL:
Codex 6.1 Terra and Gemini 3.8 Flash Medium each read six returned recommendations,
selected a different candidate, sampled two ten-second windows and produced an
evaluation using the available comments/subtitles. Terra used four tools, Flash
eight; both stayed within the media-window budget and performed no platform write.
Representative source frames were reviewed against their descriptions. Opinions
and subtitles do not establish untested properties of the projects in those videos.

The first compact speech implementation reduced the same ten-second speech
container workload from about 210 MiB to 150 MiB, and a repeated large-image/speech
workload from 321 MiB to 268 MiB. Encoder release, bounded vocabulary expansion
and a later model-file cache hint are distinct optimizations. English and Mandarin
samples retained exact transcripts; numerical tile/row tests compare with the
full f32 reference. The upstream model adaptation retains Candle's dual license.

Further measurements and raw Agent answers remain private under `.runtime`;
512 MiB container results exclude the host OS. A 64 MiB Go experiment produced
one empty final reply after media tools executed; it was not counted as complete
and the default remains 128 MiB. No causal claim about that empty reply is made.
QQ/Weixin were excluded and existing subscription/platform login was reused.

The final standalone speech/container fixture measured about 131 MiB, and the
four-round image/speech fixture about 209 MiB. Completed fixed-video model
comparisons measured about 337 MiB for Terra and 424 MiB for Flash with the
original 128 MiB Go target (before the last allocator-trim refinement), versus
425/499 MiB before the speech changes. All these completed fixtures had no OOM.
Final mixed-workload retained process-tree PSS was about 18 MiB versus 156 MiB.

## Autonomous activity and linked context (2026-11-01)

An enabled activity can wake the daemon with no pending user task. Its internal
turn record has an empty prompt, no observed-event payload, a typed activity
origin and a frozen execution profile. AI chooses browsing, continuation or rest
under standing permissions; a deliverable and checkpoint update are optional.
Ordinary queued work has priority. No additional resident model is introduced.

The context layer supplies bounded current notes and recent episode excerpts.
Older episodes expand by reference and UTF-8 byte range, preserving predecessor,
engine, status, action IDs and the versioned checkpoint snapshot. They survive
queue GC. Memory remains attributed data; it does not grant permissions or prove
an action executed. Archived notes do not contain the full original tool stream.
Platform details can be reopened through retained anchors when still available.

Docker checks passed: 16 queue tests and 41 selected host/profile/memory/activity
integration tests, including six new activity tests. These verify a daemon wake
without a task, explicit-request priority, optional notes, native Codex-to-
Antigravity continuity after queue GC, linked/paged detail and old checkpoint
snapshots, bounded retention, revisions/scope, rest/budget limits, crash halt and
receipt pins. The protocol tests use synthetic accounts with networking disabled.

A separate real online trial reused native subscription login with
`gpt-6.1-terra` then `gemini-3.8-flash-medium`. Only standing interests and Bilibili
read capabilities were supplied; no video, search keyword, operation order or
task objective. Terra independently searched for game/cinematic presentation,
watched 10 seconds/four frames and rested with a short private note. Flash
received that episode, queried the separate popular feed and search, selected
split-screen storytelling, read three eight-second windows/twelve frames and
comments, then saved its interests, questions, anchor and rest interval.
No public reply, like, follow, new login, QQ or Weixin request was made.

Whole-container peak was 490.6 MiB under a 512 MiB/no-swap limit, with zero
limit-pressure events or OOM. After both native engines exited, retained
process-tree PSS was about 23.1 MiB. This variable, richer browsing workload is
not the earlier fixed-video comparison and leaves little space for a whole
512 MiB machine's OS. It is not full-VPS or unattended long-term acceptance.

Representative images support character close-ups, purple lighting and a
split-screen dialogue interface. Flash's exact continuous-time/black-background
wording goes beyond what the sparse frames establish; comment paraphrases and
film analogies were not independently verified in this review. The final short
checkpoint retains the broader subject/questions rather than those exact claims.
Raw model notes are retained as model statements, not promoted to source facts.

The original harness incorrectly required the first turn to save a checkpoint,
although updates are optional. That raw trial retains its original false pass
flag; reviewed acceptance checks the actual episode identity/preview injected
into the second turn and the matching checkpoint revision. Both native turns
completed. The predicate was corrected without repeating model calls. Two
earlier pre-model startup failures (host glibc incompatibility and a measurement
helper's profile-lock lifetime) remain separate failed records.

## Source quality and region rereading (2026-11-01)

The media tool surface now distinguishes source quality from output size.
`quality=preview|480p|720p|1080p` negotiates an AVC source; actual dimensions,
compatible variants and request satisfaction are returned. Source caches bind
the requested quality as well as account/video/part. Frames carry source-linked
references that can inspect the same timestamp with a source-pixel crop. Both
`media_watch action=inspect` and `media_read reference=<frame_ref>` support this.
Inspection does not advance a viewing session; normal quality preferences persist.

PNG is the default for inspections/crops. Detail attachment delivery preserves
the requested prepared geometry, rather than silently fitting it into the old
64 KiB preview allowance. Actual sent dimensions, MIME, byte count and any
preview fitting are separately reported. Larger detail returns an explicit
unavailable/partial result so the caller can choose a tighter crop or JPEG.
Codex tool results and both native JSONL/MCP paths remain bounded within 1 MiB.

150 selected checks passed in isolated 384 MiB containers, without accounts or
network. They cover source selection (including downgraded and portrait tracks),
source-before-resize pixel crops, lossless video-region equality, no upscaling,
account/time binding, non-advancing inspection, quality persistence/cache
separation, PNG MIME and actual model geometry, oversized-detail refusal, and
the affected Codex/Antigravity protocol and MCP paths. QQ/Weixin were excluded.

Real read-only acceptance reused saved Bilibili and native Codex subscription
state. A direct call reread the previously inspected dialogue from a 640x360
preview source at 1920x1080, outputting a 960x324 PNG crop without further
resizing. A separate `gpt-6.1-terra` turn chose its own crop, obtained a 941x281
PNG from the 1920x1080 source, and correctly transcribed the target dialogue on
manual comparison. No expected dialogue text was supplied in its prompt.
Peak whole-container memory was 59.9 MiB for the direct pipeline and 212.1 MiB
for this single-frame Codex task, under 512 MiB/no-swap with no pressure/OOM.
These single-run values exclude the host OS and do not generalize to dense or
longer HD viewing. No fresh login or platform write was made.

The first real HD source exceeded the previous 96 MiB conservative reference-
buffer estimate and was refused before decoding. Ordinary decoding retains that
limit; single-frame/no-audio inspection permits an estimate up to 192 MiB with a
one-second/one-output-frame envelope. The estimate is not RSS; the real workload
above measured the resulting process/cgroup use. Two earlier model probes lacked
the new case's isolated account copy; the model correctly omitted account and
reported that no saved session was available. The measurement setup was fixed;
those attempts remain failures and are not counted as reading acceptance.
Full stream download limits, codec constraints and platform source availability
still apply. A lower actual resolution is explicit and is never upscaled into HD.

Four additional autonomous test wakes alternated `gpt-6.1-terra` and
`gemini-3.8-flash-medium`, twice each, preserving the prior persona/notes. Task
prompts/events were empty; these were operator-triggered test wakes, not a timer
scheduling test. Standing instructions described media capabilities and optional
context-aware participation, with no assigned videos or required HD/crop/action.
An operator-authorized, durable shared ceiling allowed four Bilibili likes and
two short text comments across engines; failure/uncertainty also spent allowance.
Actual public effects: one like verified by readback, one duplicate-like rejection,
no comments. Comment language quality remains unvalidated; participation was optional.

Flash made six source-linked inspections across two wakes, four successfully
attached. It recovered from two oversized PNG refusals using explicit JPEG/output
choices. Observed sources included 1440x1080 and 1280x720; original preview
attachments were as small as 480x270. No spatial crop was chosen. This demonstrates
autonomous use of source rereading, not reliable detection of every small-text
case. Terra did not request HD or crops in these wakes. One Flash wake left no
final note, which is permitted; a final report is not an autonomous completion
requirement. The original strict sampler result is retained alongside a reviewed
lifecycle check; known action rejections and optional final notes are assessed
separately from the four completed native turns.

The 512 MiB/no-swap container peaked at 447.5 MiB (kernel `memory.peak`), with no
OOM/limit event. Per-engine sampled whole-container maxima were 353.6 MiB for
Codex and 440.2 MiB for Antigravity. Final retained process-group PSS was 45.1 MiB;
the retained container including file cache sampled 115.2 MiB. Engines ran serially,
and these figures exclude the host OS and do not establish a concurrent-engine
budget. Private responses, traces, frame provenance, action evidence and samples
remain ignored; derived account copies were removed and original subscription
homes/accounts retained. No login or QQ/Weixin testing occurred.

The trial exposed two interface explanations: duplicate-like code 65006 now
explains the server-reported existing state without changing the rejected ledger
or inventing verification; ordinary image quality requests now report actual
source pixels, `request_met`, and `stream_reacquired:false`, directing video
source upgrades to `frame_ref`. Both fixes have isolated regression coverage;
the real experiment is preserved as run, without another public-action replay.
34 distinct selected checks passed across autonomous lifecycle/allowance,
profiles, Antigravity host, media workflows and engagement evidence.

A separate operator-directed comment trial then ran one native wake per engine,
using the activity slot for its durable shared allowance. This is a focused
participation test, not evidence of unprompted commenting: the operator's role
instructions explicitly requested watching, reading the discussion and making
one relevant short reply when something fit. Following an existing joke/common
phrase was allowed; no video, wording or parent comment was assigned. New likes
and other writes were disabled, and the total allowance was two text comments,
with one additional reservation available per engine's wake.

Both engines watched actual frame samples before sending and read the selected
discussion. Terra made a short agreement about a split-screen reading mechanic;
Flash extended the selected comment's Tetris/Sisyphus joke using a native emote.
Both initial sends returned acknowledged IDs but failed immediate exact readback.
Later read-only thread queries found the exact author/text/root/parent; replaying
the same already-Sent operation identity performed reconciliation without another
POST, and both ledger records became Verified. The second reply also had a later
reply in the readback; no third comment was sent. The two language samples were
reviewed qualitatively, without claiming a general naturalness success rate.

The serial 512 MiB container completed in 208.4 seconds with no OOM, but hit its
512 MiB limit (`memory.events max=253`). Sampled process-group PSS peaked at
418.1 MiB, and file cache was about 198 MiB near the highest sampled residency.
This run therefore demonstrates memory pressure rather than spare VPS headroom;
host OS memory is excluded. The focused trial's bounded normalized comment
excerpts and actual submissions are opt-in measurement data, compiled out of the
ordinary Runtime. Private evidence remains ignored and test account copies were
removed after reconciliation; original accounts/subscription homes were retained.

The subsequent conversation trial switched back to `gpt-6.1-terra`, copying the
same persona, archived activity episodes and verified action records from the
Flash comment trial. Its role requested notification-driven continuation with
at most one additional reply; no notification ID, video, message or target was
supplied. Terra read reply notifications, opened the returned notification_ref,
obtained the exact root/previous own comment/new selected reply, expanded the
prior Antigravity episode and inspected that comment's verified operation. It
judged the other reply to finish the joke and ended without a new public action.
This single case validates context recovery and a decision to stop, not the
quality of an additional generated reply or unattended timer scheduling.

Four fresh serial containers completed with no OOM or memory-limit event:
the conversation trial peaked at 325.1 MiB, an empty Runtime workload at 9.5 MiB,
a 10-second Rust Whisper workload at 117.5 MiB, and a simple Codex/modules call at
342.2 MiB. The sampler now distinguishes anonymous/file PSS per process and
records mapped/dirty file pages. At the simple Codex case's sampled PSS peak,
the native engine contributed 276.4 MiB and the Runtime workload process 7.8 MiB;
these include mapped file pages. File-backed PSS overlaps cgroup file accounting
and must not be added a second time. These differing workloads are component
baselines, not an additive budget, controlled attribution of the earlier peak,
or a new worst-case bound. The earlier 512 MiB pressure finding remains valid.
All private social account copies were removed after evidence review; native
subscription homes and original accounts remain intact.


## Account recommendation and following feeds (2026-11-04)

The discovery tool now lists account feed references with `action=sources` and
opens them with the same tool. Recommended and following sources have distinct
references/cursors; omitted source selects account recommendations, not a
public popular feed. Pixiv selects ordered recommendation IDs from the real
homepage section and offers only its personalized tag topics. Tieba following
reads the account's actual followed-forum list and traverses those forums; it
does not use followed authors or guessed native tab types. Discord Highlights
are explicitly personalized notifications, not a homepage timeline. GitHub
received activity is explicitly a following/watched source, not an algorithmic
recommendation feed.

Network-disabled Docker checks passed: 207 Runtime unit tests, 6 execution-profile
routing tests, 6 MCP stdio tests, 170 platform SDK tests (2 separate tests remain
explicitly ignored), and 26 frontend checks. Additional targeted checks cover
real catalog-mode routing, followed-forum identity verification and current X
public-query extraction without executing JavaScript. The model tool catalog
still fits the 32-definition native limit; paused Weixin status compatibility
is retained. Source-path review reported no release blockers.

Real-account feed acceptance is recorded separately from these protocol tests.
No new X/Reddit/Discord session was requested for this change, and no live
platform writes were performed. On-demand media and the runtime/operator
separation are preserved.

Real-account reads on the VPS verified Bilibili, Tieba and Pixiv recommendation
and following feeds, cached tails, Bilibili/Pixiv content targets, and opening a
followed Tieba forum with its own pagination. GitHub verified the saved viewer
and returned a valid empty received-activity page. X, Reddit and Discord remain
pending real-account acceptance because no sessions were configured there.

Live responses exposed additional cases: Bilibili forwarded originals can have
an empty optional comment target, and Tieba forum pages can exceed the requested
20 posts. The adapters now preserve the full bounded provider page, and Tieba
creates target/media references only for delivered posts. Pixiv tag sections
can change on each homepage refresh; returned topic steps now read the exact
verified snapshot rather than fetching a different homepage. A network-disabled
regression exercises topic selection and pagination with an unusable synthetic
session, including source/account/viewer ownership checks. The follow-up suite
passed 210 Runtime unit tests, 6 MCP stdio tests and 10 targeted Bilibili dynamics
tests; the separate Bilibili feature integration suite passed 3 tests.

The final Pixiv topic read completed from cache in 0.30 seconds. A later Tieba
following request was rejected with `UrlPolicy`; a fixed self-identity endpoint
diagnostic returned HTTP 200 with first-party cookie domains, and retrying the
unchanged feed and cached continuation succeeded. The original rejection is
retained in private evidence; its exact cause was not established, and URL or
identity checks were not relaxed. These trials establish working reads, not a
request-success-rate guarantee.

The final read-only multi-platform trial observed one Runtime control container
at 18.52 MiB cgroup `memory.peak`, with 39 external samples and no sampling race;
sampling continued until the container exited. No model engine or media download
ran in that measurement. This is a feed-reader workload, not the memory budget
for Codex, Antigravity or concurrent agents. The VPS image retains its 512 MiB
container limit and the previously stopped worker remains stopped.

The local operator was updated and its Pixiv, Reddit and Discord provider icons
were visually checked in the installed native app. Provider artwork keeps its
owner's terms. Runtime updates were built locally in isolated Docker, delivered
over SSH and checked with an offline image doctor; no release or tag was made.
