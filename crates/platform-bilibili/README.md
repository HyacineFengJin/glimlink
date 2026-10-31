# platform-bilibili

同步 Bilibili 适配器，以 Rust HTTP/TLS 显式扫码登录，读取账号、收藏夹、元数据、分 P、播放资源和字幕索引，支持已登录账号评论公开视频、正向点赞和关注视频UP，以及动态/收藏/投币/弹幕语义操作。无需浏览器、页面渲染或 JS 引擎；没有登出或模型调用。

```rust,no_run
use platform_bilibili::{Client, Config, VideoId};
let id: VideoId = "BV17x411w7KC".parse()?;
let mut client = Client::new(Config::default())?;
let video = client.video_info(&id)?;
let playback = client.playback(&video, 1)?; // 分 P 从 1 开始
# Ok::<(), platform_bilibili::Error>(())
```

- `Config.cookie` 只能由调用方显式提供；不读取环境、HOME、浏览器或本机账号。Cookie 仅发给 `api.bilibili.com`，不发往媒体 CDN。
- `AuthClient` 使用 `passport.bilibili.com` 的 Web QR generate/poll 接口，单次轮询、不快于2秒、180秒到期后本地终止。等待与已扫未确认均不能取得 Session；成功时只接受域为 `bilibili.com`、路径为 `/` 的必要 Set-Cookie，不跟随游戏 SSO URL、重定向、自动刷新或重新申请二维码。
- `LoginAttempt` / `Session` 不公开实现 Serialize，Debug 隐去登录材料。显式 save/load 使用私有0600文件、原子替换与 fsync，读取拒绝符号链接、FIFO、宽权限和超限文件。`Session::cookie_header` 仅供嵌入方私下配置客户端，不能输出进模型资源。
- `current_user` 通过认证 nav 核实 UID/昵称等白名单字段；`favorite_folders(mid)` 只读收藏夹元数据。调用方应将成功 Session 的稳定 UID 与账号别名绑定，不能将公开视频读取成功当作账号登录成功。
- view 元数据返回 aid、bvid、标题、简介、作者及全部分 P；BV/AV 不靠本地转换算法猜测，使用实际 view 响应解析对应关系。
- WBI 从 nav 的 `wbi_img` 取 key；未登录 nav 的 `-101` 仅在该特定入口允许。key 短时缓存并按需刷新；签名在 Rust 中计算。遇 `v_voucher`、风控错误或 HTTP 412/429 明确失败，不执行验证码绕过。
- 默认请求 360P，选择可用的最低分辨率 H.264 视频与最低带宽 AAC；DASH 保持两个独立文件。progressive 保留全部片段，不伪称已合并。服务端可能不提供指定编码或完整资源，此时返回失败或显式 `preview_only`。
- 字幕索引保留 `requires_login`；播放器元数据失败保留 `subtitle_error`。没有返回字幕不会被当成已取得转写。`subtitle_lines` 可按需读取真实字幕 JSON。
- API JSON 默认限制 2 MiB（硬上限 8 MiB），带连接/读取/总超时。下载用 32 KiB 缓冲区流式写入显式 Writer/路径，单文件上限由调用方和 Config 共同控制。
- HTTPS URL 和每一跳重定向均校验域名。API 限 `api.bilibili.com`；媒体限 `bilivideo.com` / `bilivideo.cn` / `hdslb.com` 域及其子域，仅端口 443。HTTP、IP、localhost、用户信息、外部域名及非标准端口镜像被拒绝。原地址不合规时只选择服务端明确提供的合规备用地址。
- 错误不回显 URL、响应正文或 Cookie；资源的 Debug 隐去 URL。资源 serde 数据仍含短期私有 URL，调用方不应将其直接打印进公共日志。
- 路径下载使用 `create_new`，不覆盖既有文件；失败时删除该新建文件。Writer 下载若中途失败，调用方负责丢弃已写入前缀。

隔离构建/测试应经项目根 `scripts/isolate.sh`；示例 `read_video` 默认无 Cookie、不下载，只有 `--download-dir` 显式指定目录才下载。请在默认禁网测试通过后使用 `run online` 做只读公共视频验证。未实测的运行范围不能视为已可用。

## 视频上下文与正向互动

`video_tags`读取实际普通/话题/BGM标签，`uploader_info`读取公开card的身份、签名、等级、认证及计数，头像和标签封面均为未下载定位器。简介沿用VideoInfo.description；标签不会从简介中的井号文字猜出，关注总数也不从当前账号关系布尔值转换而来。

正向点赞/关注通过`prepare_video_like`或`prepare_uploader_follow`核对真实视频、当前账号、作者与状态，返回不可Clone/Serialize的`PreparedEngagement`。宿主为操作持久记录尝试后，`submit_prepared_engagement`只发一次固定POST，like=1或act=1，无重定向或自动重试。`needed=false`时宿主应记录无写入终态。已有悄悄/普通/互相关注均保留；拉黑、未知关系与自己关注自己均不转为普通关注。关系mid=0且attribute=0仅作为未建立关系哨兵，其他MID仍严格核对，实际fid始终取冻结视频作者。

`engagement_state(video,action)`独立GET核对，无需伪造成功回执；`verify_engagement`核对有回执的操作。点赞状态来自近期has/like，返回0不表示从未点赞。关注以本人关系attribute判断，不能看总粉丝数增加猜测；服务端拒绝或未知结果保留，不盲重发。协议依据：[视频互动](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/video/action.md)、[用户关系](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/user/relation.md)、[标签](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/video/tags.md)、[公开用户资料](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/user/info.md)。

## 授权评论

`prepare_own_video_comment` 读取真实账号和视频作者，要求本人视频并绑定精确正文和登录态；准备对象不公开序列化。`submit_prepared_comment` 消费该对象，仅发送一次评论 POST，不跟随跳转或自动重试。宿主先持久记录操作尝试，未知结果不得自动补发。`comment_by_id` 按回执 ID 只读核对作者、正文和评论状态。根 Runtime 将这些内部阶段合成一个动态工具调用，不需要逐步人工确认；已完成真实发送及读回，详见根验证记录。

表情通过`comment_emotes()`读取实际`packages[].emote[]`目录，返回原生文本token、可选ID和CDN地址，过滤明确标为`no_access=true`的条目；颜文字保持文本且不伪造图片URL。发评正文保留精确token。`VideoComment.emote`保留服务端实际解析出的映射，不能仅凭正文包含`[doge]`就断言发出了原生表情。

`comment_emote_packages()`保留账号评论面板中的包结构：包ID/名称、CDN图标、原始type/attr、尺寸提示，以及条目的package_id、type、meta.size/alias/suggest和flags.added/no_access/preview/unlocked白名单。未知字段、商城item_url、UID和商品/资源标识不投影。该方法保留禁用条目及其标志，旧扁平接口只过滤明确no_access=true；unlocked=false的实际含义未确定，不据此禁用。所有可选状态区分缺失和false，旧评论回执缺少新增元数据时仍可读取。

包`added=true`只说明当前账号已添加，`no_access=true`说明服务端禁用；目录出现、type=3或已添加都不等于已付费购买。type和attr保留实际值，不为未知类型（如8）猜测含义。meta.size=2表示大表情，不能据此判断是否动画；动画需要继续读取实际媒体。此入口是认证后的user/panel，不是商城所有包列表或交易记录。

`prepare_own_video_image_upload(video, bytes)`只读核对本人视频及账号，限制PNG/JPEG最多2 MiB、单边4096、总像素8 MP；本模块仅检查格式头和尺寸，不新增完整图片解码器。返回的`PreparedImageUpload`不可Clone或公开序列化。宿主须为上传本身单独持久标记尝试，再调用`submit_prepared_image_upload`向固定`/x/dynamic/feed/draw/upload_bfs`发送一次multipart请求，不跟随跳转或自动重试。私有上传回执转换为`UploadedCommentImage`，由`prepare_own_video_comment_with_pictures`核对当前作者、CDN和尺寸后写入评论`pictures`字段。上传接受不等于评论已发布；未知上传不能自动重传，URL和凭据不应进入公共资源索引。

`prepare_own_video_reply(video,root,parent,text,pictures)`核对视频、根评论及父评论均属于当前账号；直接回复根评论时`parent=root`。`reply_page`单页读取最多20条，拒绝跨oid/root的数据；`reply_by_id`只在首20条查找，缺失不代表不存在，也不会触发写重试。`VideoComment`保留root、parent、图片和表情白名单，新字段的serde默认值可读取旧回执。

字段依据为作者源码 [评论图片与回复参数](https://github.com/inorilzy/bilibili-api/blob/main/bilibili_api/comment.py)、[图片上传字段](https://github.com/inorilzy/bilibili-api/blob/main/bilibili_api/dynamic.py)及[原生表情目录协议](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/emoji/list.md)。SDK离线回归不证明当前服务端已接受图片/回复，实号结果由根Runtime单独验收。

## 只读评论区与评论媒体

`comment_page(aid, CommentOrder::{Hot,Time}, cursor, limit)`通过现行WBI`/x/v2/reply/wbi/main`读取一页，热度mode=3、时间mode=2，透传服务端`next_offset`而不猜页码。`items`普通评论、`pinned`置顶附加条目、`hots`热评附加条目分开，跨组按rpid去重并合并来源标记；置顶不占普通页的1..20条预算。服务端普通页超出请求限额时，`truncated=true`且不提供可直接推进的`next_cursor`，另保留`server_next_cursor`与说明，避免跳过未返回条目。缺少`is_end`时`has_more=None`，挑战、缺失结构和未经证实的null空列表明确报错，不伪装成空评论成功。

`comment_replies(aid, root, page, limit)`读取独立楼中楼分页，并返回富信息根评论及回复；这是任意可见作者的读取接口，与写入预检的本人作者限制分离。页大小1..20，不自动翻页；缺失完整分页、服务端夹取页码或输出截断时不伪造后继页。

`CommentRead`保留实际作者ID/昵称、正文、时间、评论获赞数、回复数、UP主点赞标志、原始用户action、root/parent及状态。缺失计数保持None；匿名接口也可能返回action=0，因此不能据匿名0断言某个账号未点赞。仅一层最多3条预览，剩余回复显式标识。媒体保留为`media`定位器：实际图片或表情CDN地址、尺寸、原生token/包ID/type和文件名格式提示，默认不下载。GIF后缀和大表情type/size均不证明动画，`animated=None`，须按需拉取后验签/解码。完整地址应由宿主保存为私有asset，再给模型可按需读取的引用；本模块不拉图、动画帧或音视频。

动态表情可以同时提供`url`静态封面、`gif_url`和`webp_url`。三种非空地址分别保留且按规范化URL去重，不能仅取PNG封面就声称已读动画。当前服务端也可能连续多页重复同一`next_offset`但返回不同评论；`cursor_reused=true`只作事实标记，不因此丢掉可用续页。调用方须按rpid去重，空页/is_end/无新增时停止并限制页数，不能无限循环。真实观察见根验证记录。

实现依据：[当前根评论游标与楼中楼协议](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/comment/list.md)、[评论计数、作者和富内容对象](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/comment/readme.md)及[作者SDK的WBI懒加载参数](https://github.com/inorilzy/bilibili-api/blob/main/bilibili_api/comment.py)。这些来源不代表服务端稳定性承诺，真实GET验收另由根Runtime记录。

## UP空间动态工具

`uploader_dynamics(mid, offset, limit)`只向现行`/x/polymer/web-dynamic/v1/feed/space`发送一次有界GET，复用已有Cookie；不自动翻页、轮询、登录、标已读或转向旧接口，不添加伪造指纹。返回实际动态ID/type、作者与字符串/数值时间、原始文字及富文本节点、必要提及、赞评转快照和viewer现有点赞状态。置顶单独标记，不能把第一条置顶旧稿当作最新发布时间。该模块是给Runtime/AI调用的现成能力，不要求AI自行发现接口或拼参数。

支持文字、图片动态、视频/专栏引用和现代opus摘要；无正文的图片动态保持`text=None`。说明视频内容的archive.desc与动态自己的正文分开，desc.has_more透传为text_truncated，不能把摘录当全文。orig最多展开一层，原作者独立保留；更深、已删除或缺失的原动态保留未展开标识和可取得的ID引用，不归为当前UP原创。未知major/additional保留实际类型与unexpanded标记，不伪造正文。

图片、封面和表情的PNG/GIF/WebP变体只作为私有媒体定位器返回，不下载或判断动画；宿主应换成按需读取的asset引用。评论关联仅采用basic.comment_id_str/comment_type，不从视频ID或动态类型猜评论区type。分页透传has_more和offset；缺失结束状态保持未知，截断不给可直接跳过遗漏条目的next_offset，空页止步，重复opaque offset提示按动态ID去重并限制读取。不用服务端total=0等可能矛盾的字段猜总动态数。

协议与字段参考：[现行用户空间动态接口](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/dynamic/space.md)。实际账号最小验收由根Runtime统一执行；SDK合成测试不证明服务端或任意动态类型都已实测。

## 本人已发布视频

`my_publications(page, page_size)` 先从认证nav确认当前账号UID，再以该UID签名请求空间已发布视频列表 `/x/space/wbi/arc/search`，最新发布优先；没有用户提供的替代UID参数，不访问创作中心草稿、不发布或修改稿件。每页1..20条，仅单页读取。返回服务端总数、实际页码/大小、是否还有后续及下一页；不把超限响应截断后跳至下一页。

列表保留aid/BV号、标题、简介、发布时间、原始时长与可解析的秒数、封面引用。封面不自动下载。合作视频保留实际主作者和合作标识，不能因其主作者UID不同就认为账号串号。响应挑战标记、缺失结构或不一致空页明确失败，不能伪装成“没有投稿”。这是空间可见的发布列表，不是创作中心完整资产清单；长期变化、不可见或下架稿件不在此声明范围内。

实现依据：[投稿空间协议主源](https://github.com/bilibili-plugins/bilibili-api-collect/blob/master/docs/user/space.md)、[Go客户端作者的WBI请求及字段](https://github.com/PastGone/bilibili_go/blob/master/user.go)。旧 `/x/space/arc/search` 已废弃，本实现不自动切换接口、伪造指纹或重试挑战。实际账号验证由根Runtime执行。

`uploader_videos(mid,page,page_size)`独立读取指定UP当前会话可见的已发布空间视频：先通过公开用户名片核对请求UID并取得昵称，再读取同一WBI投稿端点。返回`UploaderVideoPage.uploader_mid/uploader_name`，不把该UP包装成当前登录账号的`AccountInfo`。`my_publications`仍只从认证nav取得当前UID，不能传入替代账号。

两种入口共用有界分页和视频投影，每页1..20条；公开UP列表同样不自动翻页或下载封面。合作稿保留服务端的主作者UID、昵称和合作标志，不因主作者不同就错误丢掉空间中的合作作品。接口拒绝和挑战直接失败，不自动换API；它不是草稿、完整历史资产或全部匿名可见内容的清单。

## 点赞、回复和 @ 通知

`notification_unread()` 读取通知未读计数；缺失类别保持 `None`，不会补成0。`notifications(kind, cursor, limit)` 对 `Reply`、`At`、`Like` 执行单页GET，输出上限1..20条，不自动翻页。仍只使用 `api.bilibili.com`，不增加Cookie域。

输出按白名单投影通知ID、用户、实际时间、业务类型、回复/被回复上下文以及关联链接和图片引用。图片只作引用，未下载、未提交模型。响应未知结构明确失败，不把鉴权/风控或协议变化伪装成空通知。接口不发送已读确认、清空未读、删除或其他写操作；服务端是否隐式更新未读状态尚待实际观测，不能据GET方法保证不变。

Like保留latest/total分区标签，首次输出优先latest，同ID不重复返回。Cursor分页只返回total区，防止latest重复；输出截断时，`next_cursor`来自实际已返回的total条目，另保留`server_cursor`。latest挤满预算且无法无漏项继续时，明确返回分页局限和空`next_cursor`，不声称已经读取全部通知。

通知协议依据：[通知计数与回复数据主源](https://github.com/bilibili-plugins/bilibili-api-collect/blob/master/docs/message/msg.md)、[bpi-rs只读通知实现](https://github.com/Yuelioi/bpi-rs/blob/main/src/message/client.rs)、[B站清理工具作者源码中的通知GET与like分区读取](https://greasyfork.org/en/scripts/542115-b%E7%AB%99%E6%B8%85%E7%90%86%E5%B7%A5%E5%85%B7/code)。这里只参考读取形状，没有实现或运行该参考工具的删除功能。实号结果由根Runtime统一验收。

协议参考：[yt-dlp Bilibili 提取器作者源码](https://github.com/yt-dlp/yt-dlp/blob/master/yt_dlp/extractor/bilibili.py)、[WBI 贡献者存档](https://github.com/pskdje/bilibili-API-collect/blob/master/docs/misc/sign/wbi.md)、[view 元数据](https://github.com/pskdje/bilibili-API-collect/blob/master/docs/video/info.md)、[播放器与字幕](https://github.com/pskdje/bilibili-API-collect/blob/master/docs/video/player.md)。存档资料不是当前服务器兼容承诺；真实验证单独记录。

扫码实现核对了 [bpi-rs 作者的 QR 数据与协议合约源码](https://docs.rs/bpi-rs/latest/src/bpi_rs/login/login_action/qr.rs.html)、[QR 协议记录](https://github.com/bilibili-plugins/bilibili-api-collect/blob/master/docs/login/login_action/QR.md) 与 [nav 账号信息](https://github.com/bilibili-plugins/bilibili-api-collect/blob/master/docs/login/login_info.md)。2026-10-29官方 generate 实际返回 `account.bilibili.com/h5/account-h5/auth/scan-web`，实现精确允许此新二维码路径及旧路径；HTTP请求入口仍仅限 Passport 固定端点。新增状态/凭据/投影/路径测试已在禁网Docker通过，实际账号验收见根记录。

## 公开评论与动态/收藏/弹幕

`prepare_video_comment_with_pictures`、`prepare_video_reply`、`prepare_video_image_upload` 支持账号有权限的普通公开视频。`owner_mid` 始终是视频作者；`actor_mid` 才是发言/上传账号。旧 own-only 准备方法仍保留原约束；旧日记中没有 actor_mid 的本人视频目标按原 owner_mid 解释。回复准备验证实际根/父关系；不在首20条里的父回复由一次原生 WBI seek 查找，不能误换成顶层评论。

`FeatureCommand` / `PreparedFeature` 提供明确状态的收藏/取消收藏、投币、指定时间与模式的弹幕、文字/图片动态发布、动态点赞/取消赞、动态精确回复。`favorite_contents`、`video_folders`、`dynamic_detail`、`dynamic_discussion` 和 `danmaku_segment` 提供实际数据。弹幕通过 bounded protobuf 解码，不包含作者哈希，也不执行代码。动态图片按需通过 `prepare_dynamic_image` / `submit_dynamic_image` 上传，账号绑定独立于视频。

宿主的 `social_bilibili` 将收藏夹、视频、动态与回复封装为账号绑定 opaque refs，续页、图片上传与回执读取都在 Runtime 内完成。每个写入工作流在第一个 POST 前持久记账；未知结果不会自动重放。收藏/点赞已有目标状态不发 POST，投币不隐式点赞。新功能已通过 Docker 合成测试；本轮没有向真实平台发布内容或消耗硬币，不能将协议测试表述为真实写入验收。

协议依据：[动态发布](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/dynamic/publish.md)、[动态操作](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/dynamic/action.md)、[视频互动](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/video/action.md)、[收藏夹](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/fav/list.md)、[弹幕操作](https://github.com/pskdje/bilibili-API-collect/blob/main/docs/danmaku/action.md)。
