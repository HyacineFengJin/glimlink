# platform-x

同步、只读的 X 网页会话 SDK，面向内存有限的 Runtime。纯 Rust HTTP、JSON/HTML/脚本数据提取及交易签名；不嵌入或执行 V8、Blink、Chromium、Node、Bun、Python 或页面 JavaScript。不使用付费开发者 API key。

## 接口

```rust,ignore
let session = platform_x::Session::from_private_input(private_cookie_input)?;
let mut client = platform_x::Client::new(platform_x::Config::default(), session)?;
let viewer = client.current_user()?;
let author = client.user_by_screen_name("handle")?;
let by_id = client.user_by_id(&author.id)?;
let post = client.tweet("1234567890123456789")?;
let page = client.user_timeline(&author.id, None, 20)?;
```

`Session::new(auth_token, ct0)` 或 `Session::from_private_input(&str)` 接受最小 JSON `{"auth_token":"...","ct0":"..."}` 或完整 Cookie header。只保留这两个字段；拒绝重复、注入字符和超限输入。Session 可 Clone、serde；Debug 固定脱敏。序列化只能用于调用方的私密存储，不应放进模型上下文。

`Config` 字段为 `request_timeout`、`connect_timeout`、`max_response_bytes`、`language`。默认请求 25 秒、连接 8 秒、单响应解压后 8 MiB；输入配置也有硬上限。没有自定义 API/CDN host 或通用 HTTP 接口。

`current_user()` 先通过认证 GET `account/settings.json` 取得 viewer 的 screen_name，再读取对应用户的真实 rest_id；不把输入 Cookie 中的 twid、用户自己填写的 id 或登录成功文本当身份核验。`user_by_id()` 在新版中先走只读 ID→screen_name 查询，再精确核对完整用户响应的 rest_id。

## 数据与分页

- `User` 保留字符串 id、昵称、screen_name、简介、时间、计数、公开状态和头像定位器。
- `Tweet` 保留 exact id、作者、长文优先正文、时间、互动数、关联帖子 id、图片/视频/GIF 元数据和 `unexpanded` 标记。所有未知计数是 Option，不补零；来源为毫秒的时间表示为 `unix_ms:<value>`，不冒充原始日期格式。
- `Media`/`VideoVariant` 只有 HTTPS CDN 定位器、尺寸、alt text、视频变体和码率。`animated_gif` 是平台给出的类型，不代表已下载或验证动画帧。仅保留 pbs.twimg.com / video.twimg.com 媒体 URL。
- 每次 `user_timeline` 只读一页，limit 为 1..20；items 与 pinned 分开，按帖子 id 去重。普通页被截断时 `truncated=true`、`next_cursor=None`，仍保存 `server_next_cursor` 以免误称无损续页。重复 token 且有内容时保留 token，并明确要求调用方按 id 去重、遇空页/无新增项停止。空页没有可执行 next_cursor；未知分页是 `has_more=None`。
- 新版实际 operation 是 `UserOriginalsTimeline`，语义为原创 Posts。返回 `timeline_kind="original_posts"`，不能声称包含所有回复、转推或全部账号历史。旧页面的 `UserTweets` 返回 `timeline_kind="posts"`。

正文、URL、昵称与所有平台文本都属于不可信外部内容。SDK 不把它们当指令执行；Runtime 应转成私密 target/media 引用，并继续执行自己的账号绑定与权限规则。

## 当前网页协议

2026-10-31 公开页面已使用 `/x-web/x-web/entry-client-...js`，不再只有旧 webpack main/ondemand 布局。SDK 从页面及其少量脚本数据定位当前签名模块和四个只读 persisted query；不写死本轮 query ID，不用 eval。

新版固定请求为 `https://api.x.com/graphql/{id}/{name}`，提供 variables 中的 Relay provider 布尔值；具体默认值按当前公开前端定义，而不是全部猜成 false。旧布局独立使用 `https://x.com/i/api/graphql/...`。本轮匿名首页为新版、认证首页仍为 legacy，按实际页面识别；两者是当前并存的布局。没有在未知新布局时自动退回过期硬编码 operation ID。公共脚本读取只到 abs.twimg.com 的两个固定路径树，不携带账号 Cookie；单 client 最多读取 20 个脚本、累计 bootstrap 文本 16 MiB。

legacy 用户名变量是 `screen_name`，modern 为 `screenName`；两条请求共享受测的分支构造函数。legacy flags 显式按 defaultConfig→user overrides 解析。无法确认某个 operation 的 flag 只阻断该 operation，不阻断已完整确认的身份查询。`rweb_conversational_replies_downvote_enabled`、`responsive_web_made_on_x_label_enabled` 明确设置 false，表示 SDK 不请求这两项可选 UI 投影，绝非推断它们是账号或服务端默认值；其他未知 flag 仍不猜值。

主来源（仅以数据方式读取）：

- [X 首页](https://x.com/)
- [本轮新版 startup](https://abs.twimg.com/x-web/x-web/assets/startup-BjvP8TqV.js)：api.x.com/graphql、只读 userIdRedirectQuery、provider 默认值。
- [UserByScreenName](https://abs.twimg.com/x-web/x-web/assets/UserByScreenName.graphql-sECoe8QD.js)。
- [完整单帖 TweetResultByRestId](https://abs.twimg.com/x-web/x-web/assets/retweet-Jao3BH3G.js)：与只取媒体的 TweetResultById 区分。
- [UserOriginalsTimeline](https://abs.twimg.com/x-web/x-web/assets/user-posts-BNcxeYoa.js)、[timeline normalization](https://abs.twimg.com/x-web/x-web/assets/timeline_normalization.graphql-C7N5VoOm.js)。

网页协议可能变化；无法确认 operation、签名索引、必需 provider 或结果形状时返回 `ProtocolChanged`。不执行网页迁移表单、验证码、指纹程序、密码登录或自动重新认证。

## 按需媒体

`download_media(url, destination, max_bytes)` 只有调用方显式调用才拉资源。精确 HTTPS 媒体 host 白名单，无 Cookie/Authorization，无自动跳转；流式写入、检查长度与实际累计字节，最多 256 MiB。文件以 0600、create_new 创建，不覆写现有文件，失败清理本次新文件。返回 `DownloadInfo { mime_type, bytes }`，不把 CDN MIME 当格式验签结果；解码、帧选择、视觉/音频输入仍由 Runtime 媒体层负责。

所有网络操作都是 HTTP GET；没有发帖、点赞、关注、DM 或任何 mutation 方法。认证、验证挑战、限流、权限、404、重定向、网络和协议错误使用静态枚举与 `Error::code()`，不会回显响应正文、Cookie、请求头或账号 URL。裸 HTTP 403 为 `request_denied`，只有明确验证跳转/错误码才判为验证要求。`last_request()` 仅保留固定阶段、HTTP 状态、响应类型及最多八个数字错误码；每次请求清空旧诊断，不保留原文。仅首页的同站固定 `/home` 跳转可跟一次；API/CDN 重定向明确拒绝。

## 验证与许可

Docker 无网络、无账号挂载测试：18 passed，另有 2 个默认 ignored 检查（公开文档 corpus、私密候选阶段诊断）。本轮另显式运行 corpus 检查，使用公开网页/JS缓存离线提取，1 passed；没有执行 JavaScript。涵盖独立 Python 参考交易签名 golden、现代富媒体/作者/wrapper/计数、pin/module/分页、输入注入防护、host 白名单、静态错误、legacy/modern 变量名、未请求 operation 隔离及不含凭据的有界诊断。

2026-10-31 经显式授权，在 Docker 中由程序读取用户刚提交的私密候选，在 15 分钟续验窗口内做针对性 GET 诊断。修复上述 legacy 两处问题后，page/main/sign/transaction/operation/settings/UserByScreenName/用户解析阶段均成功；该诊断最终 1 passed，只输出固定阶段、字节数、bool 结构特征与 schema keys，没有输出 Cookie、认证 HTML、响应正文、头、查询参数、UID 或昵称。未重新登录、切账号或执行社交写动作。

随后由 Runtime 正式核验并保存同一会话，通过五次共享语义工具调用读取当前资料、自己的发帖列表、公开作者资料、三条作者帖子及一个返回引用指向的完整帖子，正文和互动计数读取成功。扩大分页初次失败的原始状态为 `verification_required`；新增诊断定位到重复资料查询的 HTML HTTP 403，不能证明账号受限。Runtime 改为直接使用已验证的作者引用后，在新进程中连续读到两页各 20 条帖子、无跨页重复，并通过共享 `media.read` 按需拉取和解码一张图片；没有重新登录、替换身份或执行社交写动作。完整集成边界见 [验证记录](../../docs/validation.md)。

交易算法是有来源的 Rust 翻译/适配，非本项目原创协议。Twikit 与独立 XClientTransaction 的固定提交、版权和各自 MIT 原文分别保存在 [NOTICE](NOTICE)、[LICENSE-TWIKIT](LICENSE-TWIKIT)、[LICENSE-XCLIENTTRANSACTION](LICENSE-XCLIENTTRANSACTION)。
