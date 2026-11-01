# Runtime memory is a design constraint

The deployment Runtime targets a **512 MiB VPS in total**. Native model-engine
children, media work, signing modules and page-cache pressure share that machine.
Memory economy is a default requirement for every Runtime feature, rather than
a final packaging step. Compilation is unrestricted and development tests stay
isolated in Docker.

The human operator desktop has a separate initial budget: **below 1 GiB** for
the optimized app and its login windows. This is a ceiling, not a target to fill.
The Tauri frontend and native login window are optional local components. Their
budgets and dependencies never increase the VPS Runtime budget.

## Boundaries that must remain true

- No WebView, GTK, Tauri, Wry, Chromium, Blink, V8 or JavaScript renderer enters
  the Runtime dependency graph or its loaded libraries. The desktop has its own
  Cargo workspace and lock; the optional native login window is never a core
  dependency and has no default GUI feature.
- Startup creates no browser, decoder, model or per-module background process.
  Expensive components load when a real operation needs them and release their
  working memory when that operation finishes. Native engine processes count
  toward the Runtime workload budget.
- Large bodies and media stream to bounded disk resources. Public tools pass
  references and bounded observations. Avoid whole-file buffers, duplicated
  JSON trees and repeated byte/base64 copies when a stream or borrowed range
  can serve the same operation.
- Cache, inbox, history, queue, context and pagination state have explicit caps.
  One model job and bounded media windows run at a time; no speculative parallel
  downloads or model preloads are enabled to improve a demonstration.
- New behavior includes a measurable lifetime and peak budget. Optimize the
  largest measured allocations first, preserve correct results, and measure
  after changing ownership, copying, caching or concurrency.

## Measurement

Use optimized binaries. Report workload, limits, child processes, RSS/high-water
RSS, PSS where available and cgroup memory peak. Binary size and compilation
memory are different measurements. Warm shared file mappings can change cgroup
charging, so a small cgroup number alone is not a complete VPS budget.

The empty-account control smoke has an initial **32 MiB per-core RSS regression
gate**, with the observed result recorded rather than treated as a target. It
also checks loaded libraries and rejects GUI/browser linkage. This gate covers
startup and status/tool discovery only. Real media and native engine workloads
must still pass their separate 384 MiB runtime acceptance on the 512 MiB target.

```sh
bash scripts/isolate.sh build operator-webview -- python3 \
  /workspace/scripts/validate-runtime-memory.py /cache/bin/agent-runtime
```

The development image supplies the measurement tool. It mounts no account state;
the tested core must not load that image's GUI libraries.

For the independently built desktop, use the separate client fixture:

```sh
bash scripts/isolate.sh client-test operator-webview -- dbus-run-session -- \
  xvfb-run -a -s '-screen 0 1180x820x24' python3 \
  /workspace/scripts/validate-operator-desktop.py /cache/bin/agent-operator-desktop
```

This fixture mounts no account state and has a 1 GiB cgroup ceiling. Its report
includes the real application's WebKit children, RSS/PSS samples, container peak
and OOM events. Optional synthetic status inspection also includes native engine
probe processes. It does not claim a real provider verification page's peak.
The [acceptance summary](validation.md) records the measured workloads and limits.

## Simulated use and overlapping workloads

The opt-in `runtime-memory-workload` binary exercises the real Rust Runtime and
native subscription engines. It is excluded from default builds. The external
`scripts/measure-runtime.py` monitor samples the whole Docker cgroup and every
child process; its own Python process is outside the measured container.

With the native binaries and speech model already installed in the development
cache, build the workload and prepare synthetic image/GIF fixtures:

```sh
bash scripts/isolate.sh build -- sh /workspace/scripts/cargo.sh build \
  --release --locked --offline --features memory-bench \
  --bin runtime-memory-workload
install -m 700 .runtime/cache/target/release/runtime-memory-workload \
  .runtime/cache/bin/runtime-memory-workload
bash scripts/isolate.sh build -- /cache/bin/runtime-memory-workload prepare mem-fixtures
# Supply a short test speech WAV explicitly; it is never downloaded implicitly.
install -m 600 /path/to/test-speech.wav .runtime/cache/memory-bench-fixtures/speech.wav
python3 scripts/measure-runtime.py --run-id mem-example
```

Defaults are a 384 MiB cgroup limit, two CPUs, no swap and nominal 100 ms samples.
Offline cases use synthetic media and the supplied speech WAV; the measured
acceptance used the public JFK sample. Online cases reuse Runtime-owned Bilibili/Tieba/X sessions
and native Codex/Antigravity homes; GitHub reads the public project repository.
They neither initiate login nor perform platform writes. QQ and Weixin are
excluded. The fixed local acceptance account aliases are part of this fixture,
not a general account discovery API.

Reports and per-case state live only in ignored, private `.runtime` directories.
Reports contain counts and memory/process metadata, not account credentials
or platform response bodies. Actual-use cases additionally save the requested
Agent answers in separate private `*-answer.json` artifacts; the monitor does
not print them. Private copies of native binaries
and model files receive best-effort file-cache eviction advice before each cold
case; shared system libraries and native-profile caches can remain warm. This
is not a cold-boot VPS measurement or a global host-cache flush.

`platform-parallel` uses four independent Runtime processes with separate state.
The combined cases retain those processes while a fifth Runtime runs one model
task and speech transcription; a second platform request wave starts when the
model invokes its tool. Another 32 jobs remain queued. This exercises overlap
across processes without claiming concurrent dispatch inside one Runtime.
`codex-platforms` and `antigravity-platforms` instead ask one agent task to use the
semantic tools for all four platforms and their returned references.

The `*-use`, `*-watch` and `*-speech` cases provide ordinary Chinese goals and
let the Agent select tools. `*-vision` compares fresh random image pixels with
the model's answer, covering a still image and four frames without exposing
the expected colors in tool text. Model selection is frozen in the test job:

```sh
python3 scripts/measure-runtime.py --run-id agent-use-example \
  --cases codex-use,antigravity-use,codex-watch,antigravity-watch \
  --codex-model gpt-6.1-terra \
  --antigravity-model gemini-3.8-flash-medium
```

The named models are explicit local acceptance targets, not universal deployment
defaults. The opt-in Runtime records bounded per-tool status/count/image metadata;
default builds contain neither the trace buffer nor its recording code. Review
actual media replies against the sampled frames: model completion and tool
success are separate from interpretation accuracy.

Always inspect functional completion and `memory.events`, not only exit status.
A model can finish a reply without invoking the required tools. A peak equal to
`memory.max` with reclaim events and no OOM is a pressure result, not proof of
comfortable headroom. RSS/PSS are sampled and may miss brief allocation spikes;
the kernel cgroup high-water counter captures the overall peak. Docker does not
include the host OS or operator desktop in that number.

## Vocabulary working set and autonomous home-feed acceptance

The speech path keeps its vocabulary in Q8_0 storage and expands bounded f32
rows/tiles, rather than materializing the full ~76 MiB f32 table. Projection
uses the original f32 arithmetic and pinned weights, not another activation
quantization. Encoder weights are released after feature extraction. On Linux,
loaded GGUF file pages receive a readonly per-file DONTNEED hint; the owned CPU
weights remain intact. Subsequent windows may require more disk I/O.

The Antigravity Go soft target remains 128 MiB by default. A trusted embedding
may set `AntigravityConfig.go_memory_limit_mib`; 64 MiB was evaluated, including
successful autonomous tasks, but one later job ended without a final reply.
The data do not establish causation, so the lower target is not adopted as the
default. A soft Go target is not an RSS cap.

`*-browse` starts from the signed-in homepage recommendation tool and lets the
Agent choose a candidate and at most two ten-second windows. The measurement
checks the selected video against actual returned candidate IDs, image delivery,
window budget, and final reply. It never posts the model's evaluation.

```sh
python3 scripts/measure-runtime.py --run-id home-example \
  --cases codex-browse,antigravity-browse \
  --codex-model gpt-6.1-terra --antigravity-model gemini-3.8-flash-medium
```

`--program-dir` permits a private archived binary set for controlled before/after
runs. Reports include executable hashes. Memory improvements must include actual
working-set/RSS/PSS evidence; file-cache reclaim alone is not a smaller live
model. Observed tasks, account scope and one-run measurement limits remain
separate from full-machine or long-running deployment acceptance.

On GNU/Linux the completed speech call also requests `malloc_trim` after its
worker pool and model temporaries have finished, releasing unused allocator
pages before the Agent waits on more I/O. No background trimmer is started.
The four-round image/speech fixture retained about 18 MiB process-tree PSS,
versus 156 MiB before these changes; this is distinct from container file cache.

## Source-aware visual detail

AI may choose source quality and a normalized source region through the existing
media tools. Compact preview remains available; text/UI detail rereads use a
source-linked frame reference, bounded HD AVC retrieval and a single cropped
frame. Cropping the YUV-to-RGB conversion avoids a full RGB copy, but H264
reference buffers still decode the full source. The normal 96 MiB conservative
decoder estimate remains; single-frame, no-audio inspection has a 192 MiB
estimate ceiling and a one-second/one-frame limit. Whole-process/cgroup memory
must still be measured; the estimate is not RSS.

Prepared geometry and actual model attachment geometry are separately reported.
Detail is not automatically resized to the old 64 KiB preview budget: a bounded
512 KiB total raw-image allowance fits within the 1 MiB wire frame. An oversized
detail produces an explicit input-unavailable/partial result so the AI can choose
a tighter region or explicit JPEG. There is no upscaling substitute for HD.

## Context on the VPS

Inference KV caches for remote models are held by the inference service. This
does not make client context free: Runtime/CLI prompt strings, parsed JSON,
serialization, tool responses and decoded media still consume VPS memory.
Character/token counts and local byte/RSS counts are different budgets.

The current engine-independent assembly already supplies a shared persona/role,
selected attributed recollections, a conversation recap, bounded recent dialogue
or autonomous checkpoints, and references to larger resources. Each managed job
starts a new native engine thread/session. Recent dialogue retains up to eight
turns in at most 16 KiB of encoded history; a recalled-memory snapshot is capped
at 16 KiB, including at most 6 KiB of recalled facts. Native request paths have
128 KiB input limits. These are separate bounds, not a guarantee that every tool
turn or image decoding allocation fits in 128 KiB.

The next context optimization should add one measured assembly budget with
per-category reservations, separate media budgets, source references for omitted
material, and per-turn byte/peak reporting. Important identity instructions,
current intent and durable action receipts must survive compaction; historical
observations can stay on disk and be expanded on demand. Do not introduce an
always-resident vector index, or assume that summarizing everything into one
ever-growing text is memory management. Whole-file JSON loading and duplicated
serialization buffers still need measurement before choosing an optimization.
