# platform-tieba

Rust 1.90 的贴吧原生 HTTP 模块。帖子读取使用 protobuf；个人登录使用显式调用的百度扫码接口。不包含浏览器、DOM、JS 运行时、Python 或独立桌面客户端，不发现或导入宿主账号。

## 接口

```rust,ignore
let client = Client::new(Config::default())?;
let thread: ThreadId = "https://tieba.baidu.com/p/8093410706".parse()?;
let fetched = client.thread_page(&thread, 1)?;
let floor = &fetched.data.posts[0];
let comments = client.comments(&thread, floor.id, 1)?;
```

- `thread_page(&ThreadId, page: u32) -> Result<Fetched<ThreadPage>>`：题目、贴吧、作者、楼层、时间、文本及富媒体片段、分页。
- `comments(&ThreadId, parent_post_id: u64, page: u32) -> Result<Fetched<CommentPage>>`：独立读取楼中楼、发布者、回复关系与时间。
- `download_to_writer(&MediaResource, &mut dyn Write, max_bytes)` / `download_to_path(...)`：显式按需下载，后者只创建新文件，失败删除部分内容。

页码从 1 开始，限制 100000。帖子每次请求 30 楼，响应最多接纳 100 楼、512 个用户、每楼 512 个内容片段、100 条内嵌楼中楼。服务器可能不下发总楼层计数，因此 `Page.total_count` 为 `Option<u64>`，`None` 不等于 0。

`Fragment` 保留 Text / Link / Mention / Media / Unsupported，不把图片、语音或未知组件静默丢成纯文本。`Fragment::Media` 的 `MediaResource` 包含媒体类型、URL、已知尺寸/时长；未知资源格式明确保持 Unsupported，内容字段仍可从凭据剔除的来源中展开。语音 MD5 等尚未实现专门解析器，不能宣称音视频已能下载或理解。

媒体 URL 的 Debug 隐藏，serde 默认也跳过 URL。Runtime 应从 Rust 字段提取到私有 locator，给 AI 不透明引用；不要假设 `serde_json::to_value(MediaResource)` 能保存私有 URL。图片、语音和视频理解交给共享媒体/引擎模块，此 crate 不推断模型已经看过内容。

## 账号与来源边界

显式 `Config.bduss` 会写入请求的 CommonReq.BDUSS，仅 API 域接收 BDUSS Cookie。媒体请求从不携带 BDUSS。`AuthClient` 另提供个人扫码、正式 Cookie 交换与账号身份核验；不能将公开帖子读取成功当作账号认证成功。

扫码使用百度 Passport 的 getqrcode / qrcode / unicast / qrbdusslogin。临时确认值不作为正式 BDUSS；交换前用 `finish_login_persisted` 将消费标记原子落盘，未知结果不重发。若正式 Cookie 已取得而身份查询失败，`resume_exchanged_login` 仅恢复身份读取，不再轮询或交换扫码票据。

身份核验只允许实际观测到的子站同步：贴吧 json_userinfo → Passport 固定 auth 路径（return_type=3、u回指原接口，jump仅允许空值）→ 贴吧回调。回调下发贴吧子站STOKEN后返回首页跳转；客户端检查此跳转但不获取首页，直接重读固定账号JSON。总计最多4个HTTP请求，每跳按域和路径重算 Cookie。Passport 的 STOKEN 不发送给贴吧，贴吧自己下发的 Cookie 随 Session 私有保存。其他扫码端点不跟随跳转，错误 URL、非零结果和挑战直接失败。

Attempt / Session 不公开实现 Serialize，Debug 不显示凭据；save/load 使用0600私有文件，拒绝宽权限、符号链接和超限输入。`me(&mut Session)` 验证稳定身份一致后才更新 Cookie，调用方应保存更新后的 Session 并绑定自己的账号别名。

上游 `User.proto` 存在 BDUSS 字段，因此 **完整服务器响应不能直接进入公共资源缓存**。`Fetched.raw_source` 是凭据剔除后的 protobuf 来源，`source_redacted=true`：只保留已知帖子/分页/贴吧/楼中楼结构，User 仅保留 id/name/display_name/portrait，丢弃其余 User 字段和未知服务端顶层字段。PbContent 的用户富文本字段保留，可用于新增内容类型。原始字节数另外记录为 `response_bytes`；`raw_source.len()` 不是原响应字节数。

该来源不是完整原始响应；未来确需额外服务端业务字段时，应核查固定协议后纳入白名单，不能笼统暴露未知凭据字段。

## 限额与传输

默认响应/解压后正文最大 4 MiB，下载最大 128 MiB，请求全局 25 秒、连接 8 秒、单次读取 15 秒。protobuf 字段个数、varint、长度、楼层和媒体数量均有边界；解析不用递归猜测任意未知消息。

API 使用固定平台端点：帖子/楼中楼、吧主题列表、搜索、通知及显式提交接口。所有传输使用证书与主机名校验的 HTTPS。媒体只允许已明确列举的百度图床/音视频域，逐跳复核重定向且不带登录凭据；旧图床 HTTP 地址只在已允许域内规范化为 HTTPS。默认不使用环境代理。

平台非零错误保留错误码；仅识别明确的未登录说明，不依靠猜测编号将所有错误说成登录失效。HTML/JSON 挑战、HTTP 拒绝或验证码不会被当作空帖子成功，也不做自动重试。

当前 TLS 仍复用项目的 ureq/rustls ring 后端，不能称为完全 Rust TLS。

## 授权单次普通回复

`AuthClient::prepare_own_reply(&mut Session, ThreadId, text)` 只读核验本人身份、线程/首楼作者及真实贴吧信息，读取发送前首尾页ID，从已认证的`newmoindex`读取`data.tbs`并核对账号，不混用`itb_tbs`。返回不公开序列化的`PreparedReply`。宿主必须先将一次性操作持久记录为attempting，再调用`send_prepared_reply`，此方法消费准备对象并只向固定HTTPS网页普通回复端点发送一次POST，不重试或跟随重定向。根Runtime将这些内部阶段合成一个AI工具调用，不增加逐步人工确认。

`ReplyReceipt`中的post_id可能缺失；成功状态不等于可见。`verify_post`只读首尾页，要求新post_id、本人作者与精确正文，排除发送前ID。网络/格式/HTTP异常后的`WriteResultUnknown`不得自动重发。返回挑战或平台错误时停止，不伪造设备参数、验证码或鼠标指纹。

回执解析只有所有显式错误码均为0才判定接受；`no`与`err_code`可属于不同命名空间，若全部非0则作为明确拒绝，保留首个代码。0与非0混杂、缺失或非整数代码仍视为未知。之后的只读诊断结果不能替代已丢失的原始POST结果。

未知/attempting日志可直接构造独立的`ReplyVerification`，由`verify_reply`只读核对，不伪造成功回执。核对继续要求本人、目标、精确正文、新ID，并要求帖子时间至少为`attempted_at-30`秒；缺失时间不确认。`verify_post`仅将正常回执转换为该只读查询，使用`accepted_at`作为查询时间。

网页普通回复字段参考作者源码 [shfshanyue/tieba_post](https://github.com/shfshanyue/tieba_post/blob/master/tieba.py)。旧 aiotieba 的完整 Android add_post 实现已被上游移除；普通回复仍使用原网页路径，新楼中楼实现另见下节。是否被当前服务端接受须单独实号验收，离线合成测试不证明已发送。

## 搜索、吧列表和结构化发布

`Client::forum_threads` 读取真实吧 ID、主题、作者、回复/浏览计数、时间及分页；排序值为回复时间 6、发布时间 1、热门 3、关注的人 2。`search_global` 与 `search_forum` 分别使用 PC 签名 GET 和客户端签名表单，返回搜索摘要及帖子/楼层/楼中楼的精确坐标。未知分页保持 `None`，不猜测结束或重发上页。搜索摘要不是完整原帖内容。

`AuthClient::prepare_thread` 生成新主题的普通正文及经过校验的 BDE_Image 元素；`prepare_floor_reply` 先只读核对真实父楼和选中楼中楼，再固定 `quote_id/repostid/sub_post_id/reply_uid`。图片通过 `upload_image` 上传，绑定吧 ID，之后使用 `attach_floor_images` 生成客户端 `#(pic,id,width,height)` 元素。只接受 PNG/JPEG，单图最多 2 MiB、最多四张；本模块未实现 GIF、贴吧视频/语音发布、复杂表情包管理或 @ 解析。

发送能力 `PreparedAction` 不可序列化，绑定当前账号和五分钟期限，`send_action` 消费它并仅发一次请求。宿主将图片上传与最终提交一起置于持久单次操作中；超时/挑战/混合状态/丢失响应均不自动重试。JSON 新主题回执必须有明确零状态及真实主题 ID；楼中楼回执可缺少新 ID，此时只能报告接受，不能标为已回读验证。读回要求本人、精确正文、时间、吧/父楼及所用图片都匹配。服务器拒绝或挑战不会改走另一接口。

根工具 `social_tieba` 封装 `forum/search/open_reply/create_thread/reply_floor/upload_image`。发现结果返回通用 `target_ref` 和精确 `reply_ref`；图片上传与 CSRF 由 Runtime 处理，AI 不拼平台接口。继续分页保留本页尚未交付的结果，账号归属由不透明引用固定。旧未知普通回复记录不迁移、不重发。

协议依据均来自作者主源，Rust 代码独立实现，不捆绑或运行这些项目：

- [aiotieba get_threads](https://github.com/lumina37/aiotieba/blob/3922a9f1f4643f4652bc4a33f853e8aea29ffe24/src/aiotieba/api/get_threads/_api.py)、`search_global/search_in_forum`、`ThreadInfo/Page/CommonReq` schema；许可 Unlicense。
- [dsh-tieba-dive lib/tieba.js](https://github.com/Nekomeow114/dsh-tieba-dive/blob/main/lib/tieba.js)：最小原生楼中楼请求与网页主题提交字段；MIT，声明见 [license](licenses/dsh-tieba-dive-MIT.txt)。没有复制其浏览器、设备指纹或挑战处理。
- [libsgh/tieba-api TieBaApi.java](https://github.com/libsgh/tieba-api/blob/master/src/main/java/com/github/libsgh/tieba/api/TieBaApi.java)：imgtbs、图片上传响应及客户端图片元素；MIT，声明见 [license](licenses/tieba-api-MIT.txt)。

新增功能通过 41 项 SDK 测试及 3 项宿主契约测试。2026-10-30 管理面实网只读验收：搜索返回 3 条真实结果及可用 continuation；没有执行平台写入。社区接口可能返回当前账号的风控限制，发布/图片上传的实际接受度仍需后续明确授权的实号测试。

## 互动通知

`Client::notifications(NotificationKind::{Reply,At,Like}, page, limit)` 使用固定的`replyme/atme/agreeme`签名表单端点；`AuthClient::notifications(&mut Session, ...)` 可复用已有登录态。不会自动登录、翻页或发送已读确认。正文、对方、时间、帖子/楼层及分页均按实际字段返回；空引用ID保持None，必需帖子ID仍严格校验。被赞采用`agree_list/agreeer/op_time/thread_info`，未带正文时不生成假正文，媒体保留未展开标识。

本地输出限额与服务器分页分开，截断不保证能直接跳到下一页而无漏项；服务端没有返回页码时保持None。读取是否隐式影响未读状态未知。

2026-10-29实号本页验证：回复20条、@11条、被赞通知20条，见[根记录](../../docs/validation.md)。协议主源为作者[NewTiebaApi](https://github.com/sanniou/Thread/blob/7d2a3a2aa4bc78a7129c03c7a09234ef24571f60/TiebaLite-4.0-dev/app/src/main/java/com/huanchengfly/tieba/post/api/retrofit/interfaces/NewTiebaApi.kt)，字段差异以本项目受控的真实响应核验。

开发诊断示例`inspect_notifications`仅在显式传入私有Session路径时请求失败的reply/like类别，原始响应写入私有`/state`，终端只输出结构。它不属于根Runtime二进制或模型工具。

## 复现与源码依据

使用主项目隔离脚本构建/运行；示例 `read_thread` 可显式读取两页、一个楼中楼入口，并可落盘一张实际图片。数据仅写私有报告目录。

```sh
bash scripts/isolate.sh build -- sh /workspace/scripts/cargo.sh build --locked -p platform-tieba --example read_thread
bash scripts/isolate.sh run online -- /cache/target/debug/examples/read_thread 8093410706 /reports/tieba-text-new
bash scripts/isolate.sh run online -- /cache/target/debug/examples/read_thread 10818040463 /reports/tieba-image-new
```

输出目录中的文件必须尚不存在。实际验证见 [VALIDATION.md](../../docs/validation.md)。

协议来源是维护者 [aiotieba](https://github.com/lumina37/aiotieba/tree/3922a9f1f4643f4652bc4a33f853e8aea29ffe24)，固定提交 `3922a9f1f4643f4652bc4a33f853e8aea29ffe24`：

- [`get_posts` 请求](https://github.com/lumina37/aiotieba/blob/3922a9f1f4643f4652bc4a33f853e8aea29ffe24/src/aiotieba/api/get_posts/_api.py) 与 PbPage request/response schemas。
- [`get_comments` 请求](https://github.com/lumina37/aiotieba/blob/3922a9f1f4643f4652bc4a33f853e8aea29ffe24/src/aiotieba/api/get_comments/_api.py) 与 PbFloor schemas。
- [`HttpCore`](https://github.com/lumina37/aiotieba/blob/3922a9f1f4643f4652bc4a33f853e8aea29ffe24/src/aiotieba/core/http.py) 的 multipart 包装；本模块保持正常 TLS 校验，没有照搬参考中的 `ssl=False`。
- [`Post`](https://github.com/lumina37/aiotieba/blob/3922a9f1f4643f4652bc4a33f853e8aea29ffe24/src/aiotieba/api/_protobuf/Post.proto)、[`PbContent`](https://github.com/lumina37/aiotieba/blob/3922a9f1f4643f4652bc4a33f853e8aea29ffe24/src/aiotieba/api/_protobuf/PbContent.proto)、[`User`](https://github.com/lumina37/aiotieba/blob/3922a9f1f4643f4652bc4a33f853e8aea29ffe24/src/aiotieba/api/_protobuf/User.proto) 字段定义。

这是社区逆向协议参考，不是百度稳定性承诺。读取动作、普通回复和结构化发布分别处理；没有点赞、签到或私信发送。

扫码参数参考开发者主源 [libsgh/tieba-api](https://github.com/libsgh/tieba-api/blob/master/src/main/java/com/github/libsgh/tieba/api/TieBaApi.java)。子站同步的精确跳转及参数由本项目2026-10-29对官方接口的受控响应确认，未运行参考项目或其中任何写操作。
