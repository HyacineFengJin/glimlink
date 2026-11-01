# runtime-engine

Rust 1.90、edition 2024 的原生 Codex app-server 与 Antigravity ACP stdio 适配器。不依赖 runtime-core，没有浏览器、Node 或 V8 运行环境；引擎及工具代理必须是 Linux 原生 ELF。

使用 [OpenAI 官方 app-server 协议](https://learn.chatgpt.com/docs/app-server)，精确的动态工具字段还通过官方 `codex-cli 0.165.0 app-server generate-json-schema --experimental` 核对。`initialize`/`initialized`、`account/read`、线程创建/恢复和回合事件分开处理。

## 使用接口

```rust,ignore
let mut config = CodexConfig::new(binary, dedicated_codex_home, read_only_workspace);
config.timeout = Duration::from_secs(120);
let mut client = CodexClient::spawn(config)?;
client.initialize()?;
let status = client.account_read()?;
let thread = client.thread_start(Some(selected_model))?;
let result = client.turn_start(&thread.id, &inputs, |event| { /* bounded callback */ })?;
assert_eq!(result.status, TurnStatus::Completed);
```

`Input` 包括 `Text`、`LocalImage`，以及会明确拒绝的 `Audio`、`Video`。路径必须在 workspace 内，图片有文件大小限制；当前层不代替媒体模块做完整图像解码。[订阅 plan flow 文档](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations) 明确不支持音频/视频输入。不能把本地 app-server schema 中的 `inputAudio` 字段当作订阅通道可用的证据，也不自动以转写或字幕替换声音。

`CodexClient`不读取auth文件，也不复制桌面账户。子进程清空环境后使用显式`CODEX_HOME`、独立HOME、最小PATH与原生file credential store；认证及刷新由Codex管理。模型回合前只允许ChatGPT auth，线程显式使用openai provider；未登录及API-key状态拒绝回合。0.165.0/gpt-6.1-sol专用订阅已经通过真实文本、Runtime动态工具和工具结果图片验收；`model_input_verified=true`表示这一已测路线，不是所有模型/账号的兼容保证。

## 动态平台工具

`thread_start_with_tools(model, &[DynamicToolSpec])` 注册最多 32 个函数。每个工具的 `name`、`description`、`input_schema` 生成实际线上的 `type:function`、`inputSchema` 定义。

`turn_start_with_tools(thread, inputs, event_callback, tool_callback)` 将已注册且 thread/turn 匹配的 `item/tool/call` 路由到调用者。`ToolCall` 包括工具名、参数和 call ID；重复 call ID 不会再次执行。工具回调必须自行限制网络和其他 I/O 的时间，不能在同步回调里无限等待。

`ToolResult` 的 `content_items` 可以返回 `InputText{text}` 或 `InputImage{image_url}`。图片必须是带真实字节的 JPEG/PNG/WebP base64 data URL；不接受普通远程 URL、音频或空资源占位。仅检查格式签名，媒体解码和图像有效性仍由上游媒体模块负责。默认单图原始编码字节 512 KiB、完整结果 JSON 896 KiB、每回合 32 次请求；所有限制可以在 `CodexConfig` 内显式配置。

`thread_resume_with_tools` 仅重新绑定本地工具白名单，须与保存的线程定义一致。0.165.0 的 resume schema 没有替换 `dynamicTools` 的字段；服务端恢复它保存的定义。普通 `turn_start` 没有工具执行回调，工具不会借助 shell 绕行。

## 进程与资源边界

默认关闭shell/unified exec、code mode及其独立host/prewarm、浏览器/电脑使用、apps/plugins/hooks、多Agent和web search。线程使用原生schema的`sandbox=read-only`与`approvalPolicy=untrusted`，回合使用`SandboxPolicy {type:readOnly,networkAccess:false}`；未实现的`access/readableRoots`字段已移除，不能把只读误称为文件路径隔离。内置执行工具被关闭，非Runtime的审批/请求由适配器拒绝；Docker负责进程能实际接触的目录。修改`config.args`是可信宿主调试能力，不交给模型。

0.165.0部分模型metadata固定`code_mode_only`，其优先级高于功能开关，关host后仍可能返回host disabled。宿主`Settings::engine_config`会安装[data里的公开Direct目录](data/README.md)，通过启动参数`model_catalog_json`采用直接function调用；仅在`thread/start.config`配置会无效。该目录完整替换远端metadata，必须与CLI版本配套维护；没有更改真实model slug、权限或订阅认证。独立`codex-code-mode-host`含V8，既不部署也不启动。来源与更改说明见[目录记录](data/README.md)。

JSONL 单行、累计接收、输入、文本输出、工具结果、排队事件和 stderr 都有上限。stdin 写入也带超时，避免服务器不读输入导致挂起。模型终止状态区分 completed/failed/interrupted；item/completed 的最终文字覆盖 delta 草稿。服务器原始错误、stderr、email、账户 ID 和令牌不会进入公共输出。delta/final 回调内容是模型回答本身，调用者负责自己的存储策略。

超时或协议错误杀掉独立进程组并回收主进程，Drop 同样执行回收。这里不替代外层 Docker/cgroup：故意脱离进程组的外部子进程需要宿主的整体隔离兜底。引擎子进程峰值仍计入 VPS 总预算。

## 验证记录

- 在 Docker build 档的无账号、禁网环境中，原生 Rust fake app-server 的 10 项测试覆盖握手、账户过滤、会话恢复、最终事件、审批拒绝、图片输入、音视频拒绝、动态文本/图片工具、跨会话/重复调用拒绝、限额、写入超时和失败状态。
- 官方musl0.165.0的禁网空home探针验证认证阻塞；另经专用设备登录，真实gpt-6.1-sol订阅回合完成Runtime模块/状态查询，图片回合通过`media_read`获取真实JPEG并准确回答色带顺序。真实推理在384MiB容器执行，报告见根目录VALIDATION；原生音视频仍不可用。
- Linux musl 下载归档 SHA256：`be635d3486a5bd060d0fff582ae6d30160280c31344dd85719707fa920e187f0`。缓存原生二进制位于隔离目录 `/cache/bin/codex-native`，不发布进源码。
- fake 服务仅在 `test-helper` feature 下编译；测试命令为 `cargo test -p runtime-engine --features test-helper`，应在 `scripts/isolate.sh build` 容器的可写源码副本运行。

## Antigravity ACP

同一个官方原生agy1.4.11包含 `acp` 子命令，无需另一个ACP整包。`AntigravityConfig::new(binary, dedicated_home, workspace)` 与 `AntigravityClient::spawn / initialize / new_session / prompt` 接入ACP v1；`next_turn_id()` 给出实际发出prompt前可持久保存的请求身份。所有业务工具通过MCP交给Rust宿主，传输层等待响应时每25ms调用pump，避免模型等工具而宿主只等模型的死锁。

普通任务不发送authenticate。新session若返回认证错误，调用方进入needs_auth且不发送prompt；管理面通过原生CLI个人Google OAuth登录，缓存由它管理，环境不继承API key、ADC或桌面keyring。专用HOME及workspace须分离，HOME目录0700，托管配置0600。凭据内容不由Rust适配器读取或复制。

实测原生参数为 `_meta.disableBuiltInTools=true` 和 `_meta.antigravity.systemPrompt.append=人格/岗位指令`；顶层同名禁用字段会被忽略。`--include-default-customizations=false` 关闭默认组件，`--disable-telemetry` 与禁止自更新环境显式设置。原生 `--app-data-dir` 只能为相对名字，此处为antigravity-cli，位于绝对gemini-dir内，与普通CLI登录缓存一致。配置只allow `mcp(runtime/*)`，原生命令/文件/URL拒绝；ACP客户端的权限、文件和终端请求也会被拒绝。

ACP输入仅接受已验收文字，直接图片/音频/视频明确Unsupported；只收集本session的agent_message_chunk，不把思考、用户回显或工具遥测当作回答。stopReason、请求ID、事件及输出均有检查/上限；JSONL默认1MiB、累计8MiB、输入128KiB、输出256KiB，超时/坏协议终止进程。工具结果图片需另行真实验收，不由ACP初始化的media字段推定。

Go运行时使用GOMEMLIMIT=128MiB、GOGC=60；软目标不包含所有原生/映射内存，完整进程与文件缓存仍由外层384MiB容器及VPS预算约束。两套引擎退出后均对其只读ELF做POSIX_FADV_DONTNEED提示，减少串行切换时保留的文件缓存；内核可忽略建议。离线协议、MCP桥与真实订阅验收范围见[验收记录](../../docs/validation.md)。

Detail-image tool replies use the same bounded transport: Codex defaults to a 1 MiB JSONL frame, 896 KiB encoded tool-result allowance and 512 KiB raw image allowance; Antigravity JSONL and the Rust MCP bridge also cap frames at 1 MiB. These are on-demand message limits, not additional resident services. Runtime reports actual sent image geometry and refuses oversized detail instead of silently reducing it.
