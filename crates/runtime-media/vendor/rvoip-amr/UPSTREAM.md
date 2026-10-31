AMR-only Rust fork of rvoip-codec-core 0.4.0 (MIT).
Source: https://crates.io/crates/rvoip-codec-core/0.4.0
Repository: https://github.com/eisenzopf/rvoip
Original archive SHA-256: b1c28127efb2f580e03243c5999ff8a8c13b0ed4cf8a03aaafdb37c23fbf8cbb
Changes: only AMR, shared fixed-point helpers, error and types modules retained; no SIP/RTP host, Opus/native codecs, logging service, tools or reference C programs. Minimal manifest accepts Rust 1.90, verified in this workspace. AMR kernels unchanged. Upstream tests are retained as source; the parent workspace exercises actual decoding via reference fixtures. Upstream conformance claims are not independently reproduced here unless specified in VALIDATION.md.
