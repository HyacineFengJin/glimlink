# SILK v3 Rust decoder provenance

This is the standalone Skype SILK v3 format (`#!SILK_V3` and Tencent's optional `0x02` prefix), not the later SILK mode inside Opus. Only decoding, packet framing and the reference loss-concealment path are retained. The product builds this crate with `rustc`: there is no `build.rs`, C/C++ codec dependency, dynamic codec library, browser or runtime compiler.

The fixed-point decoder comes from Skype SILK SDK **1.0.9.6**, as preserved by [kn007/silk-v3-decoder at `507be6bca8ce1fb977a061481f1d79e8c610e309`](https://github.com/kn007/silk-v3-decoder/tree/507be6bca8ce1fb977a061481f1d79e8c610e309). Copyright (c) 2006–2012 Skype Limited. The original BSD-3-Clause license and disclaimer are preserved in [LICENSE](LICENSE). The enclosing repository's converters, Windows programs, FFmpeg archives, shell workflows and account features are not part of this crate.

| Input or generated artifact | SHA-256 |
| --- | --- |
| Pinned source archive, URL in [source-pin.json](tools/source-pin.json) | `d9ee6a2c5ee411f29f3ef31b4a43c238d8aaf942640792012db1ee348b147a38` |
| Decoder-only C amalgamation | `c5574fe2aa485363f3c5ba76889d4f6bc2e64a2838b07c4ba152a23b876cf673` |
| Raw C2Rust module | `83b626370acf21964b21d3cb71f74d0b2b02dba8a921e27529fae138df2812d0` |
| Committed [kernel.rs](src/kernel.rs) | `e5ec6475e76292c704de5fa37197f94d4b036a52338276c50f34d28ae09d4dc5` |

The 54-file decoder dependency closure is recorded in [decoder-sources.json](tools/decoder-sources.json). It includes range decoding, fixed-point filters, PLC/CNG, resampling and constant tables. Translation used C2Rust **0.22.1**, LLVM/Clang **15.0.6**, C99, `NDEBUG`, `-fwrapv`, generic fixed point and Rust **1.90.0**. `--emit-modules` is enabled, macro translation is `conservative`, and definition reorganization is disabled. The optional development image recipe is [Dockerfile.c2rust](tools/Dockerfile.c2rust); it is not a deployment dependency.

[clean_generated.py](tools/clean_generated.py) removes the generated foreign memory-function declarations and global ABI export attributes, supplies Rust pointer intrinsics for `memcpy`/`memmove`/`memset`, and preserves raw access to shared read-only codebook tables. Codec arithmetic and tables are unchanged. Private functions may keep the `extern "C"` calling convention, but they are Rust definitions with ordinary Rust symbol names, not calls into a native SILK library. The generated kernel uses unsafe raw pointers; the public wrapper owns one decoder state and bounds packet input and output.

The wrapper accepts 8/12/16/24/32/44.1/48 kHz output, at most 1024 bytes per packet and at most five 20 ms internal frames per packet. It recognizes standard signed-16 little-endian packet lengths, negative terminators, EOF and Tencent framing. Truncated records are errors. Empty packets invoke the SDK's concealment path; consumers must label their samples as concealed rather than received speech. A failed decoder is not reused. The Runtime adds its own file, packet, time-window and output budgets.

## Reproduction

Use a development workspace containing only the pinned public SDK and these tools. Do not mount Runtime state or account directories. Download the archive URL from `tools/source-pin.json`, verify the archive hash above, and extract it into a scratch directory. The SDK directory passed to the scripts is the extracted `silk` directory, containing `src`, `interface` and `test`.

`tools/regenerate.py SDK_SILK_DIRECTORY NEW_OUTPUT_DIRECTORY` runs inside the C2Rust tools image with the SDK mounted read-only and a writable scratch output. It performs no downloads, refuses an existing output directory, copies only SDK source/interface directories, and emits `kernel.rs` plus `generation.json`. It does not replace source files in place. A fresh run with the pinned tools reproduced the committed kernel byte for byte. C2Rust may print that optional `rustfmt` is absent; no formatting pass is required for the hashes above.

`tools/oracle_vectors.py SDK_SILK_DIRECTORY NEW_OUTPUT_DIRECTORY` runs inside a compiler development container. It builds the fixed C SDK reference with GCC **12.2.0**, `-O2 -DNDEBUG -fwrapv`, synthesizes mathematical PCM inputs and generates encoded streams and reference PCM. [synthesize.py](tools/synthesize.py) records the exact rates, packet sizes, signal patterns, DTX flags and input hashes. Input synthesis has no personal speech or platform access. The C encoder and decoder are oracle tools only and are not linked into Runtime.

Normal regression tests need neither C nor the original SDK: copy this standalone crate into a Rust 1.90 scratch build directory and run `cargo test --offline -j4`. The parent workspace excludes this vendor directory from workspace-wide tests, so test it explicitly when changing the wrapper or generated kernel.

## Recorded validation

The committed [vectors/provenance.json](tests/vectors/provenance.json) records every source and fixture hash. Seven two-second signals cover silence, sine, mixed tones/noise, a chirp, gated noise and impulses; input rates are 8/12/16/24 kHz, packets are 20/40/60/100 ms, and both standard and Tencent framing are represented. Their 24 kHz output contains **335,520 PCM samples** and matches the fixed C reference byte for byte.

Additional fixtures cover every exposed output rate (including 44.1 kHz resampling) and two four-second DTX signals with real zero-length packets. These add **560,200 PCM samples**, also byte exact. Total recorded comparison: **895,720 samples, zero differing bytes**. Separate tests cover malformed framing/headers, oversized packets, 96 deterministic malformed packets, poisoned-decoder rejection and preservation of a 100 ms packet duration during concealment. This is a finite synthetic compatibility suite; no private QQ/Weixin speech or full normative conformance corpus was used.

The Rust decoder harness was additionally executed in a network-disabled Debian slim container with 384 MiB, no extra swap, 2 CPUs, no SDK/compiler mount and no account/state mount. All exposed sample rates and DTX cases matched the development C oracle. The standalone unstripped Rust harness is approximately 556 KiB; deployment links only the library code it uses.
