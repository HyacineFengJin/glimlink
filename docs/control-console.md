# Control and scoped Runtime commands

The optional operator has four independent pages: **Connections**, **Control
(conversations)**, **Commands**, and **Settings**, in English or Simplified
Chinese. Only language and fixed appearance preferences persist in browser
storage. Conversations and cached account labels stay in private Runtime state.
The Settings memory card measures the connected Runtime. Docker deployments use
fixed host-side cgroup reads over SSH (current, peak, limit), excluding the operator
and other VPS services, without a resident sampler. Local native deployments use
the verified Runtime process tree. Hidden pages stop polling; failed reads are
reported as unavailable and never replaced with local operator measurements.

Cached provider names are display data, not proof that a sign-in is still valid.
A provider that supplies no name is shown honestly. Codex labels are read only by
the human operator; the ordinary model/probe account DTO still excludes identity.

Control reads retained dialogue and queued/running/completed messages. A message
enqueues a managed turn under an explicit persona, Agent and conversation. An
uncertain retry reuses its message ID. Larger replies can be expanded. This is
Runtime's bounded recent history, not an unlimited mirror of native CLI logs.

Profiles select Codex or Antigravity and a subscription model ID. New profiles
inherit the main profile's tool/account limits, or memory-only tools if no main
profile exists. Model edits preserve tool limits and affect future messages;
queued turns keep their original execution profile.

Activity modes apply after the current turn:

- **Active**: direct messages and configured source/autonomous work.
- **Quiet**: direct operator messages only.
- **Paused**: no new model turn. The command receiver keeps running.

The Docker deployment offers fixed worker start/stop/status operations. Its real
container status takes precedence over a saved heartbeat in the SSH desktop.
Native manual deployments keep their service manager; unsupported worker control
is reported explicitly. Stopping a worker can interrupt a turn; interrupted
actions are not automatically replayed. Platform login still needs the worker
stopped when it holds the same kernel; conversation/profile/mode management does not.

## Private management protocol

`agent-runtime --state PATH operator manage` uses short store transactions over
local stdio or authenticated SSH without acquiring the Runtime kernel lock. Each
JSONL request contains `version: 1`, a positive `id`, and a typed `command`.
Requests are bounded at 64 KiB, responses at 512 KiB. There is no arbitrary shell,
method name, input-file path, or model-facing management tool. SSH retains strict
host verification and noninteractive key authentication. Desktop commands are
restricted to the packaged main document.

```json
{"version":1,"id":1,"command":{"action":"snapshot"}}
{"version":1,"id":2,"command":{"action":"mode","agent":"main","mode":"paused"}}
```

## Commands from an authorized conversation

These are Runtime management actions, not natural-language AI tasks. Runtime
parses and authorizes them before any model work. Ordinary AI conversation keeps
its normal messaging path.

Commands are off by default. The first receiver is QQ. A rule specifies an exact
Runtime account, private peer or group, sender UIN, prefix, initial target Agent,
allowed verbs, response language and optional confirmation reply. A private rule
requires the peer and sender to be the same UIN; a group rule requires both the
exact group and sender. There are no wildcards or implicit administrator grants.
Only the human management endpoint edits rules.

```text
/gl help
/gl status
/gl agent main
/gl model codex gpt-6.1-terra
/gl enable
/gl disable
/gl quiet
/gl wake browsing
```

Only checked verbs are accepted. `/agent` changes this rule's selected target and
the operator selection. `/disable` pauses AI turns while retaining command
reception; `/enable` resumes them. There is no `/ask`, shell execution or command
for granting permissions. Activities retain their existing read/write limits.

Enabled rules install bounded incoming-only QQ watches with a fresh baseline.
The worker must remain running and QQ must be authenticated. Outgoing, quoted,
forwarded, unresolved, truncated, pre-configuration and over-ten-minute-old
messages are rejected. Authorization uses native conversation/sender metadata,
never a display name or a field in message text. A bounded receipt journal
reserves attempts before applying changes. Retained attempts, unknown outcomes
and failed confirmations are not automatically repeated. Optional replies use
the original account-bound conversation reference and durable operation ID.

QQ login remains blocked in current real-account acceptance. Authorization,
watch integration and confirmation behavior have isolated tests; live QQ command
delivery is not claimed as accepted. Weixin remains paused.
