# Original AMR fixtures

These fixtures contain one second each of project-authored integer square,
ramp and xorshift noise, with no speech or account data. The input formula is
in [amr_reference.c](../../oracle/amr_reference.c). The harness, mathematical
inputs and resulting test fixtures are offered under the project's MIT License.

The independent development oracle is OpenCORE AMR 0.1.6 (NB encoder/decoder
and WB decoder) plus VisualOn AMR-WB encoder 0.1.3, under their Apache-2.0
terms. This does not change the licenses of those external libraries. The
libraries and Debian packages are not distributed here or linked into the
Runtime; production encoding/decoding uses Rust. No 3GPP reference test
corpus or upstream rvoip reference vectors are copied into these fixtures.

`provenance.json` pins package download URLs, package hashes, the harness hash,
and each output hash. Reproduce in a disposable development container: extract
those three verified development packages with `dpkg-deb -x`, compile the
harness with their include/static-library paths and `-lopencore-amrnb
-lopencore-amrwb -lvo-amrwbenc -lstdc++ -lm`, and run it with an empty output
directory. It refuses to overwrite outputs. Compile the harness as C, with
the static codec libraries before `-lstdc++ -lm`.

`nb-input.pcm` / `wb-input.pcm` are S16LE mono 8/16 kHz. Encoded files use 50
20 ms frames, NB MR122 and WB mode 4, with DTX disabled. The decoded `.pcm`
files are independent oracle output. Runtime tests compare both encoding bits
and exact windowed decoder PCM, including history before the selected window.
