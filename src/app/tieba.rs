// SPDX-License-Identifier: MIT
use super::*;
use crate::private_state::LocatorBatch;
use platform_tieba::{
    Client as TiebaClient, Comment, Config as TiebaConfig, Fragment, MediaKind, MediaResource,
    Post, ThreadId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const HOME_CURSOR: &str = "task-tieba-home-cursor";
const HOME_SEEN_LIMIT: usize = 512;
const FOLLOWED_FORUMS_CURSOR: &str = "task-tieba-followed-forums";
// Native forum pages can include pinned/additional posts beyond the requested
// rn=20. Preserve the SDK's full bounded page and deliver it through local tails.
const FOLLOWED_FORUM_PROVIDER_ITEMS: usize = 200;
const FOLLOWED_FORUM_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForumProgress {
    forum: platform_tieba::Forum,
    page: u32,
    has_more: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowedForumsCursor {
    schema: u32,
    source: String,
    forums: Vec<ForumProgress>,
    index: usize,
    seen: BTreeSet<u64>,
    snapshot: Option<String>,
    offset: usize,
    batches: u32,
    provider_list_may_be_capped: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowedForumSnapshot {
    forum: platform_tieba::Forum,
    forum_ref: String,
    items: Vec<Value>,
    server_page: u32,
    server_has_more: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HomeCursor {
    source: String,
    page: u32,
    seen: BTreeSet<u64>,
    snapshot: Option<String>,
    offset: usize,
    #[serde(default)]
    native_page_tag: Option<String>,
    #[serde(default)]
    request_unix: u64,
    #[serde(default)]
    server_has_more: Option<bool>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HomeSnapshot {
    items: Vec<Value>,
    server_item_count: usize,
    omitted_live_count: usize,
    #[serde(default)]
    omitted_unverified_count: usize,
}
fn preview_fragments(input: Vec<Fragment>) -> Vec<Fragment> {
    let mut characters = 640usize;
    input
        .into_iter()
        .take(12)
        .filter_map(|fragment| match fragment {
            Fragment::Text { text } => {
                let text: String = text.chars().take(characters).collect();
                characters = characters.saturating_sub(text.chars().count());
                (!text.is_empty()).then_some(Fragment::Text { text })
            }
            Fragment::Link { text, href } => {
                let text: String = text.chars().take(characters).collect();
                characters = characters.saturating_sub(text.chars().count());
                Some(Fragment::Link {
                    text,
                    href: href.chars().take(512).collect(),
                })
            }
            Fragment::Mention { text, user_id } => Some(Fragment::Mention {
                text: text.chars().take(160).collect(),
                user_id,
            }),
            Fragment::Media { resource, label } => Some(Fragment::Media {
                resource,
                label: label.chars().take(96).collect(),
            }),
            Fragment::Unsupported { kind, text, reason } => Some(Fragment::Unsupported {
                kind,
                text: text.chars().take(160).collect(),
                reason: reason.chars().take(512).collect(),
            }),
        })
        .collect()
}
// This snapshot is private: ordinary Fragment serialization drops media URLs.
// Preserve them only here so each delivered slice can create its own lazy refs.
fn cache_followed_thread(mut thread: platform_tieba::ThreadSummary) -> Result<Value> {
    let preview = preview_fragments(std::mem::take(&mut thread.fragments));
    let mut value = serde_json::to_value(thread).map_err(|_| "cannot encode followed thread")?;
    let fragments=preview.into_iter().map(|fragment|match fragment {
        Fragment::Media {resource,label}=>Ok(json!({"type":"media","resource":{"kind":resource.kind,"url":resource.url,"mime":resource.mime,"width":resource.width,"height":resource.height,"duration_seconds":resource.duration_seconds},"label":label})),
        other=>serde_json::to_value(other).map_err(|_|"cannot encode followed thread fragment".to_string()),
    }).collect::<Result<Vec<_>>>()?;
    value["fragments"] = json!(fragments);
    Ok(value)
}

pub(super) struct TiebaState {
    config: TiebaConfig,
    client: Option<TiebaClient>,
}
impl TiebaState {
    pub fn new(config: TiebaConfig) -> Self {
        Self {
            config,
            client: None,
        }
    }
}

fn fragments(
    input: &[Fragment],
    scope: &Scope,
    locators: &mut LocatorBatch<'_>,
) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    for fragment in input {
        result.push(match fragment{
            Fragment::Media{resource,label}=>{
                // Public MediaResource serialization deliberately drops the URL;
                // only this private record is allowed to persist it.
                let private=json!({"kind":resource.kind,"url":resource.url,"mime":resource.mime,"width":resource.width,"height":resource.height,"duration_seconds":resource.duration_seconds});
                let asset=locators.put(scope,"tieba-media",&private)?;
                json!({"type":"media","kind":resource.kind,"asset":asset,"label":label,"width":resource.width,"height":resource.height,"duration_seconds":resource.duration_seconds,"fetched":false})
            },
            other=>serde_json::to_value(other).map_err(|e|e.to_string())?,
        });
    }
    Ok(result)
}
fn comment(
    input: &Comment,
    scope: &Scope,
    locators: &mut LocatorBatch<'_>,
    thread: u64,
) -> Result<Value> {
    let reply = locators.put(
        scope,
        "task-tieba-reply",
        &json!({"thread":thread,"parent":input.parent_post_id,"child":input.id}),
    )?;
    Ok(
        json!({"id":input.id,"parent_post_id":input.parent_post_id,"author":input.author,"created_at":input.created_at,"floor":input.floor,"reply_to_user_id":input.reply_to_user_id,"fragments":fragments(&input.fragments,scope,locators)?,"reply_ref":reply,"reply_tool":"social_tieba","reply_action":"reply_floor","reply_supported":true}),
    )
}
fn post(
    input: &Post,
    scope: &Scope,
    locators: &mut LocatorBatch<'_>,
    thread: u64,
) -> Result<Value> {
    let reply = locators.put(
        scope,
        "task-tieba-reply",
        &json!({"thread":thread,"parent":input.id,"child":null}),
    )?;
    let comments = input
        .comments
        .iter()
        .map(|v| comment(v, scope, locators, thread))
        .collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"id":input.id,"floor":input.floor,"created_at":input.created_at,"author":input.author,"reply_count":input.reply_count,"fragments":fragments(&input.fragments,scope,locators)?,"comments":comments,"reply_ref":reply,"reply_tool":"social_tieba","reply_action":"reply_floor","reply_supported":true}),
    )
}

impl Runtime {
    fn verify_tieba_feed_viewer(&self) -> Result<()> {
        let path = super::auth::saved_session(&self.state, "tieba", &self.account)?
            .ok_or("Tieba feed requires a saved personal session")?;
        let mut session = platform_tieba::Session::load(&path).map_err(|e| e.to_string())?;
        if self.tieba.config.bduss.as_deref() != Some(session.bduss()) {
            return Err("Tieba feed client differs from the saved session".into());
        }
        platform_tieba::AuthClient::new()
            .map_err(|e| e.to_string())?
            .self_identity(&mut session)
            .map_err(|e| e.to_string())?;
        if self.tieba.config.bduss.as_deref() != Some(session.bduss()) {
            return Err(
                "Tieba feed session changed during viewer verification; refresh the connection"
                    .into(),
            );
        }
        Ok(())
    }
    pub(super) fn tieba_discover_matches(&self, params: &Value) -> bool {
        params["platform"] == "tieba"
            || params["continuation"].as_str().is_some_and(|reference| {
                [HOME_CURSOR, FOLLOWED_FORUMS_CURSOR].iter().any(|kind| {
                    self.locators
                        .scope_for(kind, reference)
                        .is_ok_and(|scope| scope.platform == Platform::Tieba)
                })
            })
    }
    pub(super) fn call_tieba_discover(&mut self, params: &Value) -> Result<Value> {
        if params.get("platform").is_some_and(|value| value != "tieba") {
            return Err("platform differs from the Tieba recommendation feed".into());
        }
        let count = number(params, "limit", 10)? as usize;
        if !(1..=20).contains(&count) {
            return Err("recommendation limit must be 1..20".into());
        }
        if let Some(reference) = params.get("continuation") {
            let reference = reference
                .as_str()
                .ok_or("continuation must be a Runtime reference")?;
            if let Ok(scope) = self.locators.scope_for(FOLLOWED_FORUMS_CURSOR, reference) {
                super::tasks::check_account(params, &scope)?;
                if scope.platform != Platform::Tieba
                    || params.get("source").is_some_and(|v| v != "following")
                {
                    return Err(
                        "source differs from the saved Tieba followed-forums continuation".into(),
                    );
                }
                let cursor: FollowedForumsCursor =
                    self.locators
                        .get(&scope, FOLLOWED_FORUMS_CURSOR, reference)?;
                return self.with_task_scope(&scope, |rt| {
                    rt.tieba_followed_forums_page(cursor, count, false)
                });
            }
            let scope = self.locators.scope_for(HOME_CURSOR, reference)?;
            if scope.platform != Platform::Tieba {
                return Err("not a Tieba recommendation continuation".into());
            }
            super::tasks::check_account(params, &scope)?;
            let cursor: HomeCursor = self.locators.get(&scope, HOME_CURSOR, reference)?;
            if cursor.source == "following" {
                return Err("This following cursor used the old followed-author meaning; refresh the followed-forums feed".into());
            }
            if !matches!(cursor.source.as_str(), "recommended" | "following")
                || params
                    .get("source")
                    .is_some_and(|source| source.as_str() != Some(cursor.source.as_str()))
            {
                return Err(
                    "source differs from the saved Tieba recommendation continuation".into(),
                );
            }
            return self.with_task_scope(&scope, |rt| rt.tieba_home_page(cursor, count));
        }
        let source = params
            .get("source")
            .map(|value| value.as_str().ok_or("discover source must be a string"))
            .transpose()?
            .unwrap_or("recommended");
        if !matches!(source, "recommended" | "following") {
            return Ok(
                json!({"status":"unsupported","platform":"tieba","available_sources":["recommended","following"],"detail":"Choose the signed-in account's personalized homepage or posts from its verified followed forums."}),
            );
        }
        self.with_platform_account("tieba", params, true, |rt| {
            if source == "following" {
                rt.verify_tieba_feed_viewer()?;
                let followed = rt
                    .tieba_client()?
                    .followed_forums()
                    .map_err(|e| e.to_string())?;
                return rt.tieba_followed_forums_page(
                    FollowedForumsCursor {
                        schema: 1,
                        source: "followed_forums".into(),
                        forums: followed
                            .forums
                            .into_iter()
                            .map(|forum| ForumProgress {
                                forum,
                                page: 1,
                                has_more: true,
                            })
                            .collect(),
                        index: 0,
                        seen: BTreeSet::new(),
                        snapshot: None,
                        offset: 0,
                        batches: 0,
                        provider_list_may_be_capped: followed.provider_list_may_be_capped,
                    },
                    count,
                    true,
                );
            }
            rt.tieba_home_page(
                HomeCursor {
                    source: source.into(),
                    page: 1,
                    seen: BTreeSet::new(),
                    snapshot: None,
                    offset: 0,
                    ..Default::default()
                },
                count,
            )
        })
    }
    fn tieba_followed_forums_page(
        &mut self,
        mut cursor: FollowedForumsCursor,
        count: usize,
        viewer_already_verified: bool,
    ) -> Result<Value> {
        if cursor.schema != 1
            || cursor.source != "followed_forums"
            || cursor.forums.len() > 200
            || cursor.index > cursor.forums.len()
            || cursor.offset > FOLLOWED_FORUM_PROVIDER_ITEMS
            || cursor.batches > 100
            || cursor.seen.len() > HOME_SEEN_LIMIT
            || !(1..=20).contains(&count)
        {
            return Err("invalid Tieba followed-forums continuation".into());
        }
        let mut forum_ids = BTreeSet::new();
        for progress in &cursor.forums {
            if progress.forum.id == 0
                || progress.forum.id > i64::MAX as u64
                || progress.forum.name.trim().is_empty()
                || progress.forum.name.len() > 256
                || progress.forum.name.chars().any(char::is_control)
                || !(1..=100).contains(&progress.page)
                || !forum_ids.insert(progress.forum.id)
            {
                return Err("invalid followed forum identity or page".into());
            }
        }
        if cursor
            .seen
            .iter()
            .any(|id| *id == 0 || *id > i64::MAX as u64)
        {
            return Err("invalid followed-forum seen thread".into());
        }
        let scope = self.scope(Platform::Tieba);
        let followed_forum_count = cursor.forums.len();
        if cursor.forums.is_empty() {
            return Ok(
                json!({"status":"ok","scope":scope,"items":[],"continuation":null,"has_more":false,"stop_reason":"no_followed_forums","context":{"source":"tieba_followed_forums","feed_kind":"followed_forums","followed_forum_count":0,"signed_in_request":true,"viewer_verified":viewer_already_verified,"cached":false,"personalization_verified":false,"ordering":"round_robin_forums_then_newest_posts","provider_list_may_be_capped":cursor.provider_list_may_be_capped},"media_downloaded":false}),
            );
        }
        let cached = cursor.snapshot.is_some();
        let fetched = if let Some(snapshot) = &cursor.snapshot {
            let (_, path) = self.store.resolve(&scope, snapshot)?;
            serde_json::from_slice::<FollowedForumSnapshot>(&read_bounded(
                &path,
                FOLLOWED_FORUM_SNAPSHOT_BYTES as u64,
            )?)
            .map_err(|_| "invalid followed-forum snapshot")?
        } else {
            if cursor.offset != 0 {
                return Err("invalid followed-forum snapshot offset".into());
            }
            let index = (0..cursor.forums.len())
                .map(|n| (cursor.index + n) % cursor.forums.len())
                .find(|&index| cursor.forums[index].has_more)
                .ok_or("followed-forum continuation has no remaining page")?;
            let progress = cursor.forums[index].clone();
            if !viewer_already_verified {
                self.verify_tieba_feed_viewer()?;
            }
            // One forum request per call. We never scan hundreds of forums to
            // fill a model page or fall back to the recommendation stream.
            let page = self
                .tieba_client()?
                .forum_threads(&progress.forum.name, progress.page, 20, 1, false)
                .map_err(|e| e.to_string())?;
            if page.forum.id != progress.forum.id
                || (page.page.current_page != 0
                    && page.page.current_page != u64::from(progress.page))
            {
                return Err("followed forum response identity/page differs".into());
            }
            if page.threads.len() > FOLLOWED_FORUM_PROVIDER_ITEMS {
                return Err("followed forum page exceeds item bound".into());
            }
            cursor.forums[index].has_more = page.page.has_more && progress.page < 100;
            if cursor.forums[index].has_more {
                cursor.forums[index].page += 1;
            }
            cursor.index = (index + 1) % cursor.forums.len();
            cursor.batches += 1;
            let forum_ref = self
                .locators
                .put_reusable(&scope, "task-tieba-forum", &page.forum)?;
            let mut items = Vec::new();
            let mut batch_seen = BTreeSet::new();
            for item in page.threads {
                if cursor.seen.contains(&item.thread_id.0) || !batch_seen.insert(item.thread_id.0) {
                    continue;
                }
                items.push(cache_followed_thread(item)?);
            }
            FollowedForumSnapshot {
                forum: page.forum,
                forum_ref,
                items,
                server_page: progress.page,
                server_has_more: page.page.has_more,
            }
        };
        if !forum_ids.contains(&fetched.forum.id)
            || fetched.items.len() > FOLLOWED_FORUM_PROVIDER_ITEMS
            || cursor.offset > fetched.items.len()
        {
            return Err("followed-forum snapshot is outside the saved follow list".into());
        }
        let forum: platform_tieba::Forum =
            self.locators
                .get(&scope, "task-tieba-forum", &fetched.forum_ref)?;
        if forum.id != fetched.forum.id {
            return Err("followed-forum snapshot reference differs".into());
        }
        let remaining = HOME_SEEN_LIMIT.saturating_sub(cursor.seen.len());
        let end = cursor
            .offset
            .saturating_add(count.min(remaining))
            .min(fetched.items.len());
        let tail = end < fetched.items.len();
        let snapshot_bytes =
            if tail && cursor.snapshot.is_none() && cursor.seen.len() < HOME_SEEN_LIMIT {
                let bytes = serde_json::to_vec(&fetched)
                    .map_err(|_| "cannot encode followed-forum page")?;
                if bytes.len() > FOLLOWED_FORUM_SNAPSHOT_BYTES {
                    return Err("followed-forum snapshot exceeds byte budget".into());
                }
                Some(bytes)
            } else {
                None
            };
        // Only materialize capabilities for the model's requested slice. In
        // particular, a 200-post provider page must not allocate 200*12 media
        // locators before those posts have ever been delivered.
        let mut assets = self.locators.batch();
        let mut items = Vec::new();
        for value in &fetched.items[cursor.offset..end] {
            let item: platform_tieba::ThreadSummary = serde_json::from_value(value.clone())
                .map_err(|_| "invalid cached followed thread")?;
            item.thread_id.validate().map_err(|e| e.to_string())?;
            if !cursor.seen.insert(item.thread_id.0) {
                continue;
            }
            let target = assets.put(
                &scope,
                "task-target",
                &super::tasks::TaskTarget::Thread {
                    thread: item.thread_id.0.to_string(),
                },
            )?;
            let content = fragments(&item.fragments, &scope, &mut assets)?;
            items.push(json!({"id":item.thread_id.0.to_string(),"thread_id":item.thread_id.0,"title":item.title,"author":item.author,"forum":fetched.forum,"forum_ref":fetched.forum_ref,"forum_open":{"tool":"social_open","args":{"reference":fetched.forum_ref}},"counts":{"replies":item.reply_count,"views":item.view_count},"created_at":item.created_at,"pinned":item.pinned,"featured":item.featured,"fragments":content,"excerpt_only":true,"target_ref":target,"open_tool":"social_open","media_downloaded":false}));
        }
        assets.commit();
        let can_continue = cursor.seen.len() < HOME_SEEN_LIMIT
            && (tail || (cursor.batches < 100 && cursor.forums.iter().any(|v| v.has_more)));
        let continuation = if can_continue {
            if tail {
                if cursor.snapshot.is_none() {
                    let bytes = snapshot_bytes.ok_or("missing followed-forum snapshot bytes")?;
                    cursor.snapshot = Some(
                        self.store
                            .import(
                                &scope,
                                "tieba-followed-forum-page",
                                "application/json",
                                &mut bytes.as_slice(),
                            )?
                            .id,
                    );
                }
                cursor.offset = end;
            } else {
                cursor.snapshot = None;
                cursor.offset = 0;
            }
            Some(self.locators.put(&scope, FOLLOWED_FORUMS_CURSOR, &cursor)?)
        } else {
            None
        };
        Ok(
            json!({"status":"ok","scope":scope,"items":items,"continuation":continuation,"has_more":can_continue,"stop_reason":if can_continue{"followed_forum_page_ready"}else if cursor.seen.len()>=HOME_SEEN_LIMIT||cursor.batches>=100{"cursor_budget"}else{"followed_forums_exhausted"},"context":{"source":"tieba_followed_forums","feed_kind":"followed_forums","signed_in_request":true,"viewer_verified":!cached,"personalization_verified":false,"followed_forum_verified":true,"membership_snapshot":true,"followed_forum_count":followed_forum_count,"provider_list_may_be_capped":cursor.provider_list_may_be_capped,"ordering":"round_robin_forums_then_newest_posts","forum":fetched.forum,"forum_ref":fetched.forum_ref,"next":{"tool":"social_open","args":{"reference":fetched.forum_ref}},"server_page":fetched.server_page,"server_has_more":fetched.server_has_more,"cached":cached,"pagination_kind":"followed_forum_round_robin_with_cached_tails","pagination_note":"Only forums from this account's own like_forum list are read. Each network batch reads one followed forum's newest-post page, then rotates to the next forum. This is not a global recommendation ranking. Use the returned social_open step to explore that forum; refresh starts a new follow-list snapshot."},"media_downloaded":false}),
        )
    }
    fn tieba_home_page(&mut self, mut cursor: HomeCursor, count: usize) -> Result<Value> {
        if cursor.source != "recommended"
            || !(1..=100).contains(&cursor.page)
            || cursor.seen.len() > HOME_SEEN_LIMIT
            || cursor.offset > 100
            || cursor
                .seen
                .iter()
                .any(|id| *id == 0 || *id > i64::MAX as u64)
        {
            return Err("invalid Tieba recommendation continuation".into());
        }
        let scope = self.scope(Platform::Tieba);
        let cached = cursor.snapshot.is_some();
        let fetched = if let Some(snapshot) = &cursor.snapshot {
            let (_, path) = self.store.resolve(&scope, snapshot)?;
            serde_json::from_slice::<HomeSnapshot>(&read_bounded(&path, 2 * 1024 * 1024)?)
                .map_err(|_| "invalid Tieba recommendation snapshot")?
        } else {
            if cursor.offset != 0 {
                return Err("invalid Tieba recommendation offset".into());
            }
            self.verify_tieba_feed_viewer()?;
            let page = self
                .tieba_client()?
                .home_recommendations(cursor.page, 20)
                .map_err(|e| e.to_string())?;
            let (entries, server_item_count, omitted_live_count, omitted_unverified_count) = (
                page.items,
                page.server_item_count,
                page.omitted_live_count,
                0,
            );
            let mut assets = self.locators.batch();
            let mut items = Vec::new();
            for item in entries {
                if cursor.seen.contains(&item.thread_id.0) {
                    continue;
                }
                let target = assets.put(
                    &scope,
                    "task-target",
                    &super::tasks::TaskTarget::Thread {
                        thread: item.thread_id.0.to_string(),
                    },
                )?;
                let preview = preview_fragments(item.fragments);
                let content = fragments(&preview, &scope, &mut assets)?;
                items.push(json!({"id":item.thread_id.0.to_string(),"thread_id":item.thread_id.0,"title":item.title,"author":item.author,"forum":item.forum,"counts":{"replies":item.reply_count,"views":item.view_count,"likes":item.like_count},"created_at":item.created_at,"provider_source":item.provider_source,"fragments":content,"excerpt_only":true,"target_ref":target,"open_tool":"social_open","media_downloaded":false}));
            }
            assets.commit();
            HomeSnapshot {
                items,
                server_item_count,
                omitted_live_count,
                omitted_unverified_count,
            }
        };
        if fetched.items.len() > 100 || cursor.offset > fetched.items.len() {
            return Err("invalid Tieba recommendation snapshot bounds".into());
        }
        let remaining = HOME_SEEN_LIMIT.saturating_sub(cursor.seen.len());
        let end = (cursor.offset + count.min(remaining)).min(fetched.items.len());
        let cached_tail_remaining = end < fetched.items.len();
        let items = fetched.items[cursor.offset..end].to_vec();
        for item in &items {
            let id = item["thread_id"]
                .as_u64()
                .filter(|id| *id > 0 && *id <= i64::MAX as u64)
                .ok_or("invalid cached Tieba thread")?;
            cursor.seen.insert(id);
        }
        let can_fetch_more = cursor.page < 100 && !items.is_empty();
        let can_continue =
            cursor.seen.len() < HOME_SEEN_LIMIT && (cached_tail_remaining || can_fetch_more);
        let continuation = if can_continue {
            if end < fetched.items.len() {
                if cursor.snapshot.is_none() {
                    let bytes = serde_json::to_vec(&fetched).map_err(|e| e.to_string())?;
                    let snapshot = self.store.import(
                        &scope,
                        "tieba-home-batch",
                        "application/json",
                        &mut bytes.as_slice(),
                    )?;
                    cursor.snapshot = Some(snapshot.id);
                }
                cursor.offset = end;
            } else {
                cursor.page += 1;
                cursor.snapshot = None;
                cursor.offset = 0;
            }
            Some(self.locators.put(&scope, HOME_CURSOR, &cursor)?)
        } else {
            None
        };
        let stop_reason = if can_continue {
            "batch_ready"
        } else if cursor.seen.len() >= HOME_SEEN_LIMIT {
            "deduplication_budget"
        } else if cursor.page >= 100 {
            "batch_budget"
        } else {
            "empty_or_repeated_batch"
        };
        let context = json!({"source":"tieba_home_personalized","feed_kind":"account_home_recommendations","signed_in_request":true,"viewer_verified":!cached,"cached":cached,"personalization_verified":false,"followed_author_verified":false,"server_has_more":cursor.server_has_more,"server_item_count":fetched.server_item_count,"omitted_live_count":fetched.omitted_live_count,"omitted_unverified_count":fetched.omitted_unverified_count,"pagination_kind":"refresh_load_more_batches","pagination_note":"The provider exposes refresh/load-more batches without a stable cursor or end indicator. Runtime preserves local batch tails and deduplicates seen threads; continuation is a bounded attempt, not proof of another server page."});
        Ok(
            json!({"status":"ok","scope":scope,"items":items,"continuation":continuation,"has_more":if !can_continue {json!(false)} else if cached_tail_remaining {json!(true)} else {Value::Null},"new_item_count":items.len(),"stop_reason":stop_reason,"context":context,"media_downloaded":false}),
        )
    }

    pub(super) fn tieba_client(&mut self) -> Result<&TiebaClient> {
        if self.tieba.client.is_none() {
            self.tieba.client =
                Some(TiebaClient::new(self.tieba.config.clone()).map_err(|e| e.to_string())?);
        }
        Ok(self.tieba.client.as_ref().unwrap())
    }
    pub(super) fn call_tieba(&mut self, method: &str, params: &Value) -> Result<Value> {
        let scope = self.scope(Platform::Tieba);
        match method {
            "tieba.thread" => {
                let id: ThreadId = required_str(params, "thread")?
                    .parse()
                    .map_err(|e: platform_tieba::Error| e.to_string())?;
                let page = u32::try_from(number(params, "page", 1)?).map_err(|_| "invalid page")?;
                let fetched = self
                    .tieba_client()?
                    .thread_page(&id, page)
                    .map_err(|e| e.to_string())?;
                let mut assets = self.locators.batch();
                let mut resources = self.store.batch();
                let raw = resources.import(
                    &scope,
                    &format!("thread:{}:{page}:source", id.0),
                    fetched.source_mime,
                    &mut fetched.raw_source.as_slice(),
                )?;
                let posts = fetched
                    .data
                    .posts
                    .iter()
                    .map(|v| post(v, &scope, &mut assets, id.0))
                    .collect::<Result<Vec<_>>>()?;
                let page_data = json!({"thread_id":id.0,"title":fetched.data.title,"forum":fetched.data.forum,"author":fetched.data.author,"reply_count":fetched.data.reply_count,"page":fetched.data.page,"posts":posts,"comments_included":fetched.data.comments_included,"notes":fetched.data.notes});
                let document = Document::new(
                    scope.clone(),
                    format!("tieba:{}:page:{page}", id.0),
                    fetched.data.title,
                    vec![
                        ContentBlock::Json {
                            value: page_data.clone(),
                        },
                        ContentBlock::Source {
                            resource: raw.clone(),
                            label: "credential-stripped protobuf source".into(),
                        },
                    ],
                );
                let result = json!({"document":document,"page":page_data,"source":raw,"source_redacted":fetched.source_redacted,"response_bytes":fetched.response_bytes});
                let manifest = resources.import(
                    &scope,
                    "tieba-thread-manifest",
                    "application/json",
                    &mut serde_json::to_vec(&result)
                        .map_err(|e| e.to_string())?
                        .as_slice(),
                )?;
                resources.commit();
                assets.commit();
                Ok(json!({"manifest":manifest,"content":result}))
            }
            "tieba.comments" | "tieba.comment_context" => {
                let id: ThreadId = required_str(params, "thread")?
                    .parse()
                    .map_err(|e: platform_tieba::Error| e.to_string())?;
                let post_id = number(params, "post", 0)?;
                let page = u32::try_from(number(params, "page", 1)?).map_err(|_| "invalid page")?;
                let fetched = if method == "tieba.comment_context" {
                    let selected = number(params, "comment", 0)?;
                    self.tieba_client()?.comment_context(
                        &id,
                        selected,
                        (post_id > 0).then_some(post_id),
                    )
                } else {
                    self.tieba_client()?.comments(&id, post_id, page)
                }
                .map_err(|e| e.to_string())?;
                let mut assets = self.locators.batch();
                let mut resources = self.store.batch();
                let raw = resources.import(
                    &scope,
                    &format!("thread:{}:post:{post_id}:comments:{page}", id.0),
                    fetched.source_mime,
                    &mut fetched.raw_source.as_slice(),
                )?;
                let comments = fetched
                    .data
                    .comments
                    .iter()
                    .map(|v| comment(v, &scope, &mut assets, id.0))
                    .collect::<Result<Vec<_>>>()?;
                let parent_post = fetched
                    .data
                    .parent_post
                    .as_ref()
                    .map(|parent| post(parent, &scope, &mut assets, id.0))
                    .transpose()?;
                let result = json!({"thread_id":id.0,"parent_post_id":fetched.data.parent_post_id,"parent_post":parent_post,
                    "selected_comment_id":fetched.data.selected_comment_id,"selected_comment_found":fetched.data.selected_comment_found,
                    "title":fetched.data.title,"forum":fetched.data.forum,"page":fetched.data.page,"comments":comments,"source":raw,"source_redacted":fetched.source_redacted});
                let manifest = resources.import(
                    &scope,
                    "tieba-comments-manifest",
                    "application/json",
                    &mut serde_json::to_vec(&result)
                        .map_err(|e| e.to_string())?
                        .as_slice(),
                )?;
                resources.commit();
                assets.commit();
                Ok(json!({"manifest":manifest,"content":result}))
            }
            "tieba.media" => {
                let asset = required_str(params, "asset")?;
                let resource: MediaResource = self.locators.get(&scope, "tieba-media", asset)?;
                let work = WorkDir::new(&self.state)?;
                let path = work.path.join("download");
                let receipt = self
                    .tieba_client()?
                    .download_to_path(&resource, &path, Budget::default().max_resource_bytes)
                    .map_err(|e| e.to_string())?;
                let group = match resource.kind {
                    MediaKind::Image | MediaKind::Emoji => "image",
                    MediaKind::Audio => "audio",
                    MediaKind::Video => "video",
                };
                let mime = super::download_mime(&path, receipt.mime.as_deref(), group)?;
                let stored = self.import_output(&scope, "tieba-media", &path, &mime)?;
                Ok(
                    json!({"asset":asset,"resource":stored,"kind":resource.kind,"fetched":true,"model_observed":false}),
                )
            }
            _ => Err("unknown or unavailable Tieba tool".into()),
        }
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    fn synthetic_followed_thread(id: u64, media: bool) -> platform_tieba::ThreadSummary {
        platform_tieba::ThreadSummary {
            thread_id: ThreadId(id),
            first_post_id: None,
            title: format!("Synthetic post {id}"),
            author: Default::default(),
            reply_count: Some(0),
            view_count: Some(0),
            created_at: Some(1),
            last_reply_at: None,
            pinned: false,
            featured: false,
            fragments: if media {
                vec![Fragment::Media {
                    resource: MediaResource {
                        kind: MediaKind::Image,
                        url: "https://tiebapic.baidu.com/synthetic-cache-image.png".into(),
                        mime: Some("image/png".into()),
                        width: Some(1),
                        height: Some(1),
                        duration_seconds: None,
                    },
                    label: "Synthetic preview".into(),
                }]
            } else {
                vec![Fragment::Text {
                    text: "Synthetic preview".into(),
                }]
            },
        }
    }
    #[test]
    fn oversized_provider_page_keeps_all_tails_and_materializes_only_delivered_media() {
        let root =
            std::env::temp_dir().join(format!("tieba-followed-large-page-{}", std::process::id()));
        private_dir(&root).unwrap();
        let mut runtime = Runtime::open(&root, "public", None).unwrap();
        let scope = Scope {
            platform: Platform::Tieba,
            account: "alice".into(),
        };
        let forum = platform_tieba::Forum {
            id: 7,
            name: "Synthetic followed forum".into(),
        };
        let forum_ref = runtime
            .locators
            .put_reusable(&scope, "task-tieba-forum", &forum)
            .unwrap();
        let rows = (1000..1000 + FOLLOWED_FORUM_PROVIDER_ITEMS as u64)
            .map(|id| cache_followed_thread(synthetic_followed_thread(id, true)).unwrap())
            .collect::<Vec<_>>();
        let snapshot = FollowedForumSnapshot {
            forum: forum.clone(),
            forum_ref,
            items: rows,
            server_page: 1,
            server_has_more: false,
        };
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        assert!(bytes.len() < FOLLOWED_FORUM_SNAPSHOT_BYTES);
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .contains("https://tiebapic.baidu.com/synthetic-cache-image.png"),
            "private snapshot retains the media URL for later slices"
        );
        let stored = runtime
            .store
            .import(
                &scope,
                "synthetic-provider-large-page",
                "application/json",
                &mut bytes.as_slice(),
            )
            .unwrap();
        let cursor = FollowedForumsCursor {
            schema: 1,
            source: "followed_forums".into(),
            forums: vec![ForumProgress {
                forum,
                page: 1,
                has_more: false,
            }],
            index: 0,
            seen: BTreeSet::new(),
            snapshot: Some(stored.id),
            offset: 0,
            batches: 1,
            provider_list_may_be_capped: false,
        };
        let reference = runtime
            .locators
            .put(&scope, FOLLOWED_FORUMS_CURSOR, &cursor)
            .unwrap();
        let locator_count = || {
            fs::read_dir(root.join("locators"))
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|value| value == "json")
                })
                .count()
        };
        let before = locator_count();
        let first = runtime
            .call_tieba_discover(&json!({"continuation":reference,"limit":1}))
            .unwrap();
        assert_eq!(first["items"].as_array().unwrap().len(), 1);
        assert_eq!(
            locator_count() - before,
            3,
            "one delivered thread, one media capability and one continuation; the other 199 previews allocate nothing"
        );
        assert!(
            !first
                .to_string()
                .contains("https://tiebapic.baidu.com/synthetic-cache-image.png")
        );
        let mut ids = vec![first["items"][0]["thread_id"].as_u64().unwrap()];
        let mut continuation = first["continuation"].as_str().map(str::to_owned);
        while let Some(reference) = continuation {
            let page = runtime
                .call_tieba_discover(&json!({"continuation":reference,"limit":17}))
                .unwrap();
            assert!(page["items"].as_array().unwrap().len() <= 17);
            assert!(
                !page
                    .to_string()
                    .contains("https://tiebapic.baidu.com/synthetic-cache-image.png")
            );
            for item in page["items"].as_array().unwrap() {
                let asset = item["fragments"][0]["asset"].as_str().unwrap();
                let media: Value = runtime.locators.get(&scope, "tieba-media", asset).unwrap();
                assert_eq!(
                    media["url"],
                    "https://tiebapic.baidu.com/synthetic-cache-image.png"
                );
                ids.push(item["thread_id"].as_u64().unwrap());
            }
            continuation = page["continuation"].as_str().map(str::to_owned);
        }
        assert_eq!(
            ids,
            (1000..1000 + FOLLOWED_FORUM_PROVIDER_ITEMS as u64).collect::<Vec<_>>()
        );
        let oversized = vec![b' '; FOLLOWED_FORUM_SNAPSHOT_BYTES + 1];
        let stored = runtime
            .store
            .import(
                &scope,
                "synthetic-over-budget-page",
                "application/json",
                &mut oversized.as_slice(),
            )
            .unwrap();
        let invalid = FollowedForumsCursor {
            snapshot: Some(stored.id),
            ..cursor
        };
        let reference = runtime
            .locators
            .put(&scope, FOLLOWED_FORUMS_CURSOR, &invalid)
            .unwrap();
        assert!(
            runtime
                .call_tieba_discover(&json!({"continuation":reference}))
                .is_err()
        );
        assert!(!root.join("outbox").exists());
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn followed_forum_pages_are_membership_bound_cached_and_openable_without_network() {
        let root = std::env::temp_dir().join(format!(
            "tieba-followed-forum-contract-{}",
            std::process::id()
        ));
        private_dir(&root).unwrap();
        let mut runtime = Runtime::open(&root, "public", None).unwrap();
        let scope = Scope {
            platform: Platform::Tieba,
            account: "alice".into(),
        };
        let forum = platform_tieba::Forum {
            id: 7,
            name: "Synthetic followed forum".into(),
        };
        let forum_ref = runtime
            .locators
            .put_reusable(&scope, "task-tieba-forum", &forum)
            .unwrap();
        let items = [42u64, 43, 44]
            .into_iter()
            .map(|id| cache_followed_thread(synthetic_followed_thread(id, false)).unwrap())
            .collect();
        let snapshot = FollowedForumSnapshot {
            forum: forum.clone(),
            forum_ref: forum_ref.clone(),
            items,
            server_page: 1,
            server_has_more: false,
        };
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let stored = runtime
            .store
            .import(
                &scope,
                "synthetic-followed-forum-page",
                "application/json",
                &mut bytes.as_slice(),
            )
            .unwrap();
        let cursor = FollowedForumsCursor {
            schema: 1,
            source: "followed_forums".into(),
            forums: vec![ForumProgress {
                forum: forum.clone(),
                page: 1,
                has_more: false,
            }],
            index: 0,
            seen: BTreeSet::new(),
            snapshot: Some(stored.id.clone()),
            offset: 0,
            batches: 1,
            provider_list_may_be_capped: false,
        };
        let reference = runtime
            .locators
            .put(&scope, FOLLOWED_FORUMS_CURSOR, &cursor)
            .unwrap();
        assert!(runtime.tieba_discover_matches(&json!({"continuation":reference})));
        for params in [
            json!({"continuation":reference,"source":"recommended"}),
            json!({"continuation":reference,"account":"bob"}),
            json!({"continuation":reference,"platform":"bilibili"}),
        ] {
            assert!(runtime.call_tieba_discover(&params).is_err());
        }
        let first = runtime
            .call_tieba_discover(&json!({"continuation":reference,"limit":1}))
            .unwrap();
        assert_eq!(first["items"][0]["thread_id"], 42);
        assert_eq!(
            first["has_more"], true,
            "cached tail survives the forum provider end"
        );
        assert_eq!(first["context"]["source"], "tieba_followed_forums");
        assert_eq!(first["context"]["feed_kind"], "followed_forums");
        assert_eq!(first["context"]["viewer_verified"], false);
        assert_eq!(first["context"]["cached"], true);
        assert_eq!(first["context"]["next"]["tool"], "social_open");
        assert_eq!(first["context"]["next"]["args"]["reference"], forum_ref);
        let next: FollowedForumsCursor = runtime
            .locators
            .get(
                &scope,
                FOLLOWED_FORUMS_CURSOR,
                first["continuation"].as_str().unwrap(),
            )
            .unwrap();
        assert_eq!(next.index, 0);
        assert_eq!(next.forums[0].forum.id, 7);
        let last = runtime
            .call_tieba_discover(&json!({"continuation":first["continuation"],"limit":20}))
            .unwrap();
        assert_eq!(
            last["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["thread_id"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![43, 44]
        );
        assert_eq!(last["has_more"], false);
        assert!(last["continuation"].is_null());
        let wrong = FollowedForumsCursor {
            forums: vec![ForumProgress {
                forum: platform_tieba::Forum {
                    id: 99,
                    name: "Not followed".into(),
                },
                page: 1,
                has_more: false,
            }],
            ..cursor
        };
        let wrong = runtime
            .locators
            .put(&scope, FOLLOWED_FORUMS_CURSOR, &wrong)
            .unwrap();
        assert!(
            runtime
                .call_tieba_discover(&json!({"continuation":wrong}))
                .unwrap_err()
                .contains("outside the saved follow list")
        );
        assert_eq!(runtime.account, "public");
        assert!(!root.join("outbox").exists());
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn home_continuation_preserves_cached_tail_and_rejects_retargeting_without_network() {
        let root = std::env::temp_dir().join(format!(
            "tieba-home-continuation-{}-{}",
            std::process::id(),
            crate::outbox::now().unwrap()
        ));
        private_dir(&root).unwrap();
        let mut runtime = Runtime::open(&root, "public", None).unwrap();
        let scope = Scope {
            platform: Platform::Tieba,
            account: "alice".into(),
        };
        let mut items = Vec::new();
        for thread in [42, 43, 44] {
            let target = runtime
                .locators
                .put(
                    &scope,
                    "task-target",
                    &super::super::tasks::TaskTarget::Thread {
                        thread: thread.to_string(),
                    },
                )
                .unwrap();
            items.push(json!({"id":thread.to_string(),"thread_id":thread,"target_ref":target}));
        }
        let data = HomeSnapshot {
            items,
            server_item_count: 3,
            omitted_live_count: 0,
            omitted_unverified_count: 0,
        };
        let bytes = serde_json::to_vec(&data).unwrap();
        let snapshot = runtime
            .store
            .import(
                &scope,
                "synthetic-home-batch",
                "application/json",
                &mut bytes.as_slice(),
            )
            .unwrap();
        let reference = runtime
            .locators
            .put(
                &scope,
                HOME_CURSOR,
                &HomeCursor {
                    source: "recommended".into(),
                    page: 1,
                    seen: BTreeSet::new(),
                    snapshot: Some(snapshot.id),
                    offset: 0,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(runtime.tieba_discover_matches(&json!({"continuation":reference})));
        assert!(
            runtime
                .call_tieba_discover(&json!({"continuation":reference,"source":"following"}))
                .unwrap_err()
                .contains("source differs")
        );
        assert!(
            runtime
                .call_tieba_discover(&json!({"continuation":reference,"account":"bob"}))
                .unwrap_err()
                .contains("account differs")
        );
        assert!(
            runtime
                .call_tieba_discover(&json!({"continuation":reference,"platform":"bilibili"}))
                .is_err()
        );
        let first = runtime
            .call_tieba_discover(&json!({"continuation":reference,"limit":1}))
            .unwrap();
        assert_eq!(first["scope"]["account"], "alice");
        assert_eq!(first["items"][0]["thread_id"], 42);
        assert_eq!(first["has_more"], true);
        assert_eq!(first["context"]["server_has_more"], Value::Null);
        assert_eq!(first["context"]["source"], "tieba_home_personalized");
        assert_eq!(first["context"]["viewer_verified"], false);
        assert_eq!(first["context"]["cached"], true);
        assert_eq!(
            runtime
                .load_task_target(first["items"][0]["target_ref"].as_str().unwrap())
                .unwrap()
                .0,
            scope
        );
        let second = runtime
            .call_tieba_discover(&json!({"continuation":first["continuation"],"limit":1}))
            .unwrap();
        assert_eq!(second["items"][0]["thread_id"], 43);
        let old_following = runtime
            .locators
            .put(
                &scope,
                HOME_CURSOR,
                &HomeCursor {
                    source: "following".into(),
                    page: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            runtime
                .call_tieba_discover(&json!({"continuation":old_following}))
                .unwrap_err()
                .contains("old followed-author meaning")
        );
        assert_eq!(
            runtime.account, "public",
            "continuation must restore the caller account"
        );
        assert_eq!(
            runtime
                .call_tieba_discover(&json!({"source":"popular"}))
                .unwrap()["status"],
            "unsupported"
        );
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn native_child_projection_returns_exact_account_bound_reply_tool_input() {
        let root = std::env::temp_dir().join(format!(
            "tieba-child-projection-{}-{}",
            std::process::id(),
            crate::outbox::now().unwrap()
        ));
        let store = crate::private_state::PrivateLocators::open(&root).unwrap();
        let scope = Scope {
            platform: Platform::Tieba,
            account: "owner".into(),
        };
        let input = Comment {
            id: 789,
            parent_post_id: 456,
            author: platform_tieba::Author::default(),
            created_at: 100,
            floor: 2,
            reply_to_user_id: Some(77),
            fragments: vec![Fragment::Text {
                text: "body".into(),
            }],
        };
        let mut batch = store.batch();
        let projected = comment(&input, &scope, &mut batch, 123).unwrap();
        batch.commit();
        let reference = projected["reply_ref"].as_str().unwrap();
        let saved: Value = store.get(&scope, "task-tieba-reply", reference).unwrap();
        assert_eq!(saved, json!({"thread":123,"parent":456,"child":789}));
        assert_eq!(projected["reply_tool"], "social_tieba");
        assert_eq!(projected["reply_action"], "reply_floor");
        assert_eq!(projected["reply_supported"], true);
        assert!(
            store
                .get::<Value>(
                    &Scope {
                        platform: Platform::Tieba,
                        account: "other".into()
                    },
                    "task-tieba-reply",
                    reference
                )
                .is_err()
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
