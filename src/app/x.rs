// SPDX-License-Identifier: MIT
//! X task orchestration and private session ownership. The protocol adapter's
//! separately attributed implementations live in platform-x.
use super::*;
use crate::private_state::account_directory;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const TARGET: &str = "task-x-target";
const CURSOR: &str = "task-x-continuation";
const HOME_CURSOR: &str = "task-x-home-continuation";
const MEDIA: &str = "task-x-media";
const FAILURE: &str = "x-request:";

pub(super) struct XState {
    account: String,
    identity: String,
    client: platform_x::Client,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    schema: u32,
    identity: String,
    user: Value,
    session: platform_x::Session,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Target {
    User {
        id: String,
        screen_name: String,
    },
    Post {
        id: String,
        author_id: String,
        screen_name: String,
    },
}
impl Target {
    /// Returned opaque targets already bind a verified stable creator identity.
    /// Expanding that creator need not re-fetch the profile or source post.
    fn creator(&self) -> Result<Option<(&str, &str)>> {
        let (creator_id, name) = match self {
            Self::User { id, screen_name } => (id, screen_name),
            Self::Post {
                author_id,
                screen_name,
                ..
            } => (author_id, screen_name),
        };
        if creator_id.is_empty() {
            return Ok(None);
        }
        id(creator_id)?;
        handle(name)?;
        Ok(Some((creator_id, name)))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    user_id: String,
    screen_name: String,
    cursor: String,
    seen: BTreeSet<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HomeCursor {
    source: String,
    started: bool,
    provider_cursor: Option<String>,
    provider_has_more: Option<bool>,
    pending: Vec<Value>,
    seen: BTreeSet<String>,
    warnings: Vec<String>,
}
impl HomeCursor {
    fn validate(&self) -> Result<()> {
        if !matches!(self.source.as_str(), "recommended" | "following")
            || self.provider_cursor.as_ref().is_some_and(|v| {
                v.is_empty() || v.len() > 4096 || v.bytes().any(|b| b.is_ascii_control())
            })
            || self.pending.len() > 100
            || self.seen.len() > 512
            || self.warnings.len() > 16
            || self.seen.iter().any(|v| id(v).is_err())
        {
            return Err("invalid X home continuation".into());
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Media {
    url: String,
    kind: String,
}
fn error(error: platform_x::Error) -> String {
    format!("{FAILURE}{}", error.code())
}
fn id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 20
        || value.starts_with('0')
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("invalid X object identity".into());
    }
    Ok(())
}
fn handle(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 15
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("invalid X screen name".into());
    }
    Ok(())
}
fn user_identity(value: &Value) -> Result<&str> {
    let value = value["id"]
        .as_str()
        .ok_or("X response has no stable identity")?;
    id(value)?;
    Ok(value)
}
fn fields(params: &Value, allowed: &[&str]) -> Result<()> {
    if params
        .as_object()
        .is_none_or(|v| v.keys().any(|k| !allowed.contains(&k.as_str())))
    {
        return Err("unsupported X task parameter".into());
    }
    Ok(())
}
fn private(path: &Path) -> Result<Vec<u8>> {
    let file = open_regular(path, 256 * 1024)?;
    if file
        .metadata()
        .map_err(|_| "cannot inspect X private input")?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err("X private input must have mode0600".into());
    }
    let mut bytes = Vec::new();
    file.take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read X private input")?;
    if bytes.len() > 256 * 1024 {
        return Err("X private input exceeds limit".into());
    }
    Ok(bytes)
}
fn saved(path: &Path) -> Result<Saved> {
    let saved: Saved =
        serde_json::from_slice(&private(path)?).map_err(|_| "invalid imported session for X")?;
    if saved.schema != 1 || user_identity(&saved.user)? != saved.identity {
        return Err("X saved session identity mismatch".into());
    }
    Ok(saved)
}
fn parsed(reference: &str) -> Result<Target> {
    if reference.len() > 2048 || reference.chars().any(char::is_control) {
        return Err("invalid X reference".into());
    }
    if let Some(name) = reference.strip_prefix('@') {
        handle(name)?;
        return Ok(Target::User {
            id: String::new(),
            screen_name: name.into(),
        });
    }
    let raw = [
        "https://x.com/",
        "https://www.x.com/",
        "https://twitter.com/",
        "https://www.twitter.com/",
    ]
    .iter()
    .find_map(|prefix| reference.strip_prefix(prefix));
    if reference.contains("://") && raw.is_none() {
        return Err("use a canonical X profile or post URL".into());
    }
    let path = raw
        .unwrap_or(reference.strip_prefix('@').unwrap_or(reference))
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        [name] if !name.bytes().all(|b| b.is_ascii_digit()) => {
            handle(name)?;
            Ok(Target::User {
                id: String::new(),
                screen_name: (*name).into(),
            })
        }
        [post] => {
            id(post)?;
            Ok(Target::Post {
                id: (*post).into(),
                author_id: String::new(),
                screen_name: String::new(),
            })
        }
        ["i", "status", post] => {
            id(post)?;
            Ok(Target::Post {
                id: (*post).into(),
                author_id: String::new(),
                screen_name: String::new(),
            })
        }
        [name, "status", post] => {
            handle(name)?;
            id(post)?;
            Ok(Target::Post {
                id: (*post).into(),
                author_id: String::new(),
                screen_name: (*name).into(),
            })
        }
        _ => Err("unsupported X profile or post reference".into()),
    }
}
impl Runtime {
    fn x_saved(&self) -> Result<Saved> {
        let path = super::auth::saved_session(&self.state, "x", &self.account)?
            .ok_or_else(|| format!("{FAILURE}needs_auth"))?;
        let value = saved(&path)?;
        super::auth::bind_identity(
            path.parent().ok_or("missing X account directory")?,
            &value.identity,
        )?;
        Ok(value)
    }
    fn x_client(&mut self) -> Result<&mut XState> {
        if self.x.as_ref().is_none_or(|x| x.account != self.account) {
            let saved = self.x_saved()?;
            self.x = Some(XState {
                account: self.account.clone(),
                identity: saved.identity,
                client: platform_x::Client::new(platform_x::Config::default(), saved.session)
                    .map_err(error)?,
            });
        }
        Ok(self.x.as_mut().expect("initialized X state"))
    }
    pub(crate) fn x_operator_reuse(&mut self) -> Result<Value> {
        let state = self.x_client()?;
        match state.client.current_user() {
            Ok(user) => {
                let user = serde_json::to_value(user).map_err(|_| "cannot encode X identity")?;
                if user_identity(&user)? != state.identity {
                    return Err("X session identity mismatch".into());
                }
                Ok(json!({"status":"online","reused_session":true,"user":user}))
            }
            Err(platform_x::Error::LoginRequired) => {
                self.x = None;
                Ok(json!({"status":"needs_auth"}))
            }
            Err(e) => Err(error(e)),
        }
    }
    /// Human management only; no model-facing tool accepts Cookie material.
    pub fn x_import_login(&mut self, input: &Path) -> Result<Value> {
        if self.account == "public" {
            return Err("X login needs an explicit account alias".into());
        }
        let bytes = private(input)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| "invalid imported session for X")?;
        let session = platform_x::Session::from_private_input(text)
            .map_err(|_| "invalid imported session for X")?;
        let directory = account_directory(&self.state, "x", &self.account)?;
        // A bounded unverified candidate supports explicit retry after protocol
        // fixes without asking the human to re-enter credentials. It is never
        // counted as a logged-in account or read by model-facing tools.
        let candidate = directory.join("login-candidate.json");
        let candidate_temp = directory.join(format!(".candidate-{}.tmp", std::process::id()));
        let _ = fs::remove_file(&candidate_temp);
        save_private_new(
            &candidate_temp,
            &serde_json::to_value(&session).map_err(|_| "cannot encode X private candidate")?,
        )?;
        fs::rename(&candidate_temp, &candidate).map_err(|_| "cannot save X private candidate")?;
        let mut client = platform_x::Client::new(platform_x::Config::default(), session.clone())
            .map_err(error)?;
        let user = serde_json::to_value(match client.current_user() {
            Ok(user) => user,
            Err(e) => {
                if matches!(e, platform_x::Error::LoginRequired) {
                    let _ = fs::remove_file(&candidate);
                }
                return Err(error(e));
            }
        })
        .map_err(|_| "cannot encode X identity")?;
        let identity = user_identity(&user)?.to_owned();
        let destination = directory.join("session.json");
        if destination.exists() && saved(&destination)?.identity != identity {
            return Err("this alias is bound to another account".into());
        }
        super::auth::bind_identity(&directory, &identity)?;
        let record = Saved {
            schema: 1,
            identity: identity.clone(),
            user: user.clone(),
            session,
        };
        let temporary = directory.join(format!(
            ".session-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "invalid session clock")?
                .as_nanos()
        ));
        let committed = (|| -> Result<()> {
            save_private_new(
                &temporary,
                &serde_json::to_value(record).map_err(|_| "cannot encode X session")?,
            )?;
            fs::rename(&temporary, &destination).map_err(|_| "cannot commit X session")?;
            File::open(&directory)
                .and_then(|f| f.sync_all())
                .map_err(|_| "cannot sync X session".into())
        })();
        if committed.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        committed?;
        let _ = fs::remove_file(candidate);
        self.x = Some(XState {
            account: self.account.clone(),
            identity,
            client,
        });
        Ok(
            json!({"status":"online","scope":self.scope(Platform::X),"user":user,"session_saved":true,"host_credentials_used":false}),
        )
    }
    pub(super) fn call_x(&mut self, method: &str, params: &Value) -> Result<Value> {
        match method {
            "x.status" => {
                fields(params, &["account"])?;
                let alias = params
                    .get("account")
                    .map(|v| v.as_str().ok_or("invalid account alias"))
                    .transpose()?
                    .unwrap_or(&self.account);
                let scope = Scope {
                    platform: Platform::X,
                    account: alias.into(),
                };
                scope.validate()?;
                let Some(path) = super::auth::saved_session(&self.state, "x", alias)? else {
                    return Ok(json!({"status":"signed_out","scope":scope,"online_checked":false}));
                };
                let saved = saved(&path)?;
                Ok(
                    json!({"status":"saved_session","scope":scope,"user":saved.user,"online_checked":false}),
                )
            }
            "x.me" => self.x_operator_reuse(),
            "x.media" => {
                fields(params, &["asset"])?;
                self.x_download(required_str(params, "asset")?)
            }
            _ => Err("unsupported X management method".into()),
        }
    }
    pub fn x_resume_import(&mut self) -> Result<Value> {
        if self.account == "public" {
            return Err("X login needs an explicit account alias".into());
        }
        let path = crate::private_state::account_path(&self.state, "x", &self.account)?
            .join("login-candidate.json");
        let metadata = fs::symlink_metadata(&path).map_err(|_| "no pending X private import")?;
        if !metadata.is_file()
            || metadata
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_none_or(|t| t.as_secs() > 900)
        {
            if metadata.is_file() {
                let _ = fs::remove_file(&path);
            }
            return Err("pending X private import expired; provide a new private input".into());
        }
        self.x_import_login(&path)
    }
    pub(super) fn x_task_matches(&self, method: &str, params: &Value) -> bool {
        if !method.starts_with("social.") {
            return false;
        }
        if params["platform"] == "x" {
            return true;
        }
        if let Some(value) = params["reference"].as_str() {
            if [
                "https://x.com/",
                "https://www.x.com/",
                "https://twitter.com/",
                "https://www.twitter.com/",
            ]
            .iter()
            .any(|p| value.starts_with(p))
            {
                return true;
            }
        }
        for key in ["reference", "target_ref"] {
            if params[key]
                .as_str()
                .is_some_and(|v| self.locators.scope_for(TARGET, v).is_ok())
            {
                return true;
            }
        }
        params["continuation"]
            .as_str()
            .is_some_and(|v| self.locators.scope_for(CURSOR, v).is_ok() || self.locators.scope_for(HOME_CURSOR, v).is_ok())
    }
    pub(super) fn call_x_discover(&mut self, params: &Value) -> Result<Value> {
        self.call_x_social("social.discover", params)
    }
    pub(super) fn call_x_social(&mut self, method: &str, params: &Value) -> Result<Value> {
        if let Some(state) = self.x.as_ref() {
            state.client.clear_diagnostic();
        }
        let run = self.x_social_inner(method, params);
        match run {
            Err(e) if e.starts_with(FAILURE) => Ok(
                json!({"status":e.trim_start_matches(FAILURE),"platform":"x","automatic_retry":false,"last_request":self.x.as_ref().and_then(|x|x.client.last_request())}),
            ),
            result => result,
        }
    }
    fn x_social_inner(&mut self, method: &str, params: &Value) -> Result<Value> {
        if params.get("platform").is_some_and(|value| value != "x") {
            return Err("X reference belongs to a different platform than requested".into());
        }
        if method == "social.discover" {return self.x_discover_inner(params)}
        if !matches!(
            method,
            "social.open" | "social.profile" | "social.creator_content" | "social.publications"
        ) {
            return Ok(
                json!({"status":"unsupported","platform":"x","detail":"X currently supports profile, content open and author posts; no write or discussion request was performed."}),
            );
        }
        fields(
            params,
            match method {
                "social.profile" => &["platform", "account"],
                "social.open" => &["reference", "platform", "account"],
                "social.creator_content" => {
                    &["target_ref", "continuation", "account", "kind", "limit"]
                }
                _ => &["platform", "continuation", "account", "kind", "limit"],
            },
        )?;
        if params["kind"]
            .as_str()
            .is_some_and(|v| !matches!(v, "all" | "posts" | "post"))
        {
            return Ok(
                json!({"status":"unsupported","platform":"x","detail":"X supports posts only; a different collection was not substituted."}),
            );
        }
        let limit =
            u32::try_from(number(params, "limit", 10)?).map_err(|_| "invalid X page limit")?;
        if !(1..=20).contains(&limit) {
            return Err("X page limit must be 1..20".into());
        }
        if let Some(reference) = params["continuation"].as_str() {
            if params.get("target_ref").is_some()
                || params.get("reference").is_some()
                || !matches!(method, "social.creator_content" | "social.publications")
            {
                return Err("X continuation cannot be combined with a new target".into());
            }
            let scope = self.locators.scope_for(CURSOR, reference)?;
            super::tasks::check_account(params, &scope)?;
            if scope.platform != Platform::X {
                return Err("X continuation platform mismatch".into());
            }
            let cursor: Cursor = self.locators.get(&scope, CURSOR, reference)?;
            id(&cursor.user_id)?;
            handle(&cursor.screen_name)?;
            if cursor.cursor.len() > 4096
                || cursor.seen.len() > 512
                || cursor.seen.iter().any(|v| id(v).is_err())
            {
                return Err("invalid X continuation".into());
            }
            return self.with_task_scope(&scope, |rt| {
                rt.x_posts(
                    cursor.user_id,
                    cursor.screen_name,
                    Some(cursor.cursor),
                    cursor.seen,
                    limit,
                )
            });
        }
        if let Some(reference) = params["reference"]
            .as_str()
            .or(params["target_ref"].as_str())
        {
            if let Ok(scope) = self.locators.scope_for(TARGET, reference) {
                super::tasks::check_account(params, &scope)?;
                if scope.platform != Platform::X {
                    return Err("X target platform mismatch".into());
                }
                let target: Target = self.locators.get(&scope, TARGET, reference)?;
                return self
                    .with_task_scope(&scope, |rt| rt.x_open_or_posts(method, target, limit));
            }
            let target = parsed(reference)?;
            return self.with_platform_account("x", params, true, |rt| {
                rt.x_open_or_posts(method, target, limit)
            });
        }
        self.with_platform_account("x", params, true, |rt| {
            let state = rt.x_client()?;
            let user = serde_json::to_value(state.client.current_user().map_err(error)?)
                .map_err(|_| "cannot encode X user")?;
            if user_identity(&user)? != state.identity {
                return Err("X session identity mismatch".into());
            }
            let user_id = user_identity(&user)?.to_owned();
            let name = required_str(&user, "screen_name")?.to_owned();
            if method == "social.publications" {
                rt.x_posts(user_id, name, None, BTreeSet::new(), limit)
            } else if method == "social.profile" {
                rt.x_user_value(user)
            } else {
                Err("X content open requires a reference".into())
            }
        })
    }
    fn x_open_or_posts(&mut self, method: &str, target: Target, limit: u32) -> Result<Value> {
        if matches!(method, "social.creator_content" | "social.publications") {
            if let Some((id, screen_name)) = target.creator()? {
                return self.x_posts(id.into(), screen_name.into(), None, BTreeSet::new(), limit);
            }
        }
        match target {
            Target::User { id, screen_name } => {
                let user = if id.is_empty() {
                    self.x_client()?.client.user_by_screen_name(&screen_name)
                } else {
                    self.x_client()?.client.user_by_id(&id)
                }
                .map_err(error)?;
                let user = serde_json::to_value(user).map_err(|_| "cannot encode X user")?;
                if !id.is_empty() && user_identity(&user)? != id {
                    return Err("X target user identity mismatch".into());
                }
                if method == "social.creator_content" || method == "social.publications" {
                    self.x_posts(
                        user_identity(&user)?.into(),
                        required_str(&user, "screen_name")?.into(),
                        None,
                        BTreeSet::new(),
                        limit,
                    )
                } else {
                    self.x_user_value(user)
                }
            }
            Target::Post {
                id,
                author_id,
                screen_name,
            } => {
                let post = serde_json::to_value(self.x_client()?.client.tweet(&id).map_err(error)?)
                    .map_err(|_| "cannot encode X post")?;
                if post["id"].as_str() != Some(&id) {
                    return Err("X post identity mismatch".into());
                }
                if !author_id.is_empty() && user_identity(&post["author"])? != author_id {
                    return Err("X author identity mismatch".into());
                }
                if !screen_name.is_empty()
                    && post["author"]["screen_name"]
                        .as_str()
                        .is_none_or(|s| !s.eq_ignore_ascii_case(&screen_name))
                {
                    return Err("X post URL author mismatch".into());
                }
                if method == "social.creator_content" {
                    self.x_posts(
                        user_identity(&post["author"])?.into(),
                        required_str(&post["author"], "screen_name")?.into(),
                        None,
                        BTreeSet::new(),
                        limit,
                    )
                } else {
                    let post = self.x_post_value(post)?;
                    Ok(
                        json!({"status":"ok","scope":self.scope(Platform::X),"content":post,"content_observed":true}),
                    )
                }
            }
        }
    }
    fn x_user_value(&self, mut user: Value) -> Result<Value> {
        let target = Target::User {
            id: user_identity(&user)?.into(),
            screen_name: required_str(&user, "screen_name")?.into(),
        };
        user["target_ref"] = json!(self.locators.put_reusable(
            &self.scope(Platform::X),
            TARGET,
            &target
        )?);
        user["open_tool"] = json!("social_open");
        user["posts_tool"] = json!("social_creator_content");
        // A profile avatar URL is not a media observation or automatic download.
        user.as_object_mut()
            .ok_or("invalid X user")?
            .remove("profile_image_url");
        Ok(
            json!({"status":"ok","scope":self.scope(Platform::X),"content":user,"content_observed":true}),
        )
    }
    fn x_post_value(&self, mut post: Value) -> Result<Value> {
        let author = &post["author"];
        let author_id = user_identity(author)?.to_owned();
        let name = required_str(author, "screen_name")?.to_owned();
        handle(&name)?;
        let post_id = required_str(&post, "id")?.to_owned();
        id(&post_id)?;
        let scope = self.scope(Platform::X);
        post["target_ref"] = json!(self.locators.put_reusable(
            &scope,
            TARGET,
            &Target::Post {
                id: post_id,
                author_id: author_id.clone(),
                screen_name: name.clone()
            }
        )?);
        post["creator_ref"] = json!(self.locators.put_reusable(
            &scope,
            TARGET,
            &Target::User {
                id: author_id,
                screen_name: name
            }
        )?);
        post["open_tool"] = json!("social_open");
        post["creator_tool"] = json!("social_creator_content");
        if let Some(object) = post["author"].as_object_mut() {
            object.remove("profile_image_url");
        }
        if let Some(media) = post["media"].as_array_mut() {
            for item in media {
                let kind = item["kind"].as_str().unwrap_or("unknown").to_owned();
                let selected = item["variants"]
                    .as_array()
                    .and_then(|v| {
                        v.iter()
                            .filter(|v| v["content_type"] == "video/mp4")
                            .min_by_key(|v| v["bitrate"].as_u64().unwrap_or(u64::MAX))
                    })
                    .and_then(|v| v["url"].as_str())
                    .or(item["url"].as_str())
                    .map(str::to_owned);
                if let Some(url) = selected {
                    let media_ref = self.locators.put_reusable(
                        &scope,
                        MEDIA,
                        &Media {
                            url,
                            kind: kind.clone(),
                        },
                    )?;
                    let object = item.as_object_mut().ok_or("invalid X media")?;
                    object.remove("url");
                    object.remove("variants");
                    object.insert("media_ref".into(), json!(media_ref));
                    object.insert("read_tool".into(), json!("media_read"));
                    object.insert("fetched".into(), json!(false));
                    object.insert("model_observed".into(), json!(false));
                }
            }
        }
        Ok(post)
    }
    fn x_discover_inner(&mut self, params: &Value) -> Result<Value> {
        fields(
            params,
            &["platform", "account", "source", "continuation", "limit"],
        )?;
        let count =
            u32::try_from(number(params, "limit", 10)?).map_err(|_| "invalid X page limit")?;
        if !(1..=20).contains(&count) {
            return Err("X page limit must be 1..20".into());
        }
        if params
            .get("source")
            .is_some_and(|value| !matches!(value.as_str(), Some("recommended" | "following")))
        {
            return Ok(
                json!({"status":"unsupported","platform":"x","available_sources":["recommended","following"],"detail":"Select the authenticated For You or Following feed; no public substitute was requested."}),
            );
        }
        if let Some(reference) = params.get("continuation") {
            let reference = reference
                .as_str()
                .ok_or("X continuation must be a string")?;
            let scope = self.locators.scope_for(HOME_CURSOR, reference)?;
            super::tasks::check_account(params, &scope)?;
            if scope.platform != Platform::X {
                return Err("X continuation platform mismatch".into());
            }
            let cursor: HomeCursor = self.locators.get(&scope, HOME_CURSOR, reference)?;
            cursor.validate()?;
            if params
                .get("source")
                .is_some_and(|source| source.as_str() != Some(cursor.source.as_str()))
            {
                return Err("X continuation source differs from requested source".into());
            }
            return self.with_task_scope(&scope, |rt| rt.x_home_page(cursor, count as usize));
        }
        super::tasks::check_account(params, &self.scope(Platform::X))?;
        self.x_home_page(
            HomeCursor {
                source: params["source"].as_str().unwrap_or("recommended").into(),
                started: false,
                provider_cursor: None,
                provider_has_more: None,
                pending: Vec::new(),
                seen: BTreeSet::new(),
                warnings: Vec::new(),
            },
            count as usize,
        )
    }
    fn x_home_page(&mut self, mut saved: HomeCursor, limit: usize) -> Result<Value> {
        saved.validate()?;
        if saved.pending.is_empty() && (!saved.started || saved.provider_cursor.is_some()) {
            let previous = saved.provider_cursor.take();
            let page = self
                .x_client()?
                .client
                .home_timeline(saved.source == "recommended", previous.as_deref(), 20)
                .map_err(error)?;
            saved.started = true;
            saved.provider_has_more = page.has_more;
            saved.provider_cursor = page
                .next_cursor
                .filter(|v| Some(v.as_str()) != previous.as_deref());
            saved.warnings = page.warnings.into_iter().take(16).collect();
            if let Some(note) = page.pagination_note {
                saved.warnings.push(note);
                saved.warnings.truncate(16)
            }
            for post in page.pinned.into_iter().chain(page.items) {
                if saved.pending.len() >= 100 {
                    return Err("X home page exceeds bound".into());
                }
                saved
                    .pending
                    .push(serde_json::to_value(post).map_err(|_| "cannot encode X post")?);
            }
        }
        let mut items = Vec::new();
        let mut consumed = 0;
        for post in &saved.pending {
            if items.len() >= limit || saved.seen.len() >= 512 {
                break;
            }
            consumed += 1;
            let post_id = required_str(post, "id")?;
            id(post_id)?;
            if saved.seen.insert(post_id.to_owned()) {
                items.push(self.x_post_value(post.clone())?)
            }
        }
        saved.pending.drain(..consumed);
        // A stale provider cursor or a repeated/empty page must not create an
        // infinite autonomous polling loop. A fresh discovery is always explicit.
        if items.is_empty() {
            saved.provider_cursor = None;
            saved.pending.clear()
        }
        let source = saved.source.clone();
        let provider_has_more = saved.provider_has_more;
        let warnings = saved.warnings.clone();
        let more = (!saved.pending.is_empty() || saved.provider_cursor.is_some())
            && saved.seen.len() < 512;
        let continuation = if more {
            Some(
                self.locators
                    .put(&self.scope(Platform::X), HOME_CURSOR, &saved)?,
            )
        } else {
            None
        };
        Ok(
            json!({"status":"ok","scope":self.scope(Platform::X),"source":source,"context":{"source":if source=="recommended"{"x_for_you"}else{"x_following"},"signed_in_request":true,"personalization_verified":false},"items":items,"continuation":continuation,"has_more":more,"provider_has_more":provider_has_more,"warnings":warnings,"content_observed":true,"media_downloaded":false,"stop_reason":if more{"page_complete"}else if saved.seen.len()>=512{"session_item_budget"}else if items.is_empty(){"no_new_items"}else{"provider_end_or_no_cursor"}}),
        )
    }
    fn x_posts(
        &mut self,
        user_id: String,
        screen_name: String,
        cursor: Option<String>,
        mut seen: BTreeSet<String>,
        limit: u32,
    ) -> Result<Value> {
        id(&user_id)?;
        handle(&screen_name)?;
        let page = serde_json::to_value(
            self.x_client()?
                .client
                .user_timeline(&user_id, cursor.as_deref(), limit)
                .map_err(error)?,
        )
        .map_err(|_| "cannot encode X page")?;
        let mut items = Vec::new();
        let mut pinned = Vec::new();
        if let Some(posts) = page["pinned"].as_array() {
            for post in posts.iter().take(20) {
                let post_id = required_str(post, "id")?;
                id(post_id)?;
                if seen.len() >= 512 {
                    break;
                }
                if seen.insert(post_id.into()) {
                    let mut post = self.x_post_value(post.clone())?;
                    post["pinned"] = json!(true);
                    pinned.push(post);
                }
            }
        }
        for post in page["items"].as_array().ok_or("X timeline omitted items")? {
            let post_id = required_str(post, "id")?;
            id(post_id)?;
            if seen.contains(post_id) {
                continue;
            }
            if seen.len() >= 512 {
                break;
            }
            seen.insert(post_id.into());
            items.push(self.x_post_value(post.clone())?);
        }
        let next = page["next_cursor"].as_str().filter(|n| {
            !n.is_empty()
                && n.len() <= 4096
                && Some(*n) != cursor.as_deref()
                && seen.len() < 512
                && page["truncated"] != true
        });
        let continuation = next
            .map(|next| {
                self.locators.put(
                    &self.scope(Platform::X),
                    CURSOR,
                    &Cursor {
                        user_id: user_id.clone(),
                        screen_name: screen_name.clone(),
                        cursor: next.into(),
                        seen,
                    },
                )
            })
            .transpose()?;
        Ok(
            json!({"status":"ok","scope":self.scope(Platform::X),"author":{"id":user_id,"screen_name":screen_name},"items":items,"pinned":pinned,"continuation":continuation,"has_more":if continuation.is_some(){json!(true)}else if page["has_more"]==false{json!(false)}else{Value::Null},"provider_has_more":page["has_more"],"timeline_kind":page["timeline_kind"],"truncated":page["truncated"],"warnings":page["warnings"],"pagination_note":page["pagination_note"],"stop_reason":if continuation.is_some(){"page_complete"}else if page["has_more"]==true{"cursor_unavailable_or_budget"}else if page["has_more"]==false{"provider_end"}else{"pagination_unknown"},"content_observed":true}),
        )
    }
    pub(super) fn x_download(&mut self, reference: &str) -> Result<Value> {
        let scope = self.locators.scope_for(MEDIA, reference)?;
        if scope != self.scope(Platform::X) {
            return Err("X media belongs to another account".into());
        }
        let media: Media = self.locators.get(&scope, MEDIA, reference)?;
        let path = self.state.join("work").join(format!("x-media-{reference}"));
        let fetched =
            match self
                .x_client()?
                .client
                .download_media(&media.url, &path, 64 * 1024 * 1024)
            {
                Ok(value) => value,
                Err(e) => {
                    let _ = fs::remove_file(&path);
                    return Err(error(e));
                }
            };
        let result = (|| {
            let group = if media.kind == "photo" {
                "image"
            } else {
                "video"
            };
            let mime = download_mime(&path, fetched.mime_type.as_deref(), group)?;
            let mut file = File::open(&path).map_err(|_| "cannot open downloaded X media")?;
            let resource = self.store.import(&scope, "x-media", &mime, &mut file)?;
            Ok(json!({"resource":resource,"bytes":fetched.bytes,"media_observed":false}))
        })();
        let _ = fs::remove_file(path);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returned_creator_identity_does_not_require_reopening_its_source() {
        let post = Target::Post {
            id: "99".into(),
            author_id: "42".into(),
            screen_name: "fixture".into(),
        };
        assert_eq!(post.creator().unwrap(), Some(("42", "fixture")));
        let user = Target::User {
            id: "42".into(),
            screen_name: "fixture".into(),
        };
        assert_eq!(user.creator().unwrap(), Some(("42", "fixture")));
        let unresolved = Target::User {
            id: String::new(),
            screen_name: "fixture".into(),
        };
        assert_eq!(unresolved.creator().unwrap(), None);
        let invalid = Target::User {
            id: "42".into(),
            screen_name: String::new(),
        };
        assert!(invalid.creator().is_err());
    }
    #[test]
    fn reference_parser_rejects_foreign_hosts_and_ambiguous_paths() {
        for value in [
            "https://x.com.evil.invalid/a/status/42",
            "https://x.com@evil.invalid/a/status/42",
            "https://x.com:443/a/status/42",
            "https://x.com/a/status/42/extra",
            "https://x.com/a%2fb/status/42",
            "javascript:alert(1)",
        ] {
            assert!(parsed(value).is_err());
        }
        assert!(matches!(
            parsed("https://x.com/example/status/42?s=20").unwrap(),
            Target::Post { .. }
        ));
        assert!(matches!(parsed("@example").unwrap(), Target::User { .. }));
    }

    fn lab() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "runtime-x-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        private_dir(&path).unwrap();
        path
    }
    #[test]
    fn home_continuation_preserves_cached_tail_source_account_and_lazy_media_without_network() {
        let path = lab();
        let mut rt = Runtime::open(&path.join("state"), "owner", None).unwrap();
        let post = |id: &str| json!({"id":id,"author":{"id":"42","screen_name":"fixture","name":"Fixture"},"text":"Synthetic home post","counts":{"like":3},"media":[{"kind":"photo","url":"https://pbs.twimg.com/media/fixture.jpg","variants":[]}]});
        let cursor = HomeCursor {
            source: "recommended".into(),
            started: true,
            provider_cursor: None,
            provider_has_more: Some(false),
            pending: vec![post("111"), post("112"), post("113")],
            seen: BTreeSet::new(),
            warnings: Vec::new(),
        };
        let first = rt.x_home_page(cursor, 1).unwrap();
        assert_eq!(first["context"]["source"], "x_for_you");
        assert_eq!(first["context"]["personalization_verified"], false);
        assert_eq!(first["items"].as_array().unwrap().len(), 1);
        let item = &first["items"][0];
        assert!(item["target_ref"].is_string());
        assert!(item["creator_ref"].is_string());
        assert_eq!(item["media"][0]["fetched"], false);
        assert!(item["media"][0].get("url").is_none());
        assert!(item["media"][0]["media_ref"].is_string());
        let reference = first["continuation"].as_str().unwrap();
        assert!(
            rt.call_x_discover(&json!({"continuation":reference,"source":"following"}))
                .unwrap_err()
                .contains("source differs")
        );
        assert!(
            rt.call_x_discover(&json!({"continuation":reference,"account":"someone-else"}))
                .unwrap_err()
                .contains("account differs")
        );
        let rest = rt
            .call_x_discover(&json!({"continuation":reference,"limit":2}))
            .unwrap();
        assert_eq!(rest["items"][0]["id"], "112");
        assert_eq!(rest["items"][1]["id"], "113");
        assert_eq!(rest["has_more"], false);
        assert_eq!(rest["scope"]["account"], "owner");
        assert_eq!(rest["source"], "recommended");
        assert!(rest["continuation"].is_null());
        assert_eq!(
            rt.call_x_discover(&json!({"source":"popular"})).unwrap()["status"],
            "unsupported"
        );
        assert_eq!(
            rt.call_x_discover(&json!({})).unwrap()["status"],
            "needs_auth"
        );
        assert!(!rt.state.join("outbox").exists());
        drop(rt);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn local_status_and_invalid_private_import_do_not_start_a_network_login() {
        let path = lab();
        let mut rt = Runtime::open(&path.join("state"), "owner", None).unwrap();
        assert_eq!(
            rt.call("x.status", &json!({})).unwrap()["status"],
            "signed_out"
        );
        assert_eq!(
            rt.call("runtime.accounts", &json!({})).unwrap()["x"],
            json!([])
        );
        let input = path.join("input");
        fs::write(&input, r#"{"auth_token":"synthetic"}"#).unwrap();
        fs::set_permissions(&input, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(rt.x_import_login(&input).unwrap_err().contains("0600"));
        fs::set_permissions(&input, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            rt.x_import_login(&input)
                .unwrap_err()
                .contains("invalid imported session")
        );
        assert!(
            super::super::auth::saved_session(&rt.state, "x", "owner")
                .unwrap()
                .is_none()
        );
        drop(rt);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn stored_x_target_keeps_its_account_and_writes_remain_unsupported() {
        let path = lab();
        let mut rt = Runtime::open(&path.join("state"), "public", None).unwrap();
        let scope = Scope {
            platform: Platform::X,
            account: "owner".into(),
        };
        let reference = rt
            .locators
            .put_reusable(
                &scope,
                TARGET,
                &Target::User {
                    id: "42".into(),
                    screen_name: "example".into(),
                },
            )
            .unwrap();
        assert!(
            rt.call(
                "social.open",
                &json!({"reference":reference,"account":"another"})
            )
            .unwrap_err()
            .contains("account differs")
        );
        assert_eq!(
            rt.call("social.like", &json!({"target_ref":reference}))
                .unwrap()["status"],
            "unsupported"
        );
        assert!(rt.call("social.open",&json!({"platform":"x","reference":"@example","auth_token":"must-not-be-a-tool-argument"})).unwrap_err().contains("unsupported X task parameter"));
        drop(rt);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn expired_private_candidate_is_removed_without_authentication_or_a_saved_login() {
        let path = lab();
        let mut rt = Runtime::open(&path.join("state"), "owner", None).unwrap();
        let directory = account_directory(&rt.state, "x", "owner").unwrap();
        let candidate = directory.join("login-candidate.json");
        save_private_new(
            &candidate,
            &json!({"auth_token":"synthetic","ct0":"synthetic"}),
        )
        .unwrap();
        File::options()
            .write(true)
            .open(&candidate)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new().set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(1000),
                ),
            )
            .unwrap();
        assert!(rt.x_resume_import().unwrap_err().contains("expired"));
        assert!(!candidate.exists());
        assert!(!directory.join("session.json").exists());
        assert_eq!(
            rt.call("x.status", &json!({})).unwrap()["status"],
            "signed_out"
        );
        drop(rt);
        fs::remove_dir_all(path).unwrap();
    }
}
