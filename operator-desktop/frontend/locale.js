// SPDX-License-Identifier: MIT
// Explicit UI vocabulary only. Platform names, identities and chat bodies are
// marked translate=no and are never rewritten or persisted by this module.
(() => {
  const en = {
    "请在普通浏览器的 Glimlink 交接页，通过扩展连接已登录账号。":"Use the Glimlink handoff page in your normal browser to connect the signed-in account through the extension.",
    "重新连接":"Reconnect",
    "扫码登录 ↗":"Sign in with QR ↗",
    "尚未完成账号核验，请重新连接。":"Account verification is incomplete. Connect again.",
    "请先安装 Chrome、Chromium 或 Edge。":"Install Chrome, Chromium or Edge to continue.",
    "请先停止后台，再连接账号。":"Stop the worker before connecting an account.",
    "平台暂时限流，请稍后重试。":"The platform is limiting requests. Try again later.",
    "登录已超时，请重试。":"Sign-in timed out. Try again.",
    "连接未完成，请重试。":"Connection did not complete. Try again.",
    "浏览器登录，自动连接账号":"Sign in in your browser to connect automatically",
    "插画与漫画":"Illustrations and manga",
    "作品、推荐与按需读图":"Artworks, recommendations and images on demand",
    "改用扫码登录":"Use QR sign-in instead",
    "正在打开本机浏览器登录窗口…":"Opening a local browser sign-in window…",
    "请在浏览器里登录。完成后自动交接给 Runtime，无需复制登录信息。":"Sign in in the browser. The session is handed to Runtime automatically; no copying is needed.",
    "浏览器登录信息已交回，正在由 Runtime 核验账号…":"Browser session received. Runtime is verifying the account…",
    "未找到支持自动交接的本机浏览器。请安装 Chrome、Chromium、Edge 或 Brave；已有私密文件仍可使用 agentctl 导入。":"No supported local browser was found. Install Chrome, Chromium, Edge or Brave. Existing private session files can still be imported with agentctl.",
    "浏览器登录连接未完成，请重新打开登录。已有 Runtime 登录态已保留。":"Browser sign-in did not complete. Start sign-in again. Your saved Runtime session has been kept.",
    "网络暂时中断，授权仍在等待，不用重复登录。":"Network interrupted; authorization is still waiting. You do not need to sign in again.",
    "已收到 GitHub 授权，正在等待账号核验，不用重新授权。":"GitHub authorization received; waiting to verify the account. Do not authorize again.",
    "此窗口只支持 X 账号密码；Google / Apple 登录请使用操控端的系统浏览器入口。":"This window supports X username/password only. Use the operator’s system-browser option for Google / Apple.",
    "请在系统浏览器完成 X 登录，可以使用 Google / Apple。然后按操控端的步骤导入账号。":"Sign in to X in your system browser, including Google / Apple. Then follow the operator’s account import steps.",
    "请打开系统浏览器访问 x.com，完成登录后按操控端步骤导入账号。":"Open x.com in your system browser, sign in, and follow the operator’s account import steps.",
    "在系统浏览器中登录":"Sign in with your browser",
    "可使用 Google / Apple":"Google / Apple supported",
    "在浏览器中完成 X 登录，然后按下方步骤导入这个账号。目前使用网页登录信息，尚未接入 X 官方应用授权。":"Sign in to X in your browser, then import the account below. This uses a web session; official X app authorization is not connected yet.",
    "打开浏览器登录 X":"Sign in to X in browser",
    "使用浏览器里已登录的账号":"Import an account signed in to your browser",
    "手动导入":"Manual import",
    "这条方式需要手动复制登录信息。只在 Chrome 等浏览器中登录，不会自动连接到 Runtime。":"This method requires copying sign-in data. Signing in to Chrome or another browser alone does not connect Runtime.",
    "登录信息导入方式":"Sign-in import method",
    "复制两项信息":"Copy two values",
    "Cookie 整行（高级）":"Cookie header (advanced)",
    "从 Chrome 获取这两项":"Get the two values from Chrome",
    "在 x.com 登录你的账号。":"Sign in to your account on x.com.",
    "右键网页 → 检查 → 应用（Application）。找不到这个标签时，点开发者工具顶部的 »。":"Right-click the page → Inspect → Application. If the tab is hidden, use » at the top of DevTools.",
    "左侧展开 Cookie，选择 https://x.com。":"Expand Cookies on the left and select https://x.com.",
    "找到 auth_token 和 ct0，分别复制“值（Value）”列的完整内容，粘贴到下面。不要修改或删除浏览器里的值。":"Find auth_token and ct0. Copy each complete Value into the fields below. Leave the browser values unchanged.",
    "Cookie 请求头是什么？":"What is a Cookie header?",
    "它是浏览器请求 X 时携带的登录信息，形如 auth_token=…; ct0=…。它不是密码，也不是网页地址。":"It is sign-in data the browser sends to X, such as auth_token=…; ct0=…. It is not your password or a page address.",
    "在已登录的 x.com 打开开发者工具 → 网络（Network），然后刷新页面。":"On a signed-in x.com page, open DevTools → Network, then reload the page.",
    "选择一条发往 x.com 的请求，在“标头（Headers）→ 请求标头（Request Headers）”中找到 Cookie，复制它的整段值。":"Select a request to x.com. Under Headers → Request Headers, find Cookie and copy its complete value.",
    "粘贴到下方，只需 Cookie 这一项，不要复制整个请求或 cURL 命令。":"Paste only the Cookie value below, not the entire request or a cURL command.",
    "Cookie 整行内容":"Complete Cookie value",
    "粘贴 Cookie 的整段值":"Paste the complete Cookie value",
    "粘贴 auth_token 的值，不是名称":"Paste the auth_token value, not its name",
    "粘贴 ct0 的值，不是名称":"Paste the ct0 value, not its name",
    "这两项相当于登录凭据，只粘贴在这里，不要发到聊天或截图中。":"These values are sign-in credentials. Paste them here, never in chat or screenshots.",
    "其他方式：X 账号密码":"Other method: X username and password",
    "此窗口仅供直接输入 X 账号和密码。Google / Apple 登录请使用上方的系统浏览器。":"This window supports direct X username/password entry. For Google / Apple sign-in, use the system browser above.",
    "打开 X 账号登录窗口":"Open X account sign-in window",
    "在浏览器登录，再导入账号":"Sign in in your browser, then import the account",
    "在浏览器中授权 ↗":"Authorize in browser ↗",
    "使用已有 Token":"Use an existing token",
    "连接 X ↗":"Connect X ↗",
    "重新登录":"Sign in again",
    "验证并连接":"Verify and connect",
    "连接 X 账号":"Connect an X account",
    "浏览器登录信息":"Browser sign-in data",
    "用浏览器完成登录，再把这个账号连接到 Runtime。":"Sign in in your browser, then connect the account to Runtime.",
    "请在 X 窗口正常登录，无需复制信息。完成后等待这里显示核验结果。":"Sign in normally in the X window. No copying is needed. Wait here for verification.",
    "浏览器已打开。登录后按下方步骤复制两项信息，再点“验证并连接”；不会自动回传。":"Browser opened. After signing in, copy the two values using the guide below, then select Verify and connect. There is no automatic callback.",
    '发送中':'Sending','送达尚未确认':'Delivery unconfirmed','发送失败':'Send failed','重试发送':'Retry send','请先处理未送达的消息，再继续发送。':'Resolve undelivered messages before sending more.','Enter 发送 · Shift+Enter 换行':'Enter to send · Shift+Enter for a new line',
    '在浏览器中获取 Token ↗':'Get a token in browser ↗',
    '开始登录检查':'Checking sign-in','已取消':'Cancelled',
    '窗口标题栏':'Window title bar','最小化':'Minimize','最大化':'Maximize','还原窗口':'Restore window','关闭窗口':'Close window','窗口操作未完成。':'The window action did not complete.',
    '在浏览器中登录 ↗':'Sign in with browser ↗','后台正在使用账号状态，请先在调控页停止后台，再登录。':'The worker owns account state. Stop it in Control before signing in.',
    '停止后台后登录':'Stop worker and sign in','后台正在执行一轮，请等待结束，或在调控页主动停止。':'A turn is running. Wait for it to finish, or stop it in Control.',
    '后台已停止，登录后可在调控页重新启动。':'Worker stopped. You can restart it in Control after signing in.',
    '请在系统浏览器的登录页完成扫码或确认。':'Scan or confirm on the sign-in page in your system browser.',
    '简体中文':'Simplified Chinese',
    '峰值':'Peak','上限':'Limit','Runtime · VPS 容器':'Runtime · VPS container','Runtime · 本机':'Runtime · local',
    '统计当前连接的 Runtime，包含原生引擎，不含操控端。':'Measures the connected Runtime and its native engines, excluding this operator.',
    '包含 Runtime 容器与原生引擎，不含操控端或其他服务。':'Includes the Runtime container and native engines, excluding this operator and other services.',
    '包含 Runtime 及其原生引擎子进程，不含操控端。':'Includes Runtime and its native engine children, excluding this operator.',
    '当前连接的 Runtime 内存暂不可用。':'Memory data for the connected Runtime is unavailable.',
    '容器内存 · 含缓存':'Container memory · including cache','Runtime 后台已停止。':'The Runtime worker is stopped.',
    '直接读取容器内存，峰值按本次容器运行累计。':'Read directly from the container. Peak is recorded for this container run.',
    '每 5 秒采样':'Sampled every 5 seconds','等待当前 Runtime 的实际采样。':'Waiting for an actual sample from the current Runtime.',
    '界面预览未连接 Runtime，无法测量。':'This preview is not connected to Runtime; memory cannot be measured.',

    '已保存账号，平台暂未提供名称':'Account saved · name unavailable',
    'Agent 设置':'Agent settings','完成':'Done',
    '未核验账号':'Account not checked',
    '个进程':'processes',
    'RSS · 已读进程':'RSS · readable processes','RSS · 驻留内存':'RSS · resident memory','Token 已保存':'Token saved','一次性授权码':'One-time authorization code','仅通过私有控制通道交给 Runtime，不进入 AI 对话、活动记录或本页持久存储。提交后立即清空。':'Sent only over the private control channel, never to AI dialogue, activity logs, or browser storage. Cleared after submission.','会话已保存':'Session saved','可跳转系统浏览器登录，也可使用内置窗口。已有登录可手动复制会话导入；不会自动读取外部浏览器。':'Sign in with your system browser or the built-in window. Existing sessions can be imported manually; the external browser is never read automatically.','在 X 窗口内正常登录，完成后自动交给 Runtime 核验。':'Sign in normally in the X window. Runtime verifies the resulting session.','在系统浏览器登录后回到这里，一次粘贴 Cookie 请求头。不会自动读取外部浏览器，也没有自动回调。':'Sign in in your browser, then return to paste the Cookie header. There is no automatic external-browser access or callback.','完成订阅授权':'Complete subscription authorization','密码':'Password','导入 X 网页会话':'Import X web session','导入已有登录态':'Import existing sign-in','当前系统无法提供内存数据。':'Memory measurements are unavailable on this system.','文件只从操控端读取。请使用权限 0600 的私有普通文件。':'Files are read only on the operator computer. Use a private regular file with mode 0600.','无可用数据':'No data available','暂不可用':'Unavailable','未检查':'Not checked','本机私密文件路径':'Private local file path','核心没有返回有效的登录会话标识。':'Runtime did not return a valid sign-in session ID.','正在打开专用登录窗口…':'Opening a dedicated sign-in window…','浏览器 Cookie':'Browser cookies','访问 Token':'Access token','请填写 auth_token 与 ct0 两项 Cookie。':'Enter both auth_token and ct0.','请粘贴完整 Cookie 请求头，或改为分别填写。':'Paste the complete Cookie header or switch to separate fields.','请输入有效的 QQ 账号。':'Enter a valid QQ account number.','输入 QQ 登录信息':'Enter QQ sign-in details','这个输入类型尚未支持，请取消本次登录。':'This input type is unsupported. Cancel this sign-in.','连接 GitHub 账号':'Connect a GitHub account','通过当前私有控制通道提交，不进入 AI 上下文。':'Submit through the current private control channel, outside AI context.','预览模式不执行真实账号操作。':'Preview mode does not perform real account actions.',
    '上次读取的账号名称，核验后显示当前登录状态。':'Previously read account name. Verify to check the current sign-in state.','账号名称来自已连接的平台。':'Account name returned by the connected platform.',
    '连接':'Connections','调控':'Control','指令':'Commands','设置':'Settings','调控与会话':'Control and conversations','调控 · 会话':'Control · Conversations',
    '账号与 Runtime，各自保持连接。':'Your accounts and Runtime, connected.','和 AI 对话，选择引擎，调整它的节奏。':'Talk to your AI, choose its engine, and set its pace.','把管理权交给明确获准的会话。':'Manage Runtime from explicitly authorized conversations.','外观、语言与 Runtime 的状态。':'Appearance, language, and Runtime status.',
    '界面语言':'Language','本机 UI':'Local UI','本机':'Local','SSH 远端':'Remote SSH','SSH 目标':'SSH destination','用户@服务器 或 SSH 别名':'user@server or SSH alias','user@my-vps 或 SSH 别名':'user@my-vps or SSH alias',
    '我的配色':'My palette','一点你的个性':'Make it yours','鸢尾紫':'Iris','深海青':'Lagoon','落日橙':'Sunset','灵感上线':'A little inspiration','清爽出发':'A fresh start','暖一点点':'A little warmer','界面密度':'Density','舒展':'Comfortable','紧凑':'Compact','配色':'Colors','配色主题':'Color theme',
    '仅保存配色与密度，不保存账号或连接信息。':'Only appearance preferences are saved here, never accounts or connection details.','界面预览':'UI preview','当前没有连接 Tauri 后端，真实账号操作已禁用。':'No native bridge is connected. Account actions are disabled.',
    '检查操控通道':'Check connection','控制通道可用':'Control channel available','检查所有账号':'Check all accounts','当前概览':'Overview','轻巧连接，自在掌控':'Lightweight, connected, yours','控制这台电脑上的 Runtime':'Control the Runtime on this computer','本机与远端，同一套协议':'Local or remote, the same control channel','已核验登录':'Verified accounts','已保存会话':'Saved sessions','等待你的操作':'Ready when you are','优先复用已有登录态':'Existing sign-ins are reused first','正在逐个检查':'Checking accounts','不会自动发起新登录':'No new sign-in is started','同一会话持续验证':'Verification stays in this session','取消':'Cancel',
    'Agent 引擎':'Agent engines','分别使用订阅，共用人格与记忆。':'Separate subscriptions, shared persona and memory.','驱动 AI':'Power your AI','社交平台':'Social platforms','消息、社区与内容，使用各自的真实账号。':'Real accounts for messages, communities, and content.','连接世界':'Connect with the world','应用与协作':'Apps and collaboration','开发协作与其他应用，各自保留业务语义。':'Development and other apps, with purpose-built actions.','完成工作':'Work together','二维码与验证窗口在操控端打开。订阅授权使用系统浏览器；凭据不会交给 AI。':'QR and verification windows open here. Subscription authorization uses your system browser; credentials stay out of AI context.',
    '视频与社区':'Video and community','聊天':'Chat','社区':'Community','网页会话':'Web session','开发协作':'Development','订阅登录':'Subscription sign-in','已暂停':'Paused','视频、动态与评论':'Videos, posts, and comments','消息、群聊与附件':'Messages, groups, and attachments','帖子与楼中楼讨论':'Threads and nested replies','复用会话，或私密导入浏览器 Cookie':'Reuse a session or privately import browser cookies','仓库、Issue 与 PR':'Repositories, issues, and pull requests','原生 Codex 订阅 · 共享人格与记忆':'Native Codex subscription · shared persona and memory','Google 订阅授权 · 共享人格与记忆':'Google subscription · shared persona and memory','个人账号测试暂停，不发起登录':'Personal-account testing is paused; no sign-in is started','贴吧':'Tieba','微信':'Weixin',
    'Runtime 在哪里':'Where is Runtime?','本地使用也是控制自己':'Local use controls this computer','高级连接选项':'Advanced connection options','登录档案别名':'Sign-in profile alias','账号别名':'Account alias','状态目录':'State directory','Runtime 可执行文件':'Runtime executable','默认使用相邻 agent-runtime':'Use the adjacent agent-runtime by default','远端 agent-runtime 路径，可选':'Remote agent-runtime path (optional)','应用并检查':'Apply and check','连接配置只留在当前操控会话。':'Connection details stay in this operator session.','读取账号名称后显示':'Check to read the account name','已登录，平台暂未提供名称':'Signed in · account name unavailable',
    '内存监视':'Memory monitor','操控端 · 含本机子进程':'Operator · including local child processes','暂停':'Pause','继续':'Resume','等待实际采样':'Waiting for a sample','等待采样':'Waiting for a sample','尚无测量数据。':'No measurements yet.','仅统计当前操控端，不代表远端 Runtime。':'This measures the local operator, not the remote Runtime.','最近的内存采样趋势':'Recent memory samples','暂停内存采样':'Pause memory sampling','继续内存采样':'Resume memory sampling','后台暂停':'Paused in background','每 3 秒采样':'Sampled every 3 seconds','采样失败':'Sample unavailable','PSS · 分摊共享内存':'PSS · proportional shared memory','RSS · 可能重复计入共享页':'RSS · shared pages may be counted more than once','RSS 会重复计入共享页；PSS 按比例分摊。':'RSS counts shared pages repeatedly; PSS apportions them.','部分进程未能读取，统计可能不完整。RSS 会重复计入共享页。':'Some processes were unavailable. This sample may be incomplete; RSS double-counts shared pages.','内存采样未完成，稍后自动重试。':'Memory sampling failed; it will retry shortly.','普通浏览器预览不会连接 Runtime 或发起账号登录。':'Browser preview does not connect to Runtime or start sign-in.','界面预览没有操控端进程，无法测量。':'Process memory is unavailable in browser preview.','预览不可用':'Unavailable in preview',
    '刚刚发生':'Recent activity','本次会话':'This session','还没有操作。准备好后，检查一个账号。':'No actions yet. Check an account when ready.','UI 与 Runtime 分离 · 原创部分 MIT':'Separate UI and Runtime · original code is MIT','验证窗口按需打开，用完即关闭':'Verification windows open on demand and close when finished',
    '未检':'Not checked','检查中':'Checking','在线':'Online','已登录':'Signed in','已保存':'Saved','未登录':'Signed out','需要登录':'Sign-in required','等待确认':'Awaiting confirmation','等待验证':'Awaiting verification','验证已过期':'Verification expired','未完成':'Not completed','已取消':'Cancelled','暂时受限':'Temporarily limited','状态待确认':'Status unknown','核验与复用 ↗':'Verify / reuse ↗','复用 / 导入 ↗':'Reuse / import ↗','登录订阅 ↗':'Sign in ↗','连接账号 ↗':'Connect account ↗','暂停测试':'Testing paused','正在启动…':'Starting…','取消登录':'Cancel sign-in','Token 登录 ↗':'Token sign-in ↗','更新网页登录态':'Update web session',
    '私密输入':'Private input','完成这次登录':'Complete sign-in','信息通过私有连接交给 Runtime，不进入 AI 上下文。':'Input goes to Runtime through the private connection, never into AI context.','QQ 账号':'QQ account','验证信息':'Verification input','只用于当前登录。提交后立即清空本页输入。':'Used only for this sign-in. Fields are cleared after submission.','安全提交':'Submit securely','Cookie 导入方式':'Cookie import method','在系统浏览器打开 X':'Open X in system browser','可复用浏览器已有登录。完成后返回这里，一次粘贴会话。':'You can reuse an existing browser sign-in. Return here to paste the session once.','或使用内置 X 登录窗口':'Or use the built-in X sign-in window','或私密导入已有浏览器会话':'Or privately import an existing session','从已登录 x.com 的浏览器开发者工具复制完整 Cookie 请求头，一次粘贴即可。也可分别填写所需字段。':'Copy the Cookie request header from your signed-in x.com browser, or fill the two required values.','一次粘贴':'Paste once','分别填写':'Separate fields','Cookie 请求头':'Cookie request header',
    '操作未完成，请检查连接配置或查看错误码。':'The action did not complete. Check the connection and error code.','配置已修改，请应用后再操作账号。':'Connection changed. Apply it before using account actions.','预览配置可编辑，本页不发起真实连接。':'Preview settings can be edited; no connection is made.','请填写状态目录、账号别名，以及 SSH 模式下的连接目标。':'Enter the state directory, account alias, and SSH destination.','使用系统 SSH 密钥与 known_hosts；首次连接请先在终端确认主机。':'Uses system SSH keys and known_hosts. Verify a new host in a terminal first.','配置已应用。连接本机 Runtime，不需要 SSH。':'Applied. This controls the local Runtime without SSH.','已选择 SSH 控制通道':'Selected the SSH control channel','已选择本地控制通道':'Selected local control','已检查账号状态，没有自动发起登录':'Account status checked; no sign-in was started','个人账号测试暂停，不执行任何账号请求。':'Personal-account testing is paused; no account request is made.','本地已保存，尚未核验服务端有效性':'Saved locally; server validity has not been verified','读取可用方式与已有登录状态…':'Reading supported methods and saved sign-in…','先核验已有状态，再选择可用登录方式。':'Checking existing state before choosing a sign-in method.','登录会话已建立，等待验证状态。':'Sign-in session created; waiting for verification.','正在关闭验证会话，等待最终结果…':'Closing verification; waiting for the final result…','已载入启动器的连接配置；尚未检查账号或发起登录。':'Launcher connection loaded; no account check or sign-in has started.','无法读取启动器连接配置，请手动设置。':'Could not read launcher settings. Configure the connection manually.','无法订阅登录状态，请重新打开操控端。':'Could not subscribe to sign-in events. Reopen the operator.',
    'Runtime 工作进程':'Runtime worker','尚未读取':'Not read yet','读取后显示后台状态。':'Read the worker status to get started.','启动后台':'Start worker','停止后台':'Stop worker','刷新':'Refresh','会话':'Conversations','新会话':'New conversation','当前会话':'Current conversation','给 AI 发消息':'Message your AI','有什么想聊的？':'What would you like to talk about?','消息发送到当前 Agent。':'Messages go to the selected Agent.','发送':'Send','当前 Agent':'Selected Agent','选择 Agent':'Choose an Agent','新 Agent 标识':'New Agent ID','创建':'Create','订阅引擎':'Subscription engine','模型':'Model','使用订阅支持的模型':'A model supported by your subscription','保存模型':'Save model','活跃状态':'Activity mode','活跃':'Active','安静':'Quiet','应用状态':'Apply mode',
    '活跃：响应消息并自主行动。安静：只响应直接消息。暂停：当前轮结束后，不再开始新一轮。':'Active: messages and autonomous activity. Quiet: direct messages only. Paused: finish the current turn, then start no new turns.','模型与引擎变更用于后续消息，不改写已排队的消息。':'Engine and model changes apply to future messages; queued messages keep their original settings.','自主活动':'Autonomous activity','后台运行中':'Worker running','正在运行一轮':'A turn is running','后台需要登录':'Worker needs sign-in','后台未运行':'Worker not running','可以继续聊天或调整后续使用的 Agent。':'You can chat or adjust the Agent used for future messages.','先完成订阅登录，再启动后台处理消息。':'Sign in to your subscription, then start the worker to process messages.','当前轮结束后应用活跃状态变更。':'Activity changes apply after the current turn.','当前没有正在运行的回复。':'No reply is currently running.','排队中':'Queued','正在回复':'Replying','已完成':'Completed','已中断':'Interrupted','展开完整回复':'Read full reply','取消排队':'Cancel queued message','这里还没有消息。开始一段对话吧。':'No messages yet. Start a conversation.','显示 Runtime 保留的近期会话，较早消息可能已按保留策略清理。':'Recent retained conversation is shown. Older messages may have expired under the retention policy.','已启用':'Enabled','未启用':'Disabled','关闭活动':'Disable activity','启用活动':'Enable activity','立即唤醒':'Wake now','尚未配置自主活动。':'No autonomous activity is configured.','已请求启动后台。':'Worker start requested.','后台已停止，账号与会话保留。':'Worker stopped. Accounts and conversations are retained.','已应用':'Applied','消息已排队。':'Message queued.','消息已排队，启动后台后处理。':'Message queued. Start the worker to process it.','Agent 标识使用字母、数字、下划线、冒号或短横线。':'Agent IDs use letters, numbers, underscores, colons, or hyphens.',
    '请先为这个 Agent 保存引擎与模型。':'Save an engine and model for this Agent first.','配置已被其他会话修改，请重新读取后再保存。':'Another session changed the settings. Reload before saving.','请输入有效消息，最多 16 KiB。':'Enter a valid message, up to 16 KiB.','这条消息的标识已有不同内容，请新建消息。':'This message ID has different content. Create a new message.','请求超时，结果尚未确认。重试同一条消息会复用原标识。':'Request timed out; its result is unknown. Retrying the same message reuses its original ID.','无法连接调控通道，请检查 SSH 和远端版本。':'Cannot reach the control channel. Check SSH and the remote version.','此部署尚未提供后台进程控制，请检查 Docker 部署。':'Worker control is unavailable. Check the Docker deployment.','请明确填写账号、会话与发送者，不支持通配授权。':'Specify the account, conversation, and sender. Wildcard authorization is not supported.','请先完成或取消当前登录。':'Finish or cancel the current sign-in first.','操作未完成，请重新读取状态。':'The action did not complete. Read the status again.',
    '从获准的会话管理 Runtime':'Manage Runtime from an authorized conversation','切换、启用或暂停 Agent，切模型与查看状态。指令直接由 Runtime 处理，不交给 AI。':'Switch, enable, or pause Agents, change models, and read status. Runtime handles commands directly, without a model.','默认关闭。只接受已授权发送者在指定会话中发送的完整纯文本指令；不执行转发、引用、截断内容或超过十分钟的旧消息。':'Off by default. Only complete plain-text commands from an authorized sender in the exact conversation are accepted. Forwarded, quoted, truncated, or over-ten-minute-old messages are rejected.','启用会话指令':'Enable conversation commands','添加授权会话':'Add authorized conversation','重新读取':'Reload','保存指令配置':'Save command settings','最近的指令':'Recent commands','会话标识':'Conversation ID','允许的 QQ 发送者':'Allowed QQ sender','指令前缀':'Command prefix','初始 Agent':'Initial Agent','会话类型':'Conversation type','指定用户的私聊':'Private chat with one user','指定群与指定发送者':'One group and one sender','指令回执语言':'Confirmation language','启用这条授权':'Enable this authorization','向原会话发送执行回执':'Send confirmation to the same conversation','允许的管理操作':'Allowed management actions','移除此授权':'Remove authorization','查看帮助':'Help','查看状态':'Read status','切换 Agent':'Switch Agent','切换模型':'Change model','启用 Agent':'Enable Agent','暂停 Agent':'Pause Agent','只响应直接消息':'Direct messages only','唤醒自主活动':'Wake an activity','指令只管理 Runtime，不用于给 AI 发自然语言任务。':'Commands manage Runtime; they are not natural-language tasks for an AI.','有未保存的指令配置。':'You have unsaved command settings.','还没有收到获准的指令。':'No authorized commands have been received.','QQ 实时投递尚未完成实号验收；规则默认关闭。':'Live QQ delivery has not passed real-account acceptance. Rules are off by default.','指令配置已保存，不会启动登录。':'Command settings saved. No sign-in was started.',
    '外观与语言':'Appearance and language','只保存语言、配色和密度，账号凭据由 Runtime 保管。':'Only language, colors, and density are stored here. Runtime keeps account credentials.',
    '验证信息已提交，等待核心确认…':'Verification submitted; waiting for Runtime…','等待你在操控端提供验证信息。':'Waiting for verification input here.','请在系统浏览器完成官方授权，返回这里继续。':'Complete authorization in your system browser, then return here.','请在登录验证窗口中手动完成验证。':'Complete verification in the sign-in window.','请在登录窗口扫码，并在手机上确认。':'Scan the QR code and confirm on your phone.','等待平台确认，核心保持当前认证会话。':'Waiting for the platform; the current authentication session stays open.','已核验并复用原有登录态':'Existing sign-in verified and reused','登录已确认，账号状态已保存':'Sign-in confirmed and account state saved','验证会话已关闭，已有登录态保留。':'Verification closed; existing sign-in was retained.','操作已结束，请检查账号状态。':'The action ended. Check the account status.',
    '登录窗口不可用':'Sign-in window unavailable','无法连接 Runtime。请检查程序路径、状态目录；SSH 需预先配置密钥/agent，并在终端确认主机指纹。':'Cannot connect to Runtime. Check executable and state paths, configure SSH keys or an agent, and verify the host key in a terminal.','已有登录正在进行，请完成或取消后再操作。':'A sign-in is already active. Finish or cancel it first.','连接方式、状态目录或账号别名无效。':'Invalid connection mode, state directory, or account alias.','请填写已配置的 SSH 主机名。':'Enter a configured SSH host.','Runtime 程序路径无效。':'The Runtime executable path is invalid.','连接参数或登录方式无效。':'Invalid connection parameters or sign-in method.','缺少连接参数。':'Connection parameters are missing.','管理操作仅允许打包的本地主界面调用。':'Management is only available from the packaged local main window.'
  };
  const inverse = new Map(Object.entries(en).map(([zh, value]) => [value, zh]));
  let language; try { language = localStorage.getItem('glimlink.operator.language'); } catch (_) {}
  if (!['en', 'zh-CN'].includes(language)) language = navigator.language?.toLowerCase().startsWith('zh') ? 'zh-CN' : 'en';
  const origins = new WeakMap(); const attributes = new WeakMap();
  function text(s) {
    if (language === 'zh-CN') return inverse.get(s) || s;
    if (en[s]) return en[s];
    const status = s.match(/^(Bilibili|QQ|贴吧|微信|GitHub|X|Codex|Antigravity) · (.+)$/);
    if (status) return (en[status[1]] || status[1]) + ' · ' + text(status[2]);
    const replacements = [
      [/^通过 SSH 控制 (.+)$/, 'Control $1 over SSH'], [/^账号别名 (.+) 尚未登录$/, 'Not signed in · profile $1'],
      [/^检查 (.+) 状态$/, 'Check $1 status'], [/^(.+) 标志$/, '$1 logo'], [/^(.+) · 登录中$/, '$1 · signing in'],
      [/^(.+) · 检查未完成 \((.+)\)$/, '$1 · check failed ($2)'], [/^(.+) · 未完成 \((.+)\)$/, '$1 · not completed ($2)'],
      [/^在官方授权页输入临时代码：(.+)$/, 'Enter this temporary code on the provider page: $1'],
    ];
    for (const [pattern, replacement] of replacements) if (pattern.test(s)) return s.replace(pattern, replacement);
    return s;
  }
  function localize() {
    const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    while (walk.nextNode()) {
      const n = walk.currentNode; if (!n.parentElement || n.parentElement.closest('[translate="no"],script,style,textarea')) continue;
      const prior = origins.get(n); const source = prior && n.data === prior.last ? prior.source : n.data; const trimmed = source.trim(); if (!trimmed) continue;
      const translated = source.replace(trimmed, text(trimmed)); origins.set(n, { source, last: translated }); if (n.data !== translated) n.data = translated;
    }
    document.querySelectorAll('[title],[placeholder],[aria-label]').forEach((n) => {
      if (n.closest('[translate="no"]')) return;
      let saved = attributes.get(n); if (!saved) { saved = {}; attributes.set(n, saved); }
      for (const key of ['title', 'placeholder', 'aria-label']) if (n.hasAttribute(key)) { const value = n.getAttribute(key); const old = saved[key]; const source = old && value === old.last ? old.source : value; const next = text(source); saved[key] = { source, last: next }; if (value !== next) n.setAttribute(key, next); }
    });
    if (document.documentElement.lang !== language) document.documentElement.lang = language;
    document.querySelectorAll('[data-language-choice]').forEach(n => { const value=String(n.dataset.languageChoice === language); if(n.getAttribute('aria-pressed')!==value)n.setAttribute('aria-pressed',value); });
  }
  function set(value) { if (!['en', 'zh-CN'].includes(value)) return; language = value; try { localStorage.setItem('glimlink.operator.language', language); } catch (_) {} localize(); window.dispatchEvent(new Event('operator:language')); }
  window.GlimlinkI18n = { text, language: () => language, set, refresh: localize };
  let queued = false;
  const observer = new MutationObserver(() => { if (queued) return; queued = true; queueMicrotask(() => { queued = false; localize(); }); });
  localize(); observer.observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ['title', 'placeholder', 'aria-label'] });
  document.querySelectorAll('[data-language-choice]').forEach(n => n.addEventListener('click', () => set(n.dataset.languageChoice)));
})();
