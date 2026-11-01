# runtime-jobs

A synchronous, disk-backed queue for the single-model Agent Runtime host. It has
no executor, networking, browser, scheduler, event polling, or platform retry.
Dependencies are `runtime-core`, `serde`, and `serde_json`.

The embedding host may also enqueue a typed `ActivityWake` with no task prompt
or observed event, plus a frozen execution profile. This is a single autonomous
model turn, not a task objective. Ordinary tasks still require a nonempty prompt.
The queue's one-running-turn rule and action progress apply to both origins;
timers, standing permissions and bounded activity notes belong to the host.

Each method takes `jobs/jobs.lock` for one bounded filesystem transaction and
releases it before returning. A model turn never holds this queue lock. A CLI can
enqueue or inspect jobs while the daemon is running a turn. Lock contention waits
up to two seconds and then reports a busy error. The host's own exclusive Runtime
lock is separate and does not replace the queue transaction lock.

```rust,ignore
let queue = Queue::open(state)?;
queue.enqueue("request-1", "Read my latest notifications", None, now)?;
if let Some(claim) = queue.claim_next(now)? {
    queue.update_progress(&claim.lease, Progress {
        thread_id: Some(thread_id),
        turn_id: Some(turn_id),
        operation_ids: vec![operation_id],
        tool_calls: 1,
    }, now)?; // must succeed before executing the corresponding tool
    queue.finish(&claim.lease, Outcome::completed("Task result"), now)?;
}
```

`get` and `list` are read-only transactions and never recover or modify a live
Running job. They return `Job`, which contains no lease token. `claim_next` returns
`ClaimedJob { job, lease }`. The lease is a random private capability, redacted in
Debug, required by `update_progress` and `finish`. A stale lease cannot finish a
different job, a terminal job, or a new claim after explicit history GC and ID
reuse. Only one Running job can exist across queue instances/processes. Pending
jobs are claimed by `(created_at, id)`; `list(limit)` returns newest first.
`claim_job(id, now)` claims exactly the requested Pending job and leaves FIFO
neighbors untouched; it returns None while any model task is Running.

Re-enqueueing an existing ID compares prompt, event, context and execution policy and returns the
original job in its current state. Creation time is excluded from this comparison.
Different content is rejected. This never resets Completed, Failed, Interrupted,
or NeedsAuth back to Pending. Use a fresh job ID for a fresh logical request.

`enqueue_configured` optionally freezes an operator `ExecutionProfile`: revision,
engine, model, tool allowlist and platform default account aliases. Existing
methods keep this field None for compatibility. Profile edits cannot mutate
queued jobs; recording an engine that disagrees with a frozen profile is refused.
The queue validates structure and bounds; the host validates registered tool
names and routes the exact job to its selected native engine.

The sole exception is an explicit human `resume_needs_auth(ids, now)` after
subscription login. It accepts only NeedsAuth jobs with no thread identity, turn
identity, tool calls, or recorded operation IDs. It clears their claim and finish
markers while retaining ID, prompt, event, and original creation time. Every ID is
validated before applying the request. Interrupted/Failed jobs and NeedsAuth jobs
with execution evidence are refused; this method must never be an automatic
authentication retry. A new claim receives a new lease token.

`cancel_pending(ids, now)` is an explicit operator withdrawal. It accepts only
Pending jobs or NeedsAuth jobs with the same empty execution-evidence requirement.
It validates the entire ID set before writing fixed Canceled markers. A concurrent
claim either wins first (cancellation is then refused) or observes the Canceled
state and does not start. Cancellation never preempts Running/Interrupted work or
requeues a task, and Canceled is never claimed, recovered, or authentication-resumed.

Progress keeps one model thread identity, the latest supplied turn identity,
monotonically merged operation IDs, and the maximum tool-call count. It stores no
full model turn, tool arguments, cookies, tokens, or frozen platform request.
The caller must record an assigned/explicit operation ID before executing a write
tool. Model thread IDs are not mutable once bound. Store/model tools should project
safe summaries rather than expose the entire private prompt/event indiscriminately.

`finish` accepts Completed, Failed, NeedsAuth, or Interrupted. Completed requires
text; the others require an error/reason and may keep bounded partial text.
Completed may also carry a bounded warning, such as a notice that its output was
truncated. No terminal state is automatically retried or made Pending.

At daemon startup, after obtaining the host's exclusive Runtime lock and ensuring
the old worker has exited, call `recover_interrupted(now)`. It turns the sole
Running record into Interrupted while retaining thread/turn/operation progress.
It never replays that task. An engine disconnection during normal execution should
use `finish(lease, Outcome::interrupted(reason), now)` instead of global recovery.
Do not call recovery from status commands or periodically against a live worker.
This startup ownership precondition belongs to the embedding host.

Job files and the lock are regular 0600 files in a private directory. Individual
updates use new private temporary files, file fsync, rename, and directory fsync.
Known unpublished temporary files can be cleaned only by mutating transactions;
read-only transactions leave them alone. Invalid records, unknown entries,
symlinks, broad permissions, multiple Running records, and exceeded bounds fail
closed instead of being silently skipped.

Hard limits:

- 128 persisted jobs, with no automatic deletion of history or unfinished work.
- 16 MiB total queue bytes, including an atomic update's temporary copy.
- 16 KiB prompt, 32 KiB serialized event, event depth 64 / 8192 nodes.
- 64 KiB result or partial text, 8 KiB error/reason/warning.
- 128 operation IDs of at most 96 ASCII characters; model identities at most 256 bytes.
- 640 KiB encoded record and at most 257 scanned directory entries.

Admission and progress updates reserve one maximum-sized record of disk headroom,
so concurrent enqueues cannot consume the space needed to finish the Running job.
A terminal result can consume that reserve; further admission may require explicit
GC. `gc_finished(ids)` validates the entire explicit ID set, then removes only
Completed/Failed/Canceled records. Pending, Running, Interrupted, and NeedsAuth records are
never deleted through this API. Timestamps must not precede the last durable
update; clock rollback reports an error without changing state.

The offline tests cover idempotency, concurrent claims, CLI reads/enqueue during a
turn, durable pre-tool progress, interrupted recovery, stale leases, explicit
interruption, invalid transitions, count/disk limits, private file validation,
and unpublished temporary-file cleanup.
