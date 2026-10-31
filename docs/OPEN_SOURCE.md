# 开源范围、来源与发布准备

我们的原创实现采用 **MIT**。第三方源代码、移植、转译内核和复制的数据保留各自来源与许可；不能因为代码改成 Rust、放进我们的 crate，或此前自己填过 AGPL 字段，就改变其作者和授权。

根 [LICENSE](../LICENSE) 是原创部分的 MIT 授权，[NOTICE](../NOTICE) 列出排除和保留的第三方权利。机器清单是 [source-licenses.json](source-licenses.json)。清单按来源记录，不把旧 Cargo `license` 字段当作第三方归属证据。规则按顺序匹配，最后一条生效：宽泛原创分组在前，具体第三方或待审例外在后。每个候选文件都必须有归属，未分类会使检查失败。

## 原创与第三方边界

| 文件或模块 | 当前来源判断 | 原创 MIT 标识与保留范围 |
| --- | --- | --- |
| `src/`、`build.rs`，core/content/jobs/control/operator 的 Runtime 实现 | 我们编写的资源、存储、记忆、任务、工具、控制及操作端逻辑 | 可将明确原创 Rust 文件和包元数据标 MIT；它们调用的库仍是第三方 |
| `runtime-engine/src/` 与 GitHub REST adapter | 我们编写的协议客户端、进程管理、桥接和语义工作流 | 原创代码 MIT；不能顺带把模型目录或被调用的 CLI 算成我们的代码 |
| `platform-x` 网页会话 SDK | 原创有界 HTTP/编排与上游 MIT 参考/适配；交易签名算法有明确 Rust 翻译来源 | 逐项保留 Twikit 与 XClientTransaction 的原始 MIT 版权和固定提交；翻译不变成我们的原创，详见其 NOTICE |
| `operator-desktop/` 与 `runtime-login-window` | 原创静态前端、Tauri 桥接、验证窗口生命周期及系统 API 薄绑定 | 原创部分 MIT；Tauri、GTK、WebKit、JavaScriptCore 的实现与许可仍属于第三方，独立于 VPS Runtime |
| `runtime-speech/src/`、`runtime-media/src/` | 我们编写的模型安装/工作流、重采样与有界媒体包装 | 原创代码 MIT；Candle、tokenizers、Symphonia、解码内核和模型权重另算 |
| `platform-qq` 的 `protocol.rs`、`qr.rs`、`message.rs`、`auth.rs` 及 NOTICE 所列 rich/目录/历史/上传部分 | 明确有 LagrangeGo 适配/翻译证据 | 上游是 AGPL；新 glue 可以逐段认定原创，但现阶段不整目录改成 MIT |
| QQ `tea.rs` | 引用了 gofastTEA GPL3 实现及独立矢量 | 保留既有许可和出处，区分协议算法事实与实际移植代码后再决定边界 |
| QQ state/wire/AppInfo/key rotation/signer/password 的新增包装 | 有原创实现，也仍引用原实验和协议 | 逐文件审核；先前自填的 AGPL 不是全部来自别人之证明，反之也不能直接证明全部原创 |
| `vendor/nihav-*` | 固定提交的 NihAV 第三方原文，裁过 manifest | 保留 AGPL 与 `NIHAV-COPYING`；H.264 解码实现不是我们的 MIT 代码 |
| `vendor/runtime-silk/src/kernel.rs` | Skype SDK decoder 闭包的 C2Rust 转译 | 保留 Skype BSD3 和 [生成来源](../crates/runtime-media/vendor/runtime-silk/PROVENANCE.md)；Rust 语言不改变其归属 |
| SILK `src/lib.rs`、`src/bin/decode.rs`、`tools/` 和原创合成测试 | 我们编写的所有权/帧包装、工具与数学信号 | 原创部分可 MIT；生成器的 MIT 不会把生成的 SDK 内核变成 MIT |
| `vendor/rvoip-amr` | rvoip-codec-core 0.4.0 的第三方 MIT 内核/表格；少量 module/manifest 是本项目裁剪 | 保留 Jonathan Eisenzopf 的版权和 MIT 文本，不能仅保留我们的署名 |
| `runtime-engine/data/codex-0.165.0-direct.json` | OpenAI Codex 模型目录的修改版 | 保留 Apache2、OpenAI 来源和 `tool_mode` 修改说明，不宣称原创目录 |
| B站/贴吧/微信协议适配 | Rust 请求/解析/工作流可能是原创；各文件还引用 API 字段、公式、schemas 或固定数据 | 依据实际表达/移植判断。仅查接口文档不自动使原创代码变成 GPL；复制程序片段也不能仅称“参考”而忽略许可 |

对应的许可主源已经分别核对：LagrangeGo 为 AGPL3；gofastTEA 和所引用的 bilibili-api 具有 GPL3 文本；固定 aiotieba 提交采用 [Unlicense](https://raw.githubusercontent.com/lumina37/aiotieba/3922a9f1f4643f4652bc4a33f853e8aea29ffe24/LICENSE)；libsgh/tieba-api 为 [MIT](https://raw.githubusercontent.com/libsgh/tieba-api/master/LICENSE)；openwechat 为 [Apache2](https://raw.githubusercontent.com/eatmoreapple/openwechat/master/LICENSE)。后两者及 B站引用仍需把具体源版本固定到提交，而不是只留可变化的分支链接。

微信 `desktop.rs` 的固定兼容头有逐字复用来源，必须保留其数据出处；其余新写的 Rust 流程要单独判断。B站 `comment_images.rs` 声明的是字段参考，`wbi.rs` 是公开置换/MD5 配方；是否复制了受保护的具体实现仍应按片段检查，不能因链接了 GPL 文档就把整个 B站 SDK 直接认作别人的 GPL 程序。

[贡献规则](contributing.md)固定后续原创 MIT 与第三方导入要求。268 个锁定外部 Cargo 包的许可元数据与校验和见 [dependency-licenses.json](dependency-licenses.json)，包括可选和平台专用依赖，不等于最终二进制 SBOM。

Cargo 下载依赖也不成为我们的原创代码。例如缓存的包元数据记录 html5gum 为 MIT、Candle 为 MIT/Apache2 二选一、tokenizers 为 Apache2、Symphonia 为 MPL2。源码发布保留依赖锁；二进制发布还需要按实际 feature/依赖图整理第三方许可，而不是只放一份根 MIT。

可选桌面单独维护 [Cargo 依赖许可元数据](../operator-desktop/dependency-licenses.json)，不混入 Runtime 的依赖清单。该清单还不包含系统安装的 GTK/WebKit/JavaScriptCore，不能当作完整二进制 SBOM 或许可文本包。原创应用图标有单独来源说明；未复制字体或插画。平台图标的本地来源记录见[第三方资产清单](../operator-desktop/frontend/brands/README.md)。公开源码使用文字标识，未确认分发条件的图标文件排除；原文件保留在本地，不属于我们的 MIT 资产。

## 同一套 Runtime 与许可层次

产品保持同一套源码和控制协议：本地控制自身 Runtime，SSH 场景连接目标 Runtime，不因运行位置维护两套分叉版本。源码中的模块和可选驱动可以有各自许可，这与维护两个产品版本不同。

目前主程序还会静态链接 QQ 和 NihAV 等 copyleft 部分。**原创文件是 MIT，不等于当前组合可执行文件是纯 MIT。** 应在改动构建边界后依据实际链接产物判断。GNU [AGPL3](https://www.gnu.org/licenses/agpl-3.0.html) / [GPL3](https://www.gnu.org/licenses/gpl-3.0.html) 的组合及对应源码条件不会被根 MIT 文件覆盖。

建议的演进边界是：原创 headless core 负责账号不透明引用、存储、工具、调度、模型桥接和同一控制协议；明确 copyleft 或来源待审的协议/视频实现作为可选 driver/worker。关闭某个 driver 时必须真的不进入核心构建依赖，而不只是启动时不用它。Worker 可以按需启动，以免增加常驻服务；媒体链仍由 Runtime 编排，AI 继续使用一次 `media_read` / `social_send`。

这是工程分层方案，尚不能把当前构建当成已经完成的 MIT-only core。单独进程、IPC、RPC 或放到另一目录并不会自动消除组合/衍生关系与来源义务；接口、共享代码和实质耦合仍需核对。若保留 copyleft 组件的组合发行，应保留其许可、修改说明和所需对应源码，而不是删除版权或重新贴 MIT。

## 参考矢量隔离

`vendor/rvoip-amr/src/codecs/amr/qualification.rs` 明确区分 Apache2 OpenCORE 输出与仍有再分发疑问的 3GPP reference 输出（上游标为 IP-2b）。不能因外层 crate 写 MIT 就推定每个参考资产已完成权利确认。

当前公开候选清单排除该上游 `testdata` 目录的 **112 个文件**，不删除本地副本。父级原先六处 `include_bytes!` 已全部切换到 [我们生成的数学信号](../crates/runtime-media/tests/vectors/amr-original/README.md)：1 秒方波、斜坡和 xorshift 噪声，不含真人语音、账号数据或上游 reference corpus。

独立开发 oracle 使用 Apache2 OpenCORE/VisualOn；开发包来源、哈希、原创 C harness 和六个输出哈希均已固定，运行时仍使用 Rust 编解码器，不分发这些开发库。替换后 runtime-media 9 项测试通过，原生 AMR 编码 bitstream 与窗口解码 PCM 均匹配独立输出，父级对隔离目录的引用为 0。上游 vendor 自身依赖被隔离 corpus 的独立 qualification 测试仍未纳入公开验收；不能将其原有测试结果当作本项目完整复现。SILK 的 SDK、C2Rust 生成和自制矢量有独立可复现记录。

## 发布清单与排除验证

`.gitignore` 和 `.dockerignore` 采用顶层源码 allowlist，并再次排除嵌套状态、缓存、报告、账号导入文件名、原生二进制和模型权重。整个 `.runtime/`、`experiments/`、本地实号验收 `examples/`、未经公开整理的 `research/` 和全部历史大写 `**/VALIDATION.md`（新的公开摘要 `docs/validation.md` 不在此排除项） 不进入候选发布范围。旧原型、真实消息/登录报告与运行缓存因此不会随着 `git add .` 的默认行为被带入。任何人为强制加入都需要另行审核。

运行：

```sh
python3 scripts/source-release-audit.py
python3 scripts/source-release-audit.py --list
```

该脚本只读取已检查的来源策略 JSON，并查看候选目录的路径/文件元数据；不打开任何私有文件内容、不扫描密码值、不复制文件、不创建压缩包、不初始化 Git、不提交和不发布。它拒绝候选目录内的符号链接。输出包含拟公开的源码路径、按原因计数的排除项、隔离的引用和剩余发布障碍。

`path_audit_passed=true` 只代表路径 allowlist 检查通过。`ownership_classification_passed=true` 表示候选文件都有归属记录，包括明确待审的分类。这两项结果都不是秘密值扫描、第三方权利确认或法律清关。公开源码本身仍需最终内容审核，所有待审资产/引用和元数据冲突解决前 `publication_ready` 保持 false。脚本列出的历史排除项本身不被读取。

Codex/Antigravity 的原生程序、QQ 原生 signer、下载模型和账户资料留给用户在自己的环境显式安装/登录。源码中可以保留已固定的校验和、接口和安装说明，但不能据此推定有权重新发布那些二进制。`prepare-qq-signer.sh` 已要求显式提供兼容模块的绝对文件路径并校验固定哈希，不再默认从旧实验缓存复制。旧 `import-qq.sh` 同样是历史迁移辅助，不是干净 checkout 的账号来源。

已确认原创的 Root/核心/控制端源码以及 QQ 的独立 wire/session/AppInfo/管理包装、B站与贴吧的已确认原创客户端文件、微信原创状态和 HTTP 模块已逐文件标 MIT。混合文件继续保留原标识并列待审；后续先拆清原创和第三方片段，再同步包级 SPDX。不能把旧包级自选 AGPL 当作第三方来源证据。
