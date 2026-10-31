# Gmail 与 X 接入候选

2026-10-31 记录。X 已实现 Rust 网页会话 SDK、私密导入与共享只读工具，并在 Docker 中通过现有真实会话完成身份、资料、作者帖子、单帖、两页各 20 条的连续读取与一张按需图片解码。扩大分页的初次失败后来定位到重复资料查询的 HTTP 403；直接使用已验证作者引用后通过，没有重新登录或社交写操作。操控端已加入 X，详细范围见 [验收记录](validation.md)。Gmail 尚未实现，没有可登录的占位卡片。

## 产品分组

- Agent 引擎：Codex、Antigravity。保留各自订阅登录，共用 Runtime 人格与记忆。
- 社交平台：Bilibili、QQ、贴吧、微信、X。
- 应用与协作：GitHub，以及后续 Gmail。邮件线程、收件人和草稿保持邮件语义。

分组用于呈现角色，不拆成多套 Runtime；平台内容仍由语义工具和共享媒体链处理。

## X：优先验证网页登录态

不把付费官方 API 作为唯一接入方式。候选方案是操控端完成正常网页登录，
Runtime 使用用户自己的网页会话，Rust 直接访问网页所用 HTTP/GraphQL 接口。
本轮已经通过私密导入的现有会话完成小范围真实只读验收；具体边界见
[验证记录](validation.md)。这不代表所有网页功能均已可用。

[Twikit](https://github.com/d60/twikit) 提供 Cookie 会话、检索、发帖和媒体上传等实现；
其 [GraphQL 客户端](https://github.com/d60/twikit/blob/main/twikit/client/gql.py)
与 [请求事务实现](https://github.com/d60/twikit/blob/main/twikit/x_client_transaction/transaction.py)
表明，网页请求需要维护操作 ID、feature 参数和动态请求头。
现有实现通过解析页面、脚本中的参数及数学计算完成请求事务生成，没有因此执行整个网页。
我们可以独立用 Rust 实现所需的小范围处理，保留不使用 V8/Blink/浏览器内核的边界。

建议的实施顺序：

1. 私密控制协议导入/复用网页登录态，核验当前用户并绑定账号别名。
2. 只读验收：账号身份、指定帖子与线程、作者其他内容、有限分页和媒体引用。
3. 沿用 Runtime 的媒体读取、目标引用、Outbox 和写入回读，增加发帖、回复、图片/GIF/视频。
4. 再验证通知、点赞、转发、关注和私信；按实际通过的操作返回能力。

脚本和接口配置必须限大小、限来源、固定 schema；缺失或变化返回
`protocol_changed` / `verification_required` 等明确状态，不装入 JS 引擎作兜底。
正常登录中的设备确认/验证码仍交给人操作，沿用同一账号和控制会话。
网页本身仍有访问频率与账号限制，因此这条方案不等于无限访问。
不接入验证码代答、自动解锁或账号池；不将失败请求自动切换成收费调用。

Twikit 的 [MIT 许可](https://github.com/d60/twikit/blob/main/LICENSE) 属于上游作者。
目前的 Rust 实现已经分别记录固定提交、文件和上游版权，交易算法翻译独立保留
XClientTransaction 的原始 MIT 许可。完整范围见 [SDK NOTICE](../crates/platform-x/NOTICE)，
没有将上游翻译标成我们的原创。

官方 [OAuth 用户授权](https://docs.x.com/fundamentals/authentication/oauth-2-0/authorization-code)
可作为未来显式选择的另一路线；当前官方 [计费说明](https://docs.x.com/x-api/getting-started/pricing)
采用按使用量付费。若启用，必须单独配置开发者应用与预算，不与网页登录态隐式混用。

## Gmail：独立邮件模块

采用 Gmail HTTP API 的用户 OAuth，授权在操控端的系统浏览器完成，Runtime
只保存该邮件账号的授权状态。Antigravity 的 Google 订阅身份不等于 Gmail 授权，
不挪用其 OAuth client 或 refresh token。

首批任务入口应覆盖邮件检索/列表、展开完整线程、草稿、回复/发送、已读/归档/标签，
以及按需拉取附件。Rust 处理 MIME、HTML 正文、正文中的图片关系和附件引用，
复用现有图片/音视频资源链；AI 不需要自己拼 MIME、猜下载接口或连接处理步骤。
邮件列表保持有界，使用历史游标增量同步；游标过期时按明确范围重新同步，
不将整座邮箱复制到内存。参考 [官方同步说明](https://developers.google.com/workspace/gmail/api/guides/sync)。

回复目标包含线程、真实发件人/收件人及引用邮件关系；发送结果进入既有 Outbox。
沿用当前工具授权，不人为增加每封邮件的层层确认。结果未知时不盲目重发。

权限按实际启用功能逐步申请。Google 将 `gmail.readonly`、`gmail.modify` 等列为
restricted scopes，`gmail.send` 列为 sensitive scope；公开统一 OAuth 应用和自用应用
的验证条件应分开核对，不能把开源或自托管直接当成免审保证。参考
[官方权限表](https://developers.google.com/workspace/gmail/api/auth/scopes) 和
[验证条件及例外](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification)。

## 共同约束

默认模块化、按账号启用；未启用不启动连接或后台进程。Cookie、OAuth token 和原始
认证 URL 留在私密管理面，不成为 AI 工具参数。操控端可以使用 WebView，VPS Runtime
不引入浏览器、Python/Node 客户端或整套上游运行环境。具体可用性仍以我们的 Rust
适配和真实账号验收为准。
