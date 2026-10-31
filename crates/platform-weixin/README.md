# platform-weixin

个人微信号 WebWx 路线的 Rust 实验适配器。使用 HTTPS、有限状态和私有会话文件；默认普通 Web profile，可显式选择桌面兼容 profile。整个实现不需要微信 Bot 通道、浏览器内核、桌面微信或 Wine。当前不提供发送、好友操作、群管理或注销请求。

## 显式登录 profile

`Config.profile` 为 `LoginProfile::{Web, DesktopCompatibility}`，默认 `Web`。`DesktopCompatibility` 依照 openwechat 的公开 `desktopMode`，使扫码的 redirect_uri 带 `mod=desktop`，仅在随后取票据的 GET 请求上附加固定 `client-version: 2.0.0` 和 `extspam` 兼容值；初始化、同步、图片和声音请求均不携带这两个头。

这个 profile 仍是 WebWx，不是原生 MMTLS。首次仍需真实手机扫码和确认，服务端可随时拒绝。程序不自动尝试另一 profile，也不为未确认的扫码尝试构造 Session。Client 与 Attempt profile 不匹配时直接拒绝，避免意外混用。

`LoginAttempt::profile()` 和 `Session::profile()` 可供 Runtime 控制面查询。profile 写入私有文件，旧文件缺该字段时默认 `Web`。恢复待确认尝试时应按已保存 profile 创建 Client：

```rust,ignore
let mut attempt = LoginAttempt::load(private_attempt_path)?;
let config = Config { profile: attempt.profile(), ..Config::default() };
let client = Client::new(config)?;
let status = client.poll_login(&mut attempt)?;
```

兼容固定值来自 [openwechat mode.go](https://github.com/eatmoreapple/openwechat/blob/master/mode.go) 和 [global.go](https://github.com/eatmoreapple/openwechat/blob/master/global.go)，2026-10-29 复核。开发脚本 `check-reference-container.sh` 已将 880 字节固定值与当前上游逐字节核对一致。本模块此次仅离线验证该 profile，实际扫码由 Runtime 统一触发。

## 接口

```rust,ignore
let client = Client::new(Config::default())?;
let mut attempt = client.begin_login()?;
client.download_qr(&mut attempt, private_qr_path)?;
attempt.save(private_attempt_path)?;

// 调用方单步轮询；只有用户扫码并确认后才可能 Confirmed。
let status = client.poll_login(&mut attempt)?;
let session = client.finish_login(&mut attempt)?; // 首次 webwxinit，尚不能保证任何号可用
session.save(private_session_path)?;

let mut session = Session::load(private_session_path)?;
client.resume(&mut session)?; // 用原 SyncKey 做 synccheck，不以 init 覆盖旧游标
let page = client.contacts(&mut session, 0)?;
let update = client.sync_check(&mut session)?;
let batch = client.sync(&mut session)?;
// 调用方先持久化消息，再保存新游标；附件失败不应让原 Session 文件前移。
session.save(private_session_path)?;
```

`LoginStatus` 只有 `waiting/scanned/confirmed/expired`，不暴露 uuid、redirect 或票据。每次 `poll_login` 只做一个有界请求；网络错误不会清除旧 Session，也不会自动重新登录。二维码尝试设置本地 300 秒处理窗口，这不保证服务端二维码恰好有同样有效期。

`Session` 和 `LoginAttempt` 不实现公开 `Serialize`；`Debug` 隐藏票据。显式 save/load 使用私有格式、0600 文件、0700 父目录、no-follow/nonblock、原子 rename 和目录 fsync。`stable_identity()` 提供 UIN 绑定给宿主控制面比较，不能输出给模型或日志。相同 alias 是否允许换号由 Runtime 管理。

`SessionInfo`、`ContactPage`、`SyncStatus` 和 `SyncBatch` 是可给调用方的投影。`Message` 保留稳定 MsgId、作者、会话、群内发送者、原始消息内容与媒体类型。`message.source` 仅包含固定消息字段，不包含同步响应顶层的 SKey、SyncKey、Cookie 或其他未知字段。不要把整个登录、init 或 sync 响应另存成可给模型的资源。

`download_media` 针对实际消息构造受限的图片、语音或视频读取请求；不会接受任意 URL。资源流式写给宿主 `Write`，可直接接入共享资源缓存。它只取得媒体字节，不做语音识别；共享媒体模块的 SILK/AMR 解码仍未实现。普通文件、表情 CDN、群成员详情和历史查询尚未实现。

## 限额和实际边界

默认请求总超时 35 秒，下载 90 秒，JSON 2 MiB，媒体 64 MiB；可显式调整但有硬上限。Runtime 传给下载的限额不得高于 `Config.max_media_bytes`。QR 图片上限 256 KiB，单次 sync 至多 1000 条消息，联系人页至多 10000，SyncKey 至多 1024 项。Cookie 有域、路径、数量及总字节限制，只有指定的腾讯 WebWx 主机可接收请求。

Cookie 保存支持 Max-Age 和过期删除等当前必要子集，不实现整个浏览器 Cookie 行为。不会运行 JS、加载页面、自动绕过平台资格限制。票据拒绝、Web 登录不可用、账号过期与未知协议错误明确区分，并隐藏服务端可能包含票据的错误原文。退出 Client 不会发送 webwxlogout。

真实账号现已扫码确认，但普通 WebWx 票据接口直接返回 GBK HTML 拒绝页：服务端以账号安全为由暂不支持网页版入口，并建议使用官方客户端。当前没有取得 Session，联系人、群和媒体尚未实网接通。生产错误为 `WebLoginPageUnavailable`，不虚构数值 retcode；未知 HTML 另报 `LoginHtmlResponse`。详见 [验证记录](../../docs/validation.md)。

票据请求独立禁止追随 HTTP 重定向，因为 301/302 的中间响应可能自带 XML 票据。原始 redirect 的编码参数保持不变，只设置 `version=v2`；普通 Web profile 不附加桌面兼容头。

## 本模块验证

在项目根目录：

```sh
bash scripts/isolate.sh build online -- sh /workspace/crates/platform-weixin/check-container.sh test
bash scripts/isolate.sh build -- sh /workspace/crates/platform-weixin/check-container.sh build --offline --example qr_probe
# 需换一个尚不存在二维码文件的私有目录；不会完成登录或发送消息。
bash scripts/isolate.sh run online -- /cache/target/debug/examples/qr_probe /state/weixin-probe-new --poll-once
```

`check-container.sh` 仅是本模块独立开发脚本，在可写缓存副本中构建；项目正常构建使用根 workspace 和锁文件。测试用 localhost HTTP 重写仅存在于 `cfg(test)`，生产构建依然 HTTPS-only 和固定主机白名单。

协议对照来自项目作者 [openwechat client.go](https://github.com/eatmoreapple/openwechat/blob/master/client.go)、[url.go](https://github.com/eatmoreapple/openwechat/blob/master/url.go)、[mode.go](https://github.com/eatmoreapple/openwechat/blob/master/mode.go)，调研日期 2026-10-29。源码仅作为协议参考；没有嵌入 Go SDK 或复制 UOS 登录补丁。
