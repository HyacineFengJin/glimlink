# Contributions and source ownership

Original contributions to this project are offered under MIT. Keep a
`SPDX-License-Identifier: MIT` header on original Rust source, and retain
contributors' copyright notices. The root license grants only rights held by
this project's contributors.

New Runtime workflows, memory, task scheduling, operator controls and engine
bridges should stay in original MIT modules. Separate new original glue from
adapted upstream implementations when the boundary is clear. A mixed file is
not made wholly original by adding our checks or translating it to Rust;
retain its upstream terms until its portions are properly identified.

When importing third-party code or data, record the exact upstream revision,
original authors, license text, copied paths and local changes in
[source-licenses.json](source-licenses.json) and the nearest NOTICE/PROVENANCE.
Preserve required notices. Referencing protocol fields and writing a client is
different from copying a client's implementation; record which actually
occurred. Pending origins stay marked pending, rather than being guessed MIT.

Cargo dependencies remain third-party. The generated
[dependency metadata](dependency-licenses.json) records pinned external
packages; it does not replace notices or license analysis of a shipped binary.
Driver/process boundaries do not automatically settle combined-work licensing.

Native engine programs, QQ signing modules, model downloads and user account
state are installed or supplied separately. Do not add credentials, private
observations, login screenshots, downloaded modules or unclear reference
corpora to source. Use original synthetic test inputs with reproducible
provenance. The path and ownership audit is:

```sh
python3 scripts/source-release-audit.py
```

Keep local and SSH operation on the same control protocol and codebase. The
thin operator has its own MIT package and avoids media/platform implementation
dependencies. Copyleft components keep their licenses in combined builds; do
not advertise the current integrated Runtime binary as MIT-only.
