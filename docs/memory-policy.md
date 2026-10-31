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
