# 持久 Agent 执行岗位

`persona` 选择共同人格，`agent` 选择岗位，`conversation` 选择近期对话。角色文字继续由 `mind role` 管理；执行配置由 operator CLI 管理，模型没有修改它的工具。不同岗位可以共享同一个人格及长期记忆，同时使用不同引擎、模型、工具和默认账号。

每个 Runtime 的私有 `agent-profiles/profiles.json` 保存最多 32 个岗位、总计最多 64 KiB。它属于本 Runtime 的执行配置，`--mind-state` 不会把账号别名、引擎选择或队列共享给另一台 Runtime。

## 配置与入队

先准备权限 0600 的 JSON，例如 `/state/reader-profile.json`：

```json
{
  "engine": "codex",
  "model": null,
  "allowed_tools": ["runtime_memory", "social_notifications", "social_open", "social_discussion"],
  "default_accounts": {"bilibili": "personal", "tieba": "personal"}
}
```

`model: null` 使用所选引擎默认模型；填写实际可用的模型 ID 时固定该模型。`allowed_tools: null` 允许完整 Runtime 工具目录，空数组表示没有业务工具。只接受已注册的工具名称。默认账号支持 `bilibili/tieba/qq/github/x`，值是本 Runtime 已有账号别名，不是凭据。

```sh
agent-runtime --state /state agent profile-set reader /state/reader-profile.json 0
agent-runtime --state /state agent profile reader
agent-runtime --state /state agent profiles
agent-runtime --state /state --persona main --agent reader agent enqueue read-001 /state/task.txt
```

`0` 为首次创建，后续可传当前 revision 做版本检查；未传版本时由 operator 显式替换。修改岗位只影响以后入队/观察到的任务。入队的 `execution_profile` 完整冻结版本、引擎、模型、工具白名单和默认账号；相同任务 ID 不接受不同快照。可以更新同一岗位为 `engine: antigravity`，也可以给另一个 agent 配置不同引擎；两者仍选择同一个 `persona`。

watch 从 `context.agent` 选择岗位，在观察事件前冻结当前配置。已落盘的待提交批次、延后事件及已入队任务保留原快照，后续新事件使用新版本。平台内容不能选择岗位或更改快照。

## 串行执行

```sh
agent-runtime --state /state agent run \
  --codex-binary /opt/agent/bin/codex-native \
  --antigravity-binary /opt/agent/bin/antigravity-native
```

daemon 根据每条任务快照路由到 Codex 或 Antigravity，并按任务 ID 精确认领。队列仍只允许一个 Running 模型任务；执行完成即退出引擎，不增加常驻模型进程。订阅登录仍分别保存在各引擎专用目录，岗位配置不读取或复制令牌。

未配置岗位的旧任务保留 `agent run --engine/--model/--tools` 的原行为。配置了岗位时，引擎和模型取冻结快照；daemon 的 `--tools` 还可进一步收窄工具集合。`--binary` 覆盖 daemon 基础引擎的可执行文件；两个独立的 `--codex-binary/--antigravity-binary` 适合混合队列。内置路径分别为 `/cache/bin/codex-native` 和 `/cache/bin/antigravity-native`。

默认账号只补充全新调用：显式 `account`、不透明目标/回复/图片引用、资源引用或 continuation 都保留自身归属。岗位默认值不能把已读取对象改到另一个账号。工具白名单只限制可调用工具，不是按平台账号划分的访问控制列表。

真实平台动作继续使用同一持久操作账本；岗位修改、跨引擎执行和任务重启都不会自动重放未知写入。

## 验证

Docker 测试使用合成账号状态、原生 Codex app-server fixture 和 Antigravity ACP fixture，不连接平台或模型服务。覆盖 operator 配置、版本冲突、冻结队列快照、观察事件后修改配置、引用账号不被覆盖、两个原生引擎的精确认领和混合队列 daemon 路由。

## 自主活动与上下文

自主活动独立于用户 Task：没有任务提示词、交付物或完成目标。操作员配置人格、执行岗位和可用能力，AI 在唤醒时选择浏览、追踪兴趣、参与或休息。内部复用单个执行槽及持久记录，`activity` 字段标记唤醒来源，`prompt` 为空；平台消息不能冒充这种来源。

先为 `main` 设置执行岗位（引擎、模型、工具及默认账号），再用权限 `0600` 的配置文件启用活动，例如：

```json
{
  "id": "browsing",
  "context": {"persona": "main", "agent": "main"},
  "enabled": true,
  "interval_seconds": 300,
  "max_turns_per_day": 24,
  "read_only": true
}
```

```sh
agent-runtime --state /state agent activity-set /state/browsing.json
agent-runtime --state /state agent activities
agent-runtime --state /state agent run
```

未指定 conversation 时使用 `activity:<id>`。未配置活动时不产生自主模型调用。等待中的普通任务优先；自主活动只在执行队列空闲时预约一个回合。时间间隔下限 60 秒，上限一天；次数预算使用从首次预约开始的 24 小时窗口，预约即计数。AI 的 `rest_seconds` 可延后下一次唤醒，不能缩短操作员间隔或增加预算。等待期间不启动模型、不反复重写活动状态。当前平台轮询与模型执行仍串行；本轮没有增加并发模型或异步媒体服务。

`read_only` 默认开启，在岗位工具集合上进一步去除平台写入；本地记忆仍可更新。操作员显式设为 false 后，可在岗位白名单及已有授权范围内自主使用发送、回复、点赞等工具，沿用原操作账本与回读，不增加逐次确认层。配置不改变已经预约回合的冻结工具快照；关闭活动阻止未来预约，已预约记录可用原 `agent cancel ID` 撤回，运行中的回合沿用原中断机制。

上下文借鉴 [Super-agent 的有界读取与引用组织](https://github.com/deepseek-ai/dsh-superagent-prototype/blob/main/docs/007-architecture.md)，以本项目 Rust 代码独立实现，没有引入其运行环境：

- 每轮直接注入短摘要、兴趣、未解问题、内容锚点及最近两条经历预览。AI 可以不更新笔记，不要求每轮产生总结。
- `runtime_memory` 的 `activity_get` 读取笔记与引用，`activity_save` 以 `expected_revision` 更新当前活动笔记，`activity_read` 按经历 ID 及 UTF-8 字节偏移展开旧记录。预览明确为摘录，模型生成的摘要明确为记忆数据。
- 经历保留前驱引用、引擎、状态、当轮版本笔记快照及操作 ID；队列 GC 不影响已归档经历。平台内容通过原工具沿锚点重开，过期引用不会被当成仍然有效。事实、观点和未解问题须区分，模型笔记不成为授权或动作回执。
- 最多 8 个活动，每个保留 8 条经历、每条最多 8 KiB，经历正文单次展开最多 4 KiB，并附带当轮笔记快照；每份笔记编码最多 6 KiB，整个活动文件最多 1 MiB。裁剪与淘汰明确返回，不声称保留全部历史；长期重要内容可进入已有共享记忆。没有常驻向量索引或第二个摘要模型。
- 同一个 Runtime 中，Codex 与 Antigravity 通过相同装配路径共用人格、长期记忆及活动笔记。另一台 Runtime 的 `--mind-state` 只共享人格/长期记忆；活动记录仍属于本地 Runtime。

失败、登录失效或进程中断会停住该活动，不自动重放该回合或启动登录。检查后可用 `agent activity-wake browsing` 显式恢复；该命令仍遵守次数预算。

有限互动可在 `read_only:false` 时添加 `"interaction_limits":{"likes":4,"comments":2}`。这是同一活动、跨引擎和回合共用的 24 小时上限，不是发言指标；目前该模式只允许已登录 B 站账号对明确视频点赞、对视频或精确评论目标发短文字评论（最多 240 字）。配合岗位工具白名单使用。发送前持久预约额度，失败或结果不明也计数；同一操作 ID 重读不重复扣除，不能改成另一目标或动作。窗口重置不更新旧内容引用的有效期。减少上限或切回只读会约束当前回合后续写入。
