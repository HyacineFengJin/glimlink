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

`model: null` 使用所选引擎默认模型；填写实际可用的模型 ID 时固定该模型。`allowed_tools: null` 允许完整 Runtime 工具目录，空数组表示没有业务工具。只接受已注册的工具名称。默认账号支持 `bilibili/tieba/qq/github`，值是本 Runtime 已有账号别名，不是凭据。

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
