# 非扫码登录态导入

扫码是管理端的一个登录入口。AI 日常工具使用已保存的账号；不会要求 AI 读取、拼装或提交 Cookie，也不会自动打开扫码、浏览器或账号密码流程。

本地与 SSH 都优先使用操控端，文件路径属于操控端电脑而非 VPS：

```sh
agentctl --account bili-main bili import-login --secret-file /private/bilibili-cookie.txt
agentctl --account tieba-main tieba import-login --secret-file /private/tieba-bduss.txt
agentctl --account qq-main qq login --method password
```

QQ 账号与密码可在操控端终端输入，密码隐藏回显；验证码通过同一控制会话回传 proof，核心保持原 TCP。不要以多个独立 Runtime 进程模拟同连接续验。跨进程恢复只支持明确选择的 `--resume-from-disk`，会标注 `reconnected_from_disk`。核心没有验证码网页监听器。完整协议见 [操控端](operator-control.md)。

底层兼容管理 API 仍保留：

```sh
agent-runtime --account bili-main bili import-login /private/bilibili-cookie.txt
agent-runtime --account tieba-main tieba import-login /private/tieba-bduss.txt
```

输入必须是现有私有普通文件，建议权限 `0600`；符号链接、FIFO、过大文件和非 UTF-8 输入会被拒绝。凭据从文件读取，不作为命令行参数或模型工具参数。文件可以位于部署时明确挂载的私有目录；Runtime 不搜索浏览器、桌面或其他应用的账号材料。

| 平台 | 可接受输入 | 核验与保存 |
| --- | --- | --- |
| Bilibili | 本 Runtime 的 `session.json`，或一行 Cookie header；可带 `Cookie:` 前缀 | Cookie 至少包含 `SESSDATA`、`bili_jct`。调用已有 current-user 接口获得 MID；若提供 `DedeUserID` 必须一致。只保存已支持的账号 Cookie，不编造 refresh token。 |
| 贴吧 | 本 Runtime 的 `session.json`，一行 Cookie header，或裸正式 `BDUSS` | 复用已有固定域名的用户身份与必要子站握手；不发起二维码或临时票据交换。可保留 `STOKEN` 等必要账号 Cookie。 |

导入完成后，账号按同一别名进入普通工具的自动选择和不透明引用归属流程。服务端拒绝、验证码/风险验证、身份不匹配均会返回明确失败，不自动改走二维码。只有身份核实成功且与别名绑定一致才原子替换保存的登录态；失败不会删除原会话或重定向别名到另一个人。

已有 `--bili-cookie-file`、`--tieba-bduss-file` 全局选项继续是本次进程的临时覆盖。与上面的持久导入不同，临时覆盖不会把外部文件自动变成托管登录态。

这两个入口是复用正式登录态，尚未提供账号密码、短信验证码或浏览器挑战执行器。Cookie/BDUSS 本身依然可能过期或被平台撤销；导入不能绕过平台身份核验。Codex、Antigravity 的订阅 OAuth 和 GitHub token 管理入口各自保留，微信个人号测试继续暂停。

验证范围：本轮 Docker 无网测试覆盖 Cookie/BDUSS 格式、重复身份 Cookie、控制字符注入、过滤无关 Cookie、已核实 MID 绑定、私有文件/符号链接拒绝、失败时旧会话保留。真实导入验收由 Root 使用明确指定的已有会话执行，不由这些合成样本推定通过。
