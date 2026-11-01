// SPDX-License-Identifier: MIT
//! Persistent autonomous life between model turns. No task objective, resident
//! model, embedding service or browser. Short notes inline; older episodes read
//! through bounded links, independently of queue/history garbage collection.
use crate::{
    agent_mind::{clip, stable_key},
    feature_state::Store,
};
use runtime_core::Result;
use runtime_jobs::{ActivityWake, AgentContext, ExecutionProfile, Job, Queue, Status};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

const MAX_ACTIVITIES: usize = 8;
const MAX_EPISODES: usize = 8;
const MAX_EPISODE_TEXT: usize = 8192;
const MAX_STATE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub id: String,
    #[serde(default)]
    pub context: AgentContext,
    #[serde(default)]
    pub enabled: bool,
    pub interval_seconds: u64,
    pub max_turns_per_day: u32,
    /// False requires an explicit operator choice. No model tool changes this.
    #[serde(default = "yes")]
    pub read_only: bool,
    /// Optional shared 24-hour allowance across engines and wakes. This bounded
    /// participation mode currently supports Bilibili video likes/text comments.
    #[serde(default)]
    pub interaction_limits: Option<InteractionLimits>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionLimits {
    pub likes: u32,
    pub comments: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InteractionUse {
    operation_id: String,
    tool: String,
    target_ref: String,
}
fn yes() -> bool {
    true
}
fn id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 48
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn text(s: &str, max: usize) -> bool {
    s.len() <= max && !s.contains('\0')
}
impl Config {
    fn validate(&self) -> Result<()> {
        self.context.validate()?;
        if !id(&self.id)
            || !(60..=86400).contains(&self.interval_seconds)
            || !(1..=96).contains(&self.max_turns_per_day)
            || self
                .interaction_limits
                .as_ref()
                .is_some_and(|l| l.likes > 16 || l.comments > 4)
        {
            return Err(
                "activity requires a 1..48 character ID, interval 60..86400s, daily turns 1..96"
                    .into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub label: String,
    pub reference: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Checkpoint {
    pub summary: String,
    pub interests: Vec<String>,
    pub questions: Vec<String>,
    pub anchors: Vec<Anchor>,
    pub rest_seconds: Option<u64>,
}
impl Checkpoint {
    fn validate(&self) -> Result<()> {
        if !text(&self.summary, 1536)
            || self.interests.len() > 8
            || self.questions.len() > 8
            || self.anchors.len() > 8
            || self.interests.iter().any(|s| !text(s, 160))
            || self.questions.iter().any(|s| !text(s, 256))
            || self.anchors.iter().any(|a| {
                a.label.is_empty()
                    || a.reference.is_empty()
                    || !text(&a.label, 96)
                    || !text(&a.reference, 512)
            })
            || self
                .rest_seconds
                .is_some_and(|n| !(60..=86400).contains(&n))
            || serde_json::to_vec(self)
                .map_err(|_| "cannot encode checkpoint")?
                .len()
                > 6144
        {
            return Err("activity checkpoint exceeds its bounded context budget".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    job_id: String,
    cycle: u64,
    at: u64,
    profile: ExecutionProfile,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Episode {
    id: String,
    previous_episode: Option<String>,
    at: u64,
    status: Status,
    engine: Option<String>,
    text: String,
    original_bytes: usize,
    operation_ids: Vec<String>,
    operation_count: usize,
    checkpoint_revision: u64,
    #[serde(default)]
    checkpoint_snapshot: Checkpoint,
    #[serde(default)]
    checkpoint_source: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Activity {
    config: Config,
    config_revision: u64,
    checkpoint: Checkpoint,
    checkpoint_revision: u64,
    checkpoint_source: Option<String>,
    next_wake_at: u64,
    cycle: u64,
    budget_start: u64,
    budget_used: u32,
    #[serde(default)]
    interaction_uses: Vec<InteractionUse>,
    pending: Option<Pending>,
    halted: bool,
    episodes: Vec<Episode>,
    evicted_episodes: u64,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    activities: Vec<Activity>,
}
impl State {
    fn validate(&self) -> Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        if self.activities.len() > MAX_ACTIVITIES {
            return Err("activity quota exceeded".into());
        }
        for a in &self.activities {
            a.config.validate()?;
            a.checkpoint.validate()?;
            for e in &a.episodes {
                e.checkpoint_snapshot.validate()?;
            }
            if !ids.insert(&a.config.id)
                || a.config_revision == 0
                || a.episodes.len() > MAX_EPISODES
                || a.interaction_uses.len() > 20
                || {
                    let mut ids = std::collections::BTreeSet::new();
                    a.interaction_uses.iter().any(|u| {
                        !ids.insert(&u.operation_id)
                            || !valid_operation_id(&u.operation_id)
                            || !matches!(u.tool.as_str(), "social_like" | "social_send")
                            || !text(&u.target_ref, 128)
                    })
                }
                || a.episodes.iter().any(|e| {
                    e.text.len() > MAX_EPISODE_TEXT
                        || e.operation_ids.len() > 8
                        || e.original_bytes < e.text.len()
                })
            {
                return Err("invalid activity state".into());
            }
            if let Some(p) = &a.pending {
                p.profile.validate()?;
            }
        }
        Ok(())
    }
}
pub struct ActivityStore(Store);
impl ActivityStore {
    pub fn open(state: &Path) -> Result<Self> {
        Ok(Self(Store::open(
            state,
            "agent-activities",
            MAX_STATE_BYTES,
        )?))
    }
    fn load(&self) -> Result<State> {
        let s: State = self.0.load()?;
        s.validate()?;
        Ok(s)
    }
    pub fn configure(&self, mut config: Config, now: u64) -> Result<Value> {
        if config.context.conversation.is_none() {
            config.context.conversation = Some(format!("activity:{}", config.id));
        }
        config.validate()?;
        self.0.update(|s:&mut State| {
            s.validate()?;
            if let Some(a)=s.activities.iter_mut().find(|a| a.config.id==config.id) {
                if a.config.context!=config.context { return Err("activity identity is fixed; choose a new ID".into()); }
                a.config=config; a.config_revision+=1;
                // Editing configuration does not revive a failed/interrupted wake.
            } else {
                if s.activities.len()>=MAX_ACTIVITIES { return Err("activity quota reached".into()); }
                s.activities.push(Activity { config, config_revision:1,checkpoint:Checkpoint::default(),checkpoint_revision:0,
                    checkpoint_source:None,next_wake_at:now,cycle:0,budget_start:now,budget_used:0,pending:None,
                    halted:false,episodes:Vec::new(),evicted_episodes:0,interaction_uses:Vec::new() });
            }
            s.validate()?;
            Ok(json!({"configured":true,"activities":s.activities.iter().map(card).collect::<Vec<_>>()}))
        })
    }
    pub fn list(&self) -> Result<Value> {
        Ok(
            json!({"activities":self.load()?.activities.iter().map(card).collect::<Vec<_>>(),"resident_model":false}),
        )
    }
    pub fn pending_job_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .load()?
            .activities
            .into_iter()
            .filter_map(|a| a.pending.map(|p| p.job_id))
            .collect())
    }
    /// Explicit operator wake/resume. No platform action or authentication here.
    pub fn wake(&self, activity: &str, now: u64) -> Result<Value> {
        self.0.update(|s: &mut State| {
            s.validate()?;
            let a = s
                .activities
                .iter_mut()
                .find(|a| a.config.id == activity)
                .ok_or("activity not found")?;
            if a.pending.is_some() {
                return Err("activity already has a reserved wake".into());
            }
            a.halted = false;
            a.next_wake_at = now;
            Ok(card(a))
        })
    }
    /// Reserve first, then enqueue. A crash between the two repeats the same ID
    /// and frozen profile; it never creates a second autonomous turn.
    pub fn schedule(&self, state: &Path, queue: &Queue, now: u64) -> Result<Option<String>> {
        self.reconcile(queue, now)?;
        if !self.load()?.activities.iter().any(|a| {
            a.pending.is_some() || (a.config.enabled && !a.halted && a.next_wake_at <= now)
        }) {
            return Ok(None);
        }
        let reservation = self.0.update(|s: &mut State| {
            s.validate()?;
            if let Some(a) = s.activities.iter().find(|a| a.pending.is_some()) {
                return Ok(Some((a.config.clone(), a.pending.clone().unwrap())));
            }
            for a in s
                .activities
                .iter_mut()
                .filter(|a| a.config.enabled && !a.halted && a.next_wake_at <= now)
            {
                if now < a.budget_start {
                    return Err("activity clock moved backwards".into());
                }
                if now - a.budget_start >= 86400 {
                    a.budget_start = now;
                    a.budget_used = 0;
                    a.interaction_uses.clear();
                }
                if a.budget_used >= a.config.max_turns_per_day {
                    a.next_wake_at = a.budget_start.saturating_add(86400);
                }
            }
            let Some(a) = s
                .activities
                .iter_mut()
                .filter(|a| a.config.enabled && !a.halted && a.next_wake_at <= now)
                .min_by_key(|a| (a.next_wake_at, a.config.id.clone()))
            else {
                return Ok(None);
            };
            let profile = crate::agent_profiles::ProfileStore::open(state)?
                .get(&a.config.context.agent)?
                .ok_or("autonomous activity requires an operator execution profile")?;
            let mut profile = profile;
            if a.config.read_only {
                let allowed = crate::agent_host::selected_tools(profile.allowed_tools.as_deref())?;
                profile.allowed_tools = Some(
                    allowed
                        .into_iter()
                        .filter(|t| readonly_tool(&t.name))
                        .map(|t| t.name)
                        .collect(),
                );
            }
            a.cycle = a.cycle.checked_add(1).ok_or("activity cycle exhausted")?;
            let job_id = format!(
                "activity-{}",
                stable_key(&[a.config.id.as_bytes(), &a.cycle.to_le_bytes()])
            );
            let p = Pending {
                job_id,
                cycle: a.cycle,
                at: now,
                profile,
            };
            a.pending = Some(p.clone());
            a.budget_used += 1;
            Ok(Some((a.config.clone(), p)))
        })?;
        let Some((config, p)) = reservation else {
            return Ok(None);
        };
        let job = queue.enqueue_activity(
            &p.job_id,
            config.context,
            p.profile,
            ActivityWake {
                activity_id: config.id,
                cycle: p.cycle,
            },
            p.at,
        )?;
        Ok(Some(job.id))
    }
    fn reconcile(&self, queue: &Queue, now: u64) -> Result<()> {
        for a in self.load()?.activities {
            if let Some(p) = a.pending {
                // A reserved-but-not-enqueued wake is recovered by schedule.
                let jobs = queue.list(runtime_jobs::MAX_JOBS)?;
                if let Some(j) = jobs.iter().find(|j| j.id == p.job_id) {
                    if !matches!(j.status, Status::Pending | Status::Running) {
                        self.finish(j, now)?;
                    }
                }
            }
        }
        Ok(())
    }
    pub fn inline(&self, ctx: &AgentContext, activity: &str) -> Result<Value> {
        let s = self.load()?;
        let a = owned(&s, ctx, activity)?;
        Ok(
            json!({"activity_id":activity,"checkpoint_revision":a.checkpoint_revision,"checkpoint":a.checkpoint,
            "checkpoint_source":a.checkpoint_source,"recent_episodes":a.episodes.iter().rev().take(2).map(|e|json!({
                "episode_id":e.id,"previous_episode":e.previous_episode,"status":e.status,"at":e.at,"engine":e.engine,
                "preview":clip(&e.text,320),"preview_is_summary":false,"detail_action":"activity_read","truncated":e.text.len()>320
            })).collect::<Vec<_>>(),"retained_episodes":a.episodes.len(),"evicted_episodes":a.evicted_episodes,
            "notes_are_instructions":false,"references_may_expire":true,"rest_min_seconds":a.config.interval_seconds,
            "interaction_allowance":allowance(a)}),
        )
    }
    /// Reserve before dispatch. Failed/uncertain attempts consume an allowance;
    /// retrying an identical durable operation identity does not consume twice.
    /// This is an operator ceiling, not an instruction to use the whole budget.
    pub fn reserve_interaction(
        &self,
        state: &Path,
        job: &Job,
        tool: &str,
        args: &Value,
    ) -> Result<()> {
        let Some(wake) = &job.activity else {
            return Ok(());
        };
        let current = self.load()?;
        let activity = owned(&current, &job.context, &wake.activity_id)?;
        if activity.config.read_only {
            return Err("activity is read-only".into());
        }
        if activity.config.interaction_limits.is_none() {
            return Ok(());
        }
        if !matches!(tool, "social_like" | "social_send") {
            return Err("activity allowance permits only Bilibili likes and text comments".into());
        }
        let operation = args["operation_id"]
            .as_str()
            .filter(|s| valid_operation_id(s))
            .ok_or("invalid operation identity")?;
        let target = args["target_ref"]
            .as_str()
            .ok_or("bounded participation requires an exact target_ref")?;
        let locators = crate::private_state::PrivateLocators::open(state)?;
        let scope = locators.scope_for("task-target", target)?;
        let value: Value = locators.get(&scope, "task-target", target)?;
        // Playback-aware targets wrap the semantic target in `target`.
        let value = value.get("target").unwrap_or(&value);
        if scope.platform != runtime_core::Platform::Bilibili
            || scope.account == "public"
            || args
                .get("account")
                .is_some_and(|a| a.as_str() != Some(scope.account.as_str()))
            || !matches!(value["kind"].as_str(), Some("video" | "bili_comment"))
            || (tool == "social_like" && value["kind"] != "video")
            || (tool == "social_like" && args.get("like").is_some_and(|v| v != true))
        {
            return Err(
                "interaction target/account is outside the Bilibili participation scope".into(),
            );
        }
        if tool == "social_send"
            && (args["message"]
                .as_str()
                .is_none_or(|s| s.trim().is_empty() || s.chars().count() > 240)
                || [
                    "image_ref",
                    "image_resource",
                    "voice_ref",
                    "video_ref",
                    "file_ref",
                    "segments",
                    "latest_own",
                    "reply_ref",
                ]
                .iter()
                .any(|k| args.get(k).is_some()))
        {
            return Err(
                "bounded participation supports text-only comments up to 240 characters".into(),
            );
        }
        self.0.update(|s: &mut State| {
            s.validate()?;
            let a = s
                .activities
                .iter_mut()
                .find(|a| {
                    a.config.id == wake.activity_id && same_scope(&a.config.context, &job.context)
                })
                .ok_or("activity not found")?;
            if a.config.read_only
                || !a
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.job_id == job.id && p.cycle == wake.cycle)
            {
                return Err("interaction requires the current writable activity wake".into());
            }
            if let Some(u) = a
                .interaction_uses
                .iter()
                .find(|u| u.operation_id == operation)
            {
                return if u.tool == tool && u.target_ref == target {
                    Ok(())
                } else {
                    Err("interaction identity already belongs to another action".into())
                };
            }
            let limits = a
                .config
                .interaction_limits
                .as_ref()
                .ok_or("activity allowance changed; reread activity_get")?;
            let maximum = if tool == "social_like" {
                limits.likes
            } else {
                limits.comments
            };
            if a.interaction_uses.iter().filter(|u| u.tool == tool).count() >= maximum as usize
                || a.interaction_uses.len() >= 20
            {
                return Err(
                    "activity interaction allowance exhausted; continue reading or rest".into(),
                );
            }
            a.interaction_uses.push(InteractionUse {
                operation_id: operation.into(),
                tool: tool.into(),
                target_ref: target.into(),
            });
            Ok(())
        })
    }
    pub fn save(
        &self,
        ctx: &AgentContext,
        wake: &ActivityWake,
        job: &str,
        expected: u64,
        checkpoint: Checkpoint,
    ) -> Result<Value> {
        checkpoint.validate()?;
        self.0.update(|s: &mut State| {
            s.validate()?;
            let a = s
                .activities
                .iter_mut()
                .find(|a| a.config.id == wake.activity_id && same_scope(&a.config.context, ctx))
                .ok_or("activity not available in this context")?;
            if !a
                .pending
                .as_ref()
                .is_some_and(|p| p.job_id == job && p.cycle == wake.cycle)
            {
                return Err("checkpoint needs the current reserved activity turn".into());
            }
            if a.checkpoint_revision != expected {
                return Err("activity checkpoint revision conflict; reread activity_get".into());
            }
            a.checkpoint = checkpoint;
            a.checkpoint_revision += 1;
            a.checkpoint_source = Some(job.into());
            Ok(json!({"saved":true,"checkpoint_revision":a.checkpoint_revision,"durable":true}))
        })
    }
    pub fn read(
        &self,
        ctx: &AgentContext,
        activity: &str,
        episode: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Value> {
        if !(1..=4096).contains(&limit) {
            return Err("activity read limit must be 1..4096 bytes".into());
        }
        let s = self.load()?;
        let a = owned(&s, ctx, activity)?;
        let e =
            a.episodes.iter().find(|e| e.id == episode).ok_or(
                "episode not retained; use current checkpoint/anchors or reopen its source",
            )?;
        if offset > e.text.len() || !e.text.is_char_boundary(offset) {
            return Err("invalid episode UTF-8 offset".into());
        }
        let body = clip(&e.text[offset..], limit);
        let end = offset + body.len();
        if end == offset && offset < e.text.len() {
            return Err("limit cannot fit the next UTF-8 character".into());
        }
        Ok(
            json!({"activity_id":activity,"episode_id":e.id,"previous_episode":e.previous_episode,"at":e.at,"status":e.status,
            "engine":e.engine,"text":body,"offset":offset,"next_offset":(end<e.text.len()).then_some(end),
            "original_bytes":e.original_bytes,"retained_bytes":e.text.len(),"archive_truncated":e.original_bytes>e.text.len(),
            "operation_ids":e.operation_ids,"operation_count":e.operation_count,"operation_details_tool":"runtime_operation",
            "checkpoint_revision":e.checkpoint_revision,"checkpoint_snapshot":e.checkpoint_snapshot,"checkpoint_source":e.checkpoint_source,"content_is_attributed_recollection":true,"network_requests":0}),
        )
    }
    pub fn finish(&self, job: &Job, now: u64) -> Result<()> {
        let Some(wake) = &job.activity else {
            return Ok(());
        };
        if matches!(job.status, Status::Pending | Status::Running) {
            return Err("activity turn is still running".into());
        }
        self.0.update(|s: &mut State| {
            s.validate()?;
            let a = s
                .activities
                .iter_mut()
                .find(|a| {
                    a.config.id == wake.activity_id && same_scope(&a.config.context, &job.context)
                })
                .ok_or("activity not found")?;
            if a.episodes.iter().any(|e| e.id == job.id) {
                return Ok(());
            }
            if !a
                .pending
                .as_ref()
                .is_some_and(|p| p.job_id == job.id && p.cycle == wake.cycle)
            {
                return Err("activity result does not match its wake".into());
            }
            let result = job.text.as_deref().or(job.error.as_deref()).unwrap_or("");
            a.episodes.push(Episode {
                id: job.id.clone(),
                previous_episode: a.episodes.last().map(|e| e.id.clone()),
                at: job.finished_at.unwrap_or(now),
                status: job.status,
                engine: job.engine.clone(),
                text: clip(result, MAX_EPISODE_TEXT).into(),
                original_bytes: result.len(),
                operation_ids: job.progress.operation_ids.iter().take(8).cloned().collect(),
                operation_count: job.progress.operation_ids.len(),
                checkpoint_revision: a.checkpoint_revision,
                checkpoint_snapshot: a.checkpoint.clone(),
                checkpoint_source: a.checkpoint_source.clone(),
            });
            if a.episodes.len() > MAX_EPISODES {
                a.episodes.remove(0);
                a.evicted_episodes += 1;
            }
            a.pending = None;
            a.halted = !matches!(job.status, Status::Completed | Status::Canceled);
            a.next_wake_at = now.saturating_add(
                a.checkpoint
                    .rest_seconds
                    .unwrap_or(a.config.interval_seconds)
                    .max(a.config.interval_seconds),
            );
            Ok(())
        })
    }
}
fn same_scope(a: &AgentContext, b: &AgentContext) -> bool {
    a.persona == b.persona && a.conversation == b.conversation
}
fn owned<'a>(s: &'a State, ctx: &AgentContext, id: &str) -> Result<&'a Activity> {
    s.activities
        .iter()
        .find(|a| a.config.id == id && same_scope(&a.config.context, ctx))
        .ok_or("activity not available in this context".into())
}
fn card(a: &Activity) -> Value {
    json!({"config":a.config,"config_revision":a.config_revision,"checkpoint_revision":a.checkpoint_revision,
    "next_wake_at":a.next_wake_at,"cycle":a.cycle,"turns_in_current_24h_window":a.budget_used,"budget_window_start":a.budget_start,
    "pending_job":a.pending.as_ref().map(|p|&p.job_id),"halted":a.halted,"retained_episodes":a.episodes.len(),"evicted_episodes":a.evicted_episodes,"interaction_allowance":allowance(a)})
}
fn valid_operation_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn allowance(a: &Activity) -> Value {
    a.config.interaction_limits.as_ref().map_or(Value::Null,|l| {
        let likes=a.interaction_uses.iter().filter(|u|u.tool=="social_like").count() as u32;
        let comments=a.interaction_uses.iter().filter(|u|u.tool=="social_send").count() as u32;
        json!({"platform":"bilibili","window":"shared_24h","likes_remaining":l.likes.saturating_sub(likes),
            "comments_remaining":l.comments.saturating_sub(comments),"reserved_likes":likes,"reserved_comments":comments,
            "optional":true,"failed_or_uncertain_attempts_count":true})
    })
}
fn readonly_tool(name: &str) -> bool {
    // Memory updates remain local. Explicit writes require operator read_only=false.
    matches!(
        name,
        "runtime_memory"
            | "runtime_accounts"
            | "runtime_operation"
            | "runtime_agent_status"
            | "runtime_modules"
            | "social_discover"
            | "social_search"
            | "social_open"
            | "social_discussion"
            | "social_notifications"
            | "social_creator_content"
            | "social_profile"
            | "social_publications"
            | "social_emotes"
            | "social_favorites"
            | "social_chats"
            | "social_messages"
            | "social_engagement"
            | "social_subtitles"
            | "media_read"
            | "media_watch"
            | "runtime_expand"
            | "github_open"
            | "github_list"
    )
}

/// Extends the existing memory tool, without a second tool catalog or transport.
pub fn memory_call(
    state: &Path,
    ctx: &AgentContext,
    job: Option<&str>,
    params: &Value,
) -> Result<Value> {
    let object = params
        .as_object()
        .ok_or("activity memory parameters must be an object")?;
    if object.keys().any(|k| {
        ![
            "action",
            "activity_id",
            "episode_id",
            "offset",
            "limit",
            "checkpoint",
            "expected_revision",
        ]
        .contains(&k.as_str())
    }) {
        return Err(
            "unknown activity parameter; identity, source and permissions are host-owned".into(),
        );
    }
    let action = params["action"].as_str().ok_or("memory action required")?;
    let current = job.map(|id| Queue::open(state)?.get(id)).transpose()?;
    let wake = current.as_ref().and_then(|j| j.activity.as_ref());
    let activity = params
        .get("activity_id")
        .and_then(Value::as_str)
        .or_else(|| wake.map(|w| w.activity_id.as_str()))
        .ok_or("activity_id required outside an autonomous wake")?;
    let store = ActivityStore::open(state)?;
    let result = match action {
        "activity_get" => store.inline(ctx, activity)?,
        "activity_read" => store.read(
            ctx,
            activity,
            params["episode_id"].as_str().ok_or("episode_id required")?,
            params
                .get("offset")
                .map(|v| v.as_u64().ok_or("invalid offset"))
                .transpose()?
                .unwrap_or(0)
                .try_into()
                .map_err(|_| "invalid offset")?,
            params
                .get("limit")
                .map(|v| v.as_u64().ok_or("invalid limit"))
                .transpose()?
                .unwrap_or(2048)
                .try_into()
                .map_err(|_| "invalid limit")?,
        )?,
        "activity_save" => {
            let j = current
                .as_ref()
                .ok_or("activity_save requires a running autonomous turn")?;
            if j.status != Status::Running {
                return Err("activity turn is not running".into());
            }
            let wake = wake.ok_or("task turns cannot update autonomous checkpoints")?;
            if activity != wake.activity_id {
                return Err("cannot retarget the current activity".into());
            }
            let checkpoint = serde_json::from_value(params["checkpoint"].clone())
                .map_err(|_| "invalid activity checkpoint")?;
            store.save(
                ctx,
                wake,
                &j.id,
                params["expected_revision"]
                    .as_u64()
                    .ok_or("expected_revision required")?,
                checkpoint,
            )?
        }
        _ => return Err("unknown activity memory action".into()),
    };
    Ok(json!({"status":"ok","result":result,"network_requests":0}))
}
