# Glimlink

[English](README.md) | [简体中文](README.zh-CN.md)

以 Rust 为主的轻量 Agent Runtime，用于平台交互、共享人格与记忆。

Glimlink 把平台协议和媒体处理收拢成任务工具。AI 决定读什么、做什么，
Runtime 处理账号会话、目标、分页、下载和操作回执。

## 功能

- 接入 Codex、Antigravity 订阅登录，共享人格与记忆。
- 通过 MCP 或托管 Agent 宿主提供语义化平台工具。
- 按需处理图片、GIF、视频画面和语音。
- 精简无界面的 Runtime，与独立可选操控端通过本地管道或 SSH 连接。
- 中英文独立页面，可查看会话、发消息、切模型、调整 Agent 活跃状态及配置限定会话的管理指令。
- 登录账号的个性化漫游入口，与关注/订阅更新分开；见[推荐源范围](docs/personal-feeds.md)。

Runtime 不嵌入浏览器内核、V8、Blink 或 Chromium。桌面操控端可使用
操作系统的 WebView 完成人工登录。

## 平台范围

| 平台 | 当前范围 |
| --- | --- |
| Bilibili / 贴吧 | 个性化首页、独立关注动态/关注吧、内容及互动 |
| GitHub | 仓库、协作及独立关注用户/仓库活动 |
| X | For You、Following、资料、帖子及按需媒体；新信息流待实号验收 |
| pixiv | 个性化首页/标签推荐、关注作品及按需图片 |
| Reddit / Discord | Reddit Best/订阅更新；Discord 个性化 Highlights；待实号验收 |
| QQ | 已有 Rust 聊天与媒体实现，当前实号登录受阻 |
| 微信 | 暂停，默认构建不包含客户端 |

部分新增写入只有协议测试，尚未完成实号验收。具体边界见[验收记录](docs/validation.md)。

## 构建与运行

VPS 可使用 Linux Alpha 安装包，按[部署说明](docs/deployment.md)安装。
包内含 Runtime、终端操控端、起步配置和用户服务；订阅引擎从上游固定版本显式安装。

已验证目标为 Linux，需要 Rust 1.90+ 及系统 C 编译／链接工具。
请在开发机编译；512 MiB VPS 的内存目标针对运行阶段。

```sh
cargo build --release --locked -p agent-runtime --bin agent-runtime
cargo build --release --locked -p runtime-operator --bin agentctl
./target/release/agent-runtime tools
./target/release/agent-runtime --state .runtime/state mcp
```

目前 CLI 程序名为 `agent-runtime`，登录管理程序为 `agentctl`。
运行模型任务前需另外安装 Codex／Antigravity 原生程序；两者使用独立认证目录，
保留订阅登录。

```sh
./target/release/agentctl --runtime ./target/release/agent-runtime \
  --state .runtime/state --account bili-main bilibili login
```

继续阅读[操控端与 SSH](docs/operator-control.md)、[引擎登录](docs/engine-login.md)、
[岗位配置](docs/agent-profiles.md)和可选的[桌面操控端](operator-desktop/README.md)。
管理权限与指令边界见[调控与指令说明](docs/control-console.md)。

## 开发

Docker 构建和测试使用隔离的源码副本：

```sh
bash scripts/isolate.sh build online -- sh /workspace/scripts/cargo.sh fetch --locked
bash scripts/isolate.sh build -- sh /workspace/scripts/build-container.sh
bash scripts/isolate.sh build -- sh /workspace/scripts/cargo.sh test \
  --locked --offline --workspace --exclude platform-weixin --features agent-host-tests
```

首次构建需要下载依赖；可去掉 `--offline`，或在 `online` 构建容器中预先获取。
Runtime 测试限额为 384 MiB；完整 512 MiB VPS 工作负载仍需单独验收。

## 许可

本项目原创代码使用 MIT；第三方组件及移植部分保留原许可，其中包含 AGPL／GPL
部分，组合程序不属于纯 MIT。详见 [LICENSE](LICENSE)、[NOTICE](NOTICE)
和[源码归属说明](docs/OPEN_SOURCE.md)。

仓库不包含账号资料、下载的引擎／签名模块／模型、本地报告及构建产物。
