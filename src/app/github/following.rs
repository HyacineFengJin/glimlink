// SPDX-License-Identifier: MIT
//! Scoped following activity, not algorithmic homepage recommendations.
use super::*;

const FOLLOWING_CURSOR: &str = "task-github-following-cursor";
const FOLLOWING_MAX_EVENTS: usize = 300;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Viewer {
    id: u64,
    login: String,
}
impl Viewer {
    fn matches(&self, session: &Session) -> bool {
        self.id == session.id && self.login.eq_ignore_ascii_case(&session.login)
    }
    fn validate(&self) -> Result<()> {
        if self.id == 0 || self.id > i64::MAX as u64 || !valid_login(&self.login) {
            return Err("invalid GitHub following viewer binding".into());
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowingCursor {
    source: String,
    page: u32,
    seen: BTreeSet<String>,
    viewer: Option<Viewer>,
    pending: Option<FollowingPending>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowingPending {
    resource: String,
    offset: usize,
    next_page: Option<u32>,
    context: Value,
}
impl FollowingCursor {
    fn validate(&self) -> Result<()> {
        if self.source != "following"
            || !(1..=15).contains(&self.page)
            || self.seen.len() > FOLLOWING_MAX_EVENTS
            || self
                .seen
                .iter()
                .any(|id| id.is_empty() || id.len() > 32 || !id.bytes().all(|b| b.is_ascii_digit()))
            || self.viewer.is_none()
                && (self.page != 1 || !self.seen.is_empty() || self.pending.is_some())
        {
            return Err("invalid GitHub following continuation".into());
        }
        if let Some(viewer) = &self.viewer {
            viewer.validate()?;
        }
        if let Some(pending) = &self.pending {
            if pending.offset > PAGE_SIZE as usize
                || !pending.context.is_object()
                || pending.context["source"] != "github_received_activity"
                || pending
                    .next_page
                    .is_some_and(|page| page != self.page + 1 || page > 15)
            {
                return Err("invalid cached GitHub following page".into());
            }
        }
        Ok(())
    }
}
fn subject_target(item: &Value) -> Result<Target> {
    let name = item["repo"]["name"]
        .as_str()
        .ok_or("GitHub activity repository missing")?;
    let (owner, repo) = name
        .split_once('/')
        .ok_or("GitHub activity repository malformed")?;
    validate_repository(owner, repo)?;
    let owner = owner.to_owned();
    let repo = repo.to_owned();
    let subject = &item["subject"];
    let number = subject["number"]
        .as_u64()
        .filter(|id| *id > 0 && *id <= i64::MAX as u64);
    let comment = subject["comment"]
        .as_u64()
        .filter(|id| *id > 0 && *id <= i64::MAX as u64);
    let target = match (subject["kind"].as_str(), number, comment) {
        (Some("issue"), Some(number), _) => Target::Issue {
            owner,
            repo,
            number,
            pull: subject["pull"] == true,
        },
        (Some("comment"), Some(number), Some(comment)) => Target::Comment {
            owner,
            repo,
            number,
            pull: subject["pull"] == true,
            comment,
        },
        (Some("review_comment"), Some(number), Some(comment)) => Target::ReviewComment {
            owner,
            repo,
            number,
            comment,
        },
        _ => Target::Repository { owner, repo },
    };
    target.validate()?;
    Ok(target)
}
impl Runtime {
    pub(in crate::app) fn github_following_matches(&self, method: &str, params: &Value) -> bool {
        method == "social.discover"
            && (params["platform"] == "github"
                || params["continuation"].as_str().is_some_and(|reference| {
                    self.locators
                        .scope_for(FOLLOWING_CURSOR, reference)
                        .is_ok_and(|scope| scope.platform == Platform::Github)
                }))
    }
    pub(in crate::app) fn call_github_following(&mut self, params: &Value) -> Result<Value> {
        let result = self.github_following_entry(params);
        match result {
            Err(error) => {
                if let Some(mut value) = decode_task_error(&error) {
                    if let Some(reference) = params.get("continuation").filter(|v| v.is_string()) {
                        value["continuation"] = reference.clone();
                    }
                    Ok(value)
                } else {
                    Err(error)
                }
            }
            value => value,
        }
    }
    fn github_following_entry(&mut self, params: &Value) -> Result<Value> {
        allowed(
            params,
            &[
                "platform",
                "source",
                "account",
                "limit",
                "continuation",
                "feed_ref",
            ],
        )?;
        if params
            .get("platform")
            .is_some_and(|value| value != "github")
        {
            return Err("platform differs from the GitHub following feed".into());
        }
        let count = limit(params)?;
        if let Some(reference) = params.get("continuation") {
            let reference = reference
                .as_str()
                .ok_or("continuation must be a Runtime reference")?;
            let scope = self.locators.scope_for(FOLLOWING_CURSOR, reference)?;
            if scope.platform != Platform::Github {
                return Err("not a GitHub following continuation".into());
            }
            crate::app::tasks::check_account(params, &scope)?;
            let cursor: FollowingCursor = self.locators.get(&scope, FOLLOWING_CURSOR, reference)?;
            if params
                .get("source")
                .is_some_and(|value| value.as_str() != Some(cursor.source.as_str()))
            {
                return Err("source differs from the saved GitHub following continuation".into());
            }
            cursor.validate()?;
            return self.with_task_scope(&scope, |rt| rt.github_following_page(cursor, count));
        }
        let source = params
            .get("source")
            .map(|value| value.as_str().ok_or("discover source must be a string"))
            .transpose()?
            .unwrap_or("recommended");
        if source != "following" {
            return Ok(
                json!({"status":"unsupported","platform":"github","available_sources":["following"],"detail":"GitHub offers account-received following activity here, separate from algorithmic homepage recommendations. No public-event or repository-search fallback was made."}),
            );
        }
        self.with_platform_account("github", params, true, |rt| {
            rt.github_following_page(
                FollowingCursor {
                    source: "following".into(),
                    page: 1,
                    seen: BTreeSet::new(),
                    viewer: None,
                    pending: None,
                },
                count,
            )
        })
    }
    fn github_following_cached_viewer(&self, viewer: &Viewer) -> Result<()> {
        let path = crate::app::auth::saved_session(&self.state, "github", &self.account)?
            .ok_or_else(|| self.github_sdk_error(platform_github::Error::LoginRequired))?;
        let session: Session = serde_json::from_slice(&github_private(&path, 16384)?)
            .map_err(|_| "invalid private GitHub session")?;
        session.validate()?;
        if !viewer.matches(&session) {
            return Err("GitHub following continuation belongs to a different saved viewer; start a new feed".into());
        }
        crate::app::auth::bind_identity(
            path.parent().ok_or("GitHub account directory missing")?,
            &session.id.to_string(),
        )?;
        Ok(())
    }
    fn github_following_page(
        &mut self,
        mut cursor: FollowingCursor,
        count: usize,
    ) -> Result<Value> {
        cursor.validate()?;
        let scope = self.scope(Platform::Github);
        let (all, offset, next_page, mut context, cached) = if let Some(pending) =
            cursor.pending.take()
        {
            self.github_following_cached_viewer(
                cursor
                    .viewer
                    .as_ref()
                    .ok_or("GitHub following viewer missing")?,
            )?;
            let (resource, path) = self.store.resolve(&scope, &pending.resource)?;
            if resource.mime != "application/json" || resource.bytes > 256 * 1024 {
                return Err("GitHub following cache exceeds its bounded JSON budget".into());
            }
            let bytes = read_bounded(&path, 256 * 1024)?;
            if bytes.len() as u64 != resource.bytes {
                return Err("GitHub following cached page length changed".into());
            }
            let rows: Vec<Value> = serde_json::from_slice(&bytes)
                .map_err(|_| "invalid cached GitHub following page")?;
            if rows.len() > PAGE_SIZE as usize || pending.offset > rows.len() {
                return Err("invalid GitHub following cache offset".into());
            }
            (
                rows,
                pending.offset,
                pending.next_page,
                pending.context,
                Some(pending.resource),
            )
        } else {
            // This path retains the existing account-local rate-limit backoff
            // and OAuth refresh rules. Cached tails above remain readable.
            let (client, session) = self.github_client(true)?;
            let session = session
                .ok_or_else(|| self.github_sdk_error(platform_github::Error::LoginRequired))?;
            if cursor
                .viewer
                .as_ref()
                .is_some_and(|viewer| !viewer.matches(&session))
            {
                return Err("GitHub following continuation belongs to a different saved viewer; start a new feed".into());
            }
            let actual = client
                .current_user()
                .map_err(|e| self.github_sdk_error(e))?;
            if actual["id"].as_u64() != Some(session.id)
                || actual["login"]
                    .as_str()
                    .is_none_or(|login| !login.eq_ignore_ascii_case(&session.login))
            {
                return Err(task_error(
                    json!({"status":"needs_auth","scope":scope,"network_retry_started":false,"detail":"The saved GitHub token identifies a different viewer. No received-event request or public fallback was made."}),
                ));
            }
            cursor.viewer = Some(Viewer {
                id: session.id,
                login: session.login.clone(),
            });
            let page = client
                .received_events(&session.login, cursor.page)
                .map_err(|e| self.github_sdk_error(e))?;
            let next = match page.next_page {
                Some(next)
                    if page.has_more == Some(true) && next == cursor.page + 1 && next <= 15 =>
                {
                    Some(next)
                }
                Some(_) => return Err("GitHub following pagination did not advance safely".into()),
                None => None,
            };
            let context = json!({"source":"github_received_activity","feed_kind":"following_activity","algorithmic_recommendations":false,"signed_in_request":true,"viewer_verified":true,"server_has_more":page.has_more,"pagination_warning":page.pagination_warning,"poll_interval_seconds":page.poll_interval_seconds,"window_max_events":300,"window_days":30,"real_time":false,"history_complete":false,"marked_read":false,"pagination_consistency":"provider pages; concurrent activity may shift page boundaries"});
            (page.items, 0, next, context, None)
        };
        let mut end = offset;
        let cached_page = cached.is_some();
        let mut items = Vec::new();
        let mut duplicates = 0usize;
        while end < all.len() && items.len() < count && cursor.seen.len() < FOLLOWING_MAX_EVENTS {
            let item = &all[end];
            end += 1;
            let id = item["id"]
                .as_str()
                .filter(|id| {
                    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit())
                })
                .ok_or("invalid cached GitHub event id")?;
            if !cursor.seen.insert(id.to_owned()) {
                duplicates += 1;
                continue;
            }
            let target = subject_target(item)?;
            let (owner, repo) = target.repository();
            let repo_ref = self.save_github_target(&Target::Repository {
                owner: owner.into(),
                repo: repo.into(),
            })?;
            let reference = self.save_github_target(&target)?;
            let mut projected = selected(item, &["id", "type", "created_at", "public", "action"]);
            projected["actor"] = selected(&item["actor"], &["id", "login"]);
            projected["repo"] = selected(&item["repo"], &["id", "name"]);
            projected["subject"] = if item["subject"].is_object() {
                selected(
                    &item["subject"],
                    &["kind", "number", "pull", "comment", "title"],
                )
            } else {
                Value::Null
            };
            projected["target_ref"] = json!(reference);
            projected["repository_ref"] = json!(repo_ref);
            projected["open_tool"] = json!("github_open");
            projected["detail_observed"] = json!(false);
            projected["target_kind"] = json!(match target {
                Target::Repository { .. } => "repository",
                Target::Issue { pull: true, .. } => "pull_request",
                Target::Issue { .. } => "issue",
                Target::Comment { .. } => "comment",
                Target::ReviewComment { .. } => "review_comment",
                Target::File { .. } => "file",
            });
            items.push(projected);
        }
        let local_tail = end < all.len();
        let budget = cursor.seen.len() >= FOLLOWING_MAX_EVENTS;
        let continuation = if !budget && local_tail {
            let resource = if let Some(resource) = cached {
                resource
            } else {
                let bytes = serde_json::to_vec(&all).map_err(|e| e.to_string())?;
                if bytes.len() > 256 * 1024 {
                    return Err("GitHub following page exceeds its cache budget".into());
                }
                self.store
                    .import(
                        &scope,
                        "github-following-page",
                        "application/json",
                        &mut bytes.as_slice(),
                    )?
                    .id
            };
            cursor.pending = Some(FollowingPending {
                resource,
                offset: end,
                next_page,
                context: context.clone(),
            });
            Some(self.locators.put(&scope, FOLLOWING_CURSOR, &cursor)?)
        } else if !budget && let Some(page) = next_page {
            cursor.page = page;
            Some(self.locators.put(&scope, FOLLOWING_CURSOR, &cursor)?)
        } else {
            None
        };
        context["duplicates_removed"] = json!(duplicates);
        context["cached_page"] = json!(cached_page);
        context["retained_event_ids"] = json!(cursor.seen.len());
        let server_more = context["server_has_more"].as_bool();
        let has_more = if continuation.is_some() {
            json!(true)
        } else if budget || server_more == Some(false) {
            json!(false)
        } else {
            Value::Null
        };
        Ok(
            json!({"status":"ok","scope":scope,"items":items,"continuation":continuation,"has_more":has_more,"stop_reason":if budget {"event_window_budget"}else if local_tail {"cached_tail"}else if next_page.is_some(){"page_ready"}else if server_more==Some(false){"provider_end"}else{"pagination_unavailable"},"context":context,"media_downloaded":false,"files_downloaded":false}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activity_subjects_become_exact_existing_github_targets() {
        let item = json!({"repo":{"name":"owner/repository"},"subject":{"kind":"comment","number":42,"pull":true,"comment":99}});
        assert_eq!(
            subject_target(&item).unwrap(),
            Target::Comment {
                owner: "owner".into(),
                repo: "repository".into(),
                number: 42,
                pull: true,
                comment: 99
            }
        );
        assert!(subject_target(&json!({"repo":{"name":"owner/repository/escape"}})).is_err());
    }
    #[test]
    fn cached_activity_preserves_tail_and_binds_source_account_and_viewer_without_network() {
        let root = std::env::temp_dir().join(format!(
            "github-following-cache-{}-{}",
            std::process::id(),
            crate::outbox::now().unwrap()
        ));
        private_dir(&root).unwrap();
        let mut rt = Runtime::open(&root, "public", None).unwrap();
        let scope = Scope {
            platform: Platform::Github,
            account: "alice".into(),
        };
        let dir = account_directory(&root, "github", "alice").unwrap();
        save_private_new(
            &dir.join("session.json"),
            &json!({"schema":1,"id":7,"login":"SyntheticViewer","token":"synthetic-private-token"}),
        )
        .unwrap();
        let rows = vec![
            json!({"id":"1","type":"WatchEvent","repo":{"id":8,"name":"owner/repository"},"subject":null}),
            json!({"id":"2","type":"IssuesEvent","repo":{"id":8,"name":"owner/repository"},"subject":{"kind":"issue","number":42,"pull":false}}),
        ];
        let bytes = serde_json::to_vec(&rows).unwrap();
        let resource = rt
            .store
            .import(
                &scope,
                "synthetic-following",
                "application/json",
                &mut bytes.as_slice(),
            )
            .unwrap();
        let cursor = FollowingCursor {
            source: "following".into(),
            page: 1,
            seen: BTreeSet::new(),
            viewer: Some(Viewer {
                id: 7,
                login: "SyntheticViewer".into(),
            }),
            pending: Some(FollowingPending {
                resource: resource.id,
                offset: 0,
                next_page: None,
                context: json!({"source":"github_received_activity","server_has_more":false}),
            }),
        };
        let reference = rt.locators.put(&scope, FOLLOWING_CURSOR, &cursor).unwrap();
        assert!(rt.github_following_matches("social.discover", &json!({"continuation":reference})));
        assert!(
            rt.call_github_following(&json!({"continuation":reference,"source":"recommended"}))
                .is_err()
        );
        assert!(
            rt.call_github_following(&json!({"continuation":reference,"account":"bob"}))
                .is_err()
        );
        let first = rt
            .call_github_following(&json!({"continuation":reference,"limit":1}))
            .unwrap();
        assert_eq!(first["scope"]["account"], "alice");
        assert_eq!(first["items"][0]["id"], "1");
        assert!(first["items"][0]["target_ref"].is_string());
        assert!(!first.to_string().contains("synthetic-private-token"));
        let next = first["continuation"].clone();
        let last = rt
            .call_github_following(&json!({"continuation":next}))
            .unwrap();
        assert_eq!(last["items"][0]["id"], "2");
        assert_eq!(last["continuation"], Value::Null);
        assert_eq!(last["has_more"], false);
        // A later local account replacement must not reinterpret a cached page.
        let mut wrong = cursor.clone();
        wrong.viewer.as_mut().unwrap().id = 99;
        let other = rt.locators.put(&scope, FOLLOWING_CURSOR, &wrong).unwrap();
        assert!(
            rt.call_github_following(&json!({"continuation":other}))
                .unwrap_err()
                .contains("viewer")
        );
        assert_eq!(
            rt.call_github_following(&json!({"platform":"github"}))
                .unwrap()["status"],
            "unsupported"
        );
        assert_eq!(rt.account, "public");
        drop(rt);
        fs::remove_dir_all(root).unwrap();
    }
}
