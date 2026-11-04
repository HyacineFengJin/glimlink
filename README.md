# Glimlink

[English](README.md) | [简体中文](README.zh-CN.md)

A lightweight, Rust-first agent runtime for platform interaction, shared identity and memory.

Glimlink turns platform protocols and media processing into task-oriented tools.
Agents choose what to read and do; the runtime handles account sessions, targets,
pagination, downloads and action receipts.

## Features

- Codex and Antigravity subscription sessions, with shared persona and memory.
- Semantic platform tools through MCP or the managed agent host.
- Account-personalized roaming feeds, with following/subscription updates kept separate; see [feed support](docs/personal-feeds.md).
- On-demand image, GIF, video-frame and speech processing.
- A headless runtime and a separate optional desktop operator, connected locally or over SSH.
- English/Chinese control pages for conversations, models, Agent activity and scoped Runtime commands.

The runtime contains no browser engine, V8, Blink or Chromium. The desktop can
use the operating system's WebView for human login.

## Platform support

| Platform | Current scope |
| --- | --- |
| Bilibili / Tieba | Personalized home, separate following dynamics/followed forums, content and interactions |
| GitHub | Repositories, collaboration and separate following/watched activity |
| X | For You, Following, profiles, posts and lazy media; new feeds await real-account acceptance |
| pixiv | Personalized home/tag recommendations, followed artwork and lazy images |
| Reddit / Discord | Reddit Best/subscriptions; Discord personalized Highlights; real-account acceptance pending |
| QQ | Rust chat/media implementation; current real-account login is blocked |
| Weixin | Paused and excluded from the default build |

Some newer write operations have protocol tests but incomplete real-account
acceptance. See [validation](docs/validation.md) for the exact boundaries.

## Build and run

For a VPS, use the Linux Alpha archive and follow [deployment](docs/deployment.md).
It includes the Runtime, terminal operator, starter configuration and a user
service; native subscription engines are explicitly installed from pinned upstream assets.

The validated target is Linux. Use Rust 1.90+ and a system C/linker toolchain.
Build on a development machine; the 512 MiB VPS budget applies to runtime use.

```sh
cargo build --release --locked -p agent-runtime --bin agent-runtime
cargo build --release --locked -p runtime-operator --bin agentctl
./target/release/agent-runtime tools
./target/release/agent-runtime --state .runtime/state mcp
```

The CLI binary is currently named `agent-runtime`; `agentctl` manages login.
Install the native Codex/Antigravity programs separately before running model
jobs. Authentication remains in dedicated profiles and preserves subscription login.

```sh
./target/release/agentctl --runtime ./target/release/agent-runtime \
  --state .runtime/state --account bili-main bilibili login
```

See [operator and SSH setup](docs/operator-control.md),
[engine login](docs/engine-login.md), [agent profiles](docs/agent-profiles.md)
and the optional [desktop operator](operator-desktop/README.md).
See [control and commands](docs/control-console.md) for the human management boundary.

## Development

Docker builds and tests run against an isolated source copy:

```sh
bash scripts/isolate.sh build online -- sh /workspace/scripts/cargo.sh fetch --locked
bash scripts/isolate.sh build -- sh /workspace/scripts/build-container.sh
bash scripts/isolate.sh build -- sh /workspace/scripts/cargo.sh test \
  --locked --offline --workspace --exclude platform-weixin --features agent-host-tests
```

The first build needs dependencies available; omit `--offline` or fetch them in
an `online` build container. Runtime tests use a 384 MiB limit. A complete
512 MiB VPS workload is still a deployment target, not a blanket memory guarantee.

## License

Project-authored code is MIT. Third-party components and adaptations retain their
own licenses, including AGPL/GPL portions; the combined program is not MIT-only.
See [LICENSE](LICENSE), [NOTICE](NOTICE) and [source ownership](docs/OPEN_SOURCE.md).
Account data, downloaded engines/signers/models, local reports and build outputs
are not part of this repository.
