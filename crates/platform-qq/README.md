# platform-qq

同步、单连接的 Rust NTQQ 聊天库，从 QQ 实验独立迁出协议、缓存恢复、TempPwd 登录、收包和文本发送。不会执行实验 CLI，没有自己的后台线程、历史存储或 outbox。宿主负责账号锁、生命周期、收件箱、媒体处理和操作记录。实验文件及原 outbox 保留原样。

聊天能力现在包含原生群 @、引用回复、QQ 表情，以及图片/GIF、语音、视频和文件的类型化接收。`rich::Message.parts` 保留文本、@、表情的原始顺序；引用绑定来源序号、发送者 UIN/UID、时间。附件只提供私有解析材料，宿主向 AI 返回账号隔离的 `media_ref`，按需解析下载，不自动抓取。未知组件保留原始 PB，不能当成已经读懂。

`friends`、`groups`、`members` 使用固定原生 OIDB 命令及有界分页。私聊漫游历史按时间分页，群历史按序号分页；宿主可先取得最新群消息序号。读取历史不标已读，也不作为新消息重新写入监听收件箱。服务器没有提供的历史不能凭本地缓存补造。

`upload_image`、`upload_voice`、`upload_video`、`upload_file` 进行原生上传协商，必要时用 1 MiB 块串行走 Highway TCP。图片支持 JPEG/PNG/GIF/WebP；语音支持原生 SILK v3/AMR-NB；视频使用 MP4 和 JPEG 缩略图，视频 SHA-1 中间状态与上游算法保持一致。宿主统一封装下载、普通音频转 AMR、视频首帧、上传和发送，AI 不需要串这些步骤。私聊文件使用 Trans0x211 路由；群文件由单独的原生 publish 命令发布。发送前宿主必须把原始资源引用、实际字节摘要、目标、正文和随机值写入同一个 Outbox，再开始上传/发送；未知结果不重发。

`ClientConfig` 的 `session_file`、`device_file` 和可选 `signer_module` 均为显式绝对路径。`Client::open(config)` 加载指定现存私有文件并建立普通 QQ TCP 连接；也可调用 `Client::connect(session,device,config)`。导入的 Session JSON 保留实验版的 version、UIN、UID、sequence、票据、TempPwd、Tgtgt、cookies 和未知扩展字段，文件权限仍为 0600。设备需复用已有 GUID。

`authenticate(LoginMode::{CachedOnly,Auto,Refresh})` 进行缓存注册或一次 TempPwd 登录。Auto 仅在明确的会话拒绝后刷新；网络错误直接上报。登录挑战作为包含私有材料的类型交还宿主，不写公共日志，不自动扫码或重新生成设备。只有成功 Register 后才原子保存新登录态。没有导入二维码自动回退流程。

管理端可显式调用 `login_qr(&mut client, output_dir, &stop, on_qr, on_status)`。它复用当前设备，用固定 `keyrotate.qq.com` HTTPS 入口取得轮换公钥，再运行同一版本的 OICQ fetch/poll/login；不会从 Auto 自动进入。180 秒截止时间贯穿取公钥与 SSO 等待，每三秒轮询，取消标志在等待期间检查。回调应迅速返回；库不打印路径、不显示图片，也不启动外部程序。`on_qr` 只在私有 0700 目录中的 `qrcode.png` / `qrcode.json` 以 0600 保存后收到 PNG 路径；URL不进入公共输出。已有账号时先核对扫码 UIN，再核对返回 UID，异账号在 Register/保存前拒绝。只有同账号 Register 成功后才持久化 Session，失败、取消、过期和超时保留原文件；成功来源是 `LoginSource::Qr`。这个入口供人的管理 CLI 使用，不能添加为 AI 业务工具的隐式登录动作。

`poll(Duration)` 返回 `Option<IncomingEvent>`，一次最多一个事件；等待上限五秒，零等待采用一毫秒 socket timeout。没有隐式心跳，宿主按需调用 `heartbeat()`。等注册/发送回执期间到达的事件保留在有界队列，供后续 poll；队列最多 32 项、原始 PB 最多 8 MiB，空间不足时在读取下一帧前停止等待并明确报错。单帧及解压后数据最多 4 MiB。

事件保留原始 PB、与传输序号无关的稳定摘要 key，以及服务端实际给出的 UIN、UID、群号、消息 ID、序号和时间；缺失字段保持 None，不从 QQ 数字账号猜 UID。`View` 提供有界正文和转义 HTML，图片/音频/卡片等标记为未展开，原 PB 由宿主保存为私有引用。库不会自动下载媒体或加载浏览器。原始事件和凭据不自动实现 Debug/Serialize。

文本发送分为三个本地步骤和一次显式提交：

```rust,ignore
let prepared = client.prepare_text(target, text, persisted_random)?; // 编码/签名，无发送
client.persist_session()?; // 将预留序号持久化
host_outbox.record_attempt(&prepared)?; // 宿主负责；本库没有 outbox
let response = client.transmit_text(prepared)?; // 消费一次，不重试
```

`SendTarget` 支持 SelfAccount、Private{uid}、Group{code}，可以序列化。SelfAccount 绑定当前真实 UID。PreparedText 绑定账号、连接、登录代次及预留序号，不可 Clone/Serialize；其他请求推进序号后须重新准备，不得提交旧包。只凭 result=0 不确认发送，必须有对应私聊/群聊回执序号。提交后的网络或解析错误保留为未知结果，不自动重发；宿主先记录尝试再提交。

签名模块按需加载，缓存注册与普通收包不会加载它。当前仍依赖固定第三方原生模块，未完成纯 Rust 签名算法。模块路径由宿主提供，库验证固定 SHA256 `9a39a5c6de727a3def1c6736f9c3fe8cb9147e5bee22cb77538511a4016b5566` 及执行映射。仅适配 Linux glibc x86_64，需 libdl、libstdc++.so.6、libgnutls.so.30；宿主最终可执行文件须链接 `-Wl,--export-dynamic`，导出 `qq_magic_napi_register` 与 `_ZTVN4vips7VOptionE` 两个兼容符号。库不能通过自己的 build.rs 替宿主设置最终二进制链接选项。load/sign 共享进程级 Mutex；Client/Signer 自然实现 Send，没有手动 unsafe Send。

离线测试保留独立 Go 文本编码/回执矢量、AES-GCM/P256/登录挑战、模拟扫码服务端与 Go TLV 矢量、TLS HTTP 长度/chunked 边界回归，并新增上游 TEA 固定矢量、私有 Session 兼容、稳定事件键、真实字段位置、Unicode/HTML 限额、TCP 分帧、发送绑定、扫码身份及私有二维码文件检查。测试不连接 QQ，不读取现有账号，也不加载真实签名模块。统一 Docker 测试由根工程运行。

新增富消息、联系人和媒体协商矢量由 `tests/referencegen.go` 在禁用网络、只读挂载上游源码缓存的 Go 容器生成；Go 只作为测试参考，不进入 Runtime。覆盖 @/引用/表情编码、媒体类型识别、图片/语音/视频协商、Highway 帧和视频中间摘要。新增上传/发送尚未用真实账号验收；协议实现及离线矢量通过不能代替平台在线确认。

协议来源和许可见 [NOTICE](NOTICE)、[LICENSE](LICENSE) 和 [TEA 参考许可](LICENSE-TEA-GPL-3.0)。

## 非扫码登录与显式 AppInfo（2026-10-31）

扫码不是默认恢复方法。`Client::authenticate(Auto)` 优先已有票据、随后只尝试一次 TempPwd；TokenExpired 为固定上游明确的 `140022015`，不等同于版本过低。已过期的凭据不会触发隐式 QR。

Runtime 增加仅供人的管理入口，不注册到 AI 的工具目录：

```sh
agent-runtime --account qq-main qq password-login /private/qq-login.json
agent-runtime --account qq-main qq import-login /private/qq-login-bundle.json
```

密码输入必须为 0600 的 JSON，例如 `{"uin":12345,"password":"由操作者在本机填写"}`。也接受 `password_md5`（32位十六进制）替代 password，二者互斥。文件直接由 Runtime 读取，既不经聊天/模型，也不写进 Session、日志、Outbox。SDK 用固定 NTQQ PasswordLogin 的 TEA 内层与 P-256/AES-GCM 外层；纯 Rust，无浏览器、Node 或外部登录服务。

CAPTCHA/设备确认不会被自动解答。结果为 `verification_required`，只返回 code/kind/私有 `management_file`。操作者在该文件查看 `verify_url`，在自己的浏览器处理；Runtime 不包含浏览器内核。CAPTCHA 由独立 operator client 在人的外部浏览器完成，通过私有控制连接提交 proof；core 不提供验证码 HTTP/HTML 服务，也不运行 JS 或浏览器。`Runtime::qq_password_login` 返回验证状态时保留原 `Client`、TCP、continuation 和仅内存密码摘要；同一 Runtime 的 `qq_password_resume` 可仅接收 `captcha:{ticket,randstr,session_id}`，无需重复 UIN 或密码。`session_id` 必须是原验证 URL 的原始 `sid`；旧 `aid` 仅作为此字段的输入别名，不得填写组件应用 ID。设备确认后在原 operator 会话继续；Runtime 对固定 QQ 设备确认端点查询一次，取得实际成功 token 后提交 NewDevice 原生登录。尚未完成时继续保留原挑战，不伪称上线。UnusualDevice 目前只暴露明确的人工验证状态与页面，不实现绕过或假定确认成功。续签状态绑定 UIN、UID、GUID、AppInfo 和5分钟期限，不保存密码/密码散列；旧 Session 仅在成功 Register 后替换。

`import-login` 接受 `{session,device}` 私有导出包；已保存相同设备的 alias 也可直接导入 Runtime Session JSON。候选先在临时目录完成原生登录和身份检查，成功后才替换旧登录态；现有 alias 的 UIN、UID、GUID 不能被导入悄悄改变。它不扫描桌面文件、不导入浏览器 Cookie，也不调用旧实验程序。旧的 `qq import DIRECTORY` 仍保留首次迁移完整实验记录的原行为。

AppInfo 统一到 `AppProfile`：默认仍为固定 `3.2.19-39038`，旧 Session JSON 不增加默认字段；`3.2.26-46494` 只有操作者在密码输入 `app_profile` 或显式 QR profile 参数选定后使用。QR、SSO、Register、Highway 共用同一 profile；失败不会修改旧设备或自动升级磁盘 Session。新版本的原生签名兼容尚未线上验证，不能由版本字符串或离线矢量推定成功。

测试参考来自固定 LagrangeGo `f7f8abfaa7f1` 的 `client/ntlogin.go`、`client/client.go`、`client/auth/app.go` 和原生 protobuf。`tests/auth-reference.go` 覆盖密码内层、CAPTCHA/header、3.2.26 metadata/T100；`tests/auth-full-reference_test.go` 注入隔离的上游副本，实际调用其完整 `buildPasswordLoginRequest`，生成两种 Linux profile 的首次/续验四份完整 TEA、protobuf、AES-GCM 报文。生成容器禁网、上游源码及依赖只读，无账号目录；Rust 比对全部原生帧（使用矢量 nonce）以及解密后的密码内层。测试参考程序不会进入 Runtime。离线通过不代表平台当前允许账号登录。


验证码参数按协议分开：URL `aid` 是 TencentCaptcha 的 `widget_app_id`，`login_appid` 是 QQ 应用 ID，URL `sid` 是原生验证的 `session_id`。固定 [Go 调用示例](https://github.com/LagrangeDev/LagrangeGo/blob/f7f8abfaa7f1/main.go) 将 `sid` 作为 `SubmitCaptcha` 第三参数；[Core 同协议实现](https://github.com/LagrangeDev/Lagrange.Core/blob/b7e07cf1022055feadbcea8b820f6a0ae27ae99c/Lagrange.Core/Internal/Logic/WtExchangeLogic.cs) 同样把 `sid` 送入原生 PasswordLogin。Go protobuf 字段名称 `Aid` 不代表组件应用 ID。operator proof 区分 `widget_app_id` 和 `session_id`，绑定原 challenge 指纹和期限；恢复前核对当前 URL，拒绝错会话和已解码的 `sid`。

`140022007` 在 Go 老枚举中名为 WrongCaptcha，但 [原生 NTLogin 枚举](https://github.com/LagrangeDev/Lagrange.Core/blob/b7e07cf1022055feadbcea8b820f6a0ae27ae99c/Lagrange.Core/Internal/Packets/Login/NTLogin.cs) 名为 `LOGIN_ERROR_STRICT`；Rust 归类为 `SecurityRestricted`，保留服务端安全提示，不据此指责人工答题错误。固定 Go 的 PasswordLogin 与 SubmitCaptcha 没有 Windows profile 要求，示例含 Linux3.2.19 分支；这只是实现证据，不能当作腾讯对旧版本或第三方实现当前登录成功的保证。续验复用同一 ECDH ExchangeKey、KeySig、Cookie、GUID 以及同一个 TCP。SDK continuation 的连接绑定只存在于内存，序列化后不再声称属于原连接：默认 `Client::password_login` 和 `Runtime::qq_password_resume` 拒绝跨连接续验。明确需要跨进程时才使用 `Client::password_login_from_disk` / `Runtime::qq_password_resume_from_disk`，并要求私有密码或摘要；结果标记 `continuity=reconnected_from_disk`，不能当作原生同连接续验。


operator 所有者可调用 `qq_password_session_active()`、`qq_password_expire()` 和 `qq_password_cancel()` 管理待验证连接。过期、取消与控制连接 EOF 清理关闭待验证 TCP、清除其挑战文件，保留之前的 Session；Runtime 退出会丢弃内存连接，CLI 留下的有效私有 continuation 只能走显式 disk fallback。新的同账号普通登录不能抢占仍在等待人的连接。合成 TCP 服务端回归实际读取连续两次 SSO PasswordLogin，确认没有新 KeyExchange、同一连接复用原 exchange，且按新的挑战更新 Cookie/sid；仅测试编译路径使用合成签名器，不加载真实模块，不连接 QQ。


`140022017` 以 `SecBeatRejected` 返回，保留原始码和已有服务端安全说明；没有说明时不猜测原因。固定 Go 的 [loginstate/enum.go](https://github.com/LagrangeDev/LagrangeGo/blob/f7f8abfaa7f1/client/packets/wtlogin/loginstate/enum.go) 没有定义此码，其 `Name()` 同样返回 Unknown；固定 Core 的 [NTLogin.cs](https://github.com/LagrangeDev/Lagrange.Core/blob/b7e07cf1022055feadbcea8b820f6a0ae27ae99c/Lagrange.Core/Internal/Packets/Login/NTLogin.cs) 将它命名为 `LOGIN_ERROR_SEC_BEAT`。这是开源实现的协议映射，不是腾讯对具体失败原因的公开说明；不能据此判定验证码错误、账号限制、签名错误或缺失心跳。

2026-10-31 静态对照发现，我们当前匹配固定 Go 的路径，与另一份固定 Core 存在下列差异。这些是待定位的兼容差异，未证明任何一项导致当前实号拒绝；不据此自动改值、换 profile 或重试账号。

| 项目 | 固定 Go / 当前 Rust | Core b7e07cf10220 |
|---|---|---|
| NTLogin client field3 | Linux NTLoginType=1 | Linux Platform=7 |
| NTLogin app field1 | device.kernel_version | AppInfo.Kernel，Linux配置为 `Linux` |
| NTLogin额外身份字段 | 无 app field5/header field7 | Qua 与 SDKVersion=1 |
| SSO12 GUID | 16 bytes | 32位小写十六进制字符串 |
| SsoKeyExchange签名 | Go名单明确注释掉，Rust未签 | 签名白名单包含此命令 |
| 登录前连接保活 | 无主动 Heartbeat.Alive | 建立连接即每2秒 Heartbeat.Alive |
| 登录后SSO心跳 | 注册后270秒 | Online后270秒，首次立即 |

身份字段依据 [Core NTLoginCommon](https://github.com/LagrangeDev/Lagrange.Core/blob/b7e07cf1022055feadbcea8b820f6a0ae27ae99c/Lagrange.Core/Internal/Services/Login/NTLoginCommon.cs)、[BotAppInfo](https://github.com/LagrangeDev/Lagrange.Core/blob/b7e07cf1022055feadbcea8b820f6a0ae27ae99c/Lagrange.Core/Common/BotAppInfo.cs)；外层 GUID 依据 [SsoPacker](https://github.com/LagrangeDev/Lagrange.Core/blob/b7e07cf1022055feadbcea8b820f6a0ae27ae99c/Lagrange.Core/Internal/Packets/Struct/SsoPacker.cs)；签名白名单依据 [BotSignProvider](https://github.com/LagrangeDev/Lagrange.Core/blob/b7e07cf1022055feadbcea8b820f6a0ae27ae99c/Lagrange.Core/Common/BotSignProvider.cs)；保活阶段依据 [WtExchangeLogic](https://github.com/LagrangeDev/Lagrange.Core/blob/b7e07cf1022055feadbcea8b820f6a0ae27ae99c/Lagrange.Core/Internal/Logic/WtExchangeLogic.cs)。`Heartbeat.Alive` 是连接保活，`SsoHeartBeat` 是上线后的状态心跳，均不能只凭 `SEC_BEAT` 名称视为其同义词。
