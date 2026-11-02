// SPDX-License-Identifier: MIT
//! GitHub task surface: opaque account-bound targets, lazy file reads and durable
//! single-attempt comments. No host gh credentials or browser state are consulted.
use super::*;
use crate::{
    outbox::{Operation, Outbox, Status},
    private_state::account_directory,
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, VecDeque};

const TARGET: &str = "task-github-target";
const CONTINUATION: &str = "task-github-list";
const MAX_BODY_BYTES: usize = 256 * 1024;
const PREVIEW_CHARS: usize = 2000;
const PAGE_SIZE: u32 = 20;
const MAX_SEEN: usize = 2048;
const MAX_SEEN_BYTES: usize = 256 * 1024;
// Module-private bridge across runtime_core::Result<String>. Platform errors
// retain their typed fields until the one task boundary decodes this carrier.
const TASK_ERROR: &str = "\u{1e}github-task-error:";
const DEFAULT_BACKOFF_SECONDS: u64 = 60;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Backoff {
    schema: u32,
    scope: Scope,
    observed_at: u64,
    retry_at: u64,
    reset_at: Option<u64>,
    retry_after_seconds: Option<u64>,
}
impl Backoff {
    fn new(
        scope: Scope,
        observed_at: u64,
        retry_after_seconds: Option<u64>,
        reset_at: Option<u64>,
    ) -> Self {
        let specified = reset_at.unwrap_or(0).max(
            retry_after_seconds
                .map(|n| observed_at.saturating_add(n))
                .unwrap_or(0),
        );
        Self {
            schema: 1,
            scope,
            observed_at,
            retry_at: if specified > observed_at {
                specified
            } else {
                observed_at.saturating_add(DEFAULT_BACKOFF_SECONDS)
            },
            reset_at,
            retry_after_seconds,
        }
    }
    fn validate(&self, scope: &Scope, now: u64) -> Result<()> {
        self.scope.validate()?;
        let expected = Self::new(
            scope.clone(),
            self.observed_at,
            self.retry_after_seconds,
            self.reset_at,
        );
        if self.schema != 1
            || &self.scope != scope
            || scope.platform != Platform::Github
            || self.observed_at > now.saturating_add(300)
            || self.retry_at != expected.retry_at
        {
            return Err("invalid account-local GitHub backoff record".into());
        }
        Ok(())
    }
    fn response(&self, now: u64, blocked_locally: bool) -> Value {
        json!({"status":"rate_limited","scope":self.scope,"retry_at":self.retry_at,"reset_at":self.reset_at,"retry_after_seconds":self.retry_after_seconds,"remaining_seconds":self.retry_at.saturating_sub(now),"observed_at":self.observed_at,"time_unit":"unix_seconds","blocked_locally":blocked_locally,"network_retry_started":false,"retry_at_source":if self.reset_at.is_some_and(|n|n==self.retry_at){"server_reset"}else if self.retry_after_seconds.is_some_and(|n|self.observed_at.saturating_add(n)==self.retry_at){"server_retry_after"}else{"local_default_60s"},"detail":"No sleep, retry, account change or IP change was started. Cached pages and completed operation replays remain available."})
    }
}

fn task_error(value: Value) -> String {
    format!("{TASK_ERROR}{value}")
}
fn decode_task_error(error: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(error.strip_prefix(TASK_ERROR)?).ok()?;
    matches!(
        value["status"].as_str(),
        Some("rate_limited" | "needs_auth" | "permission_denied")
    )
    .then_some(value)
}
fn human_task_error(error: String) -> String {
    if let Some(value) = decode_task_error(&error) {
        format!(
            "GitHub request blocked: {} (retry_at={}, reset_at={}, retry_after_seconds={}); no automatic retry was started",
            value["status"], value["retry_at"], value["reset_at"], value["retry_after_seconds"]
        )
    } else {
        error
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    schema: u32,
    token: String,
    id: u64,
    login: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    oauth: Option<OAuthSession>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthSession {
    client_id: String,
    expires_at: Option<u64>,
    refresh_token: Option<String>,
    refresh_expires_at: Option<u64>,
    scope: String,
}
impl OAuthSession {
    fn from_tokens(
        tokens: &platform_github::auth::Tokens,
        client_id: &str,
        now: u64,
    ) -> Result<Self> {
        let expiry = |seconds: Option<u64>| {
            seconds
                .map(|n| now.checked_add(n).ok_or("invalid GitHub OAuth expiry"))
                .transpose()
        };
        let value = Self {
            client_id: client_id.into(),
            expires_at: expiry(tokens.expires_in)?,
            refresh_token: tokens.refresh_token.clone(),
            refresh_expires_at: expiry(tokens.refresh_expires_in)?,
            scope: tokens.scope.clone(),
        };
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> Result<()> {
        if !platform_github::auth::valid_client_id(&self.client_id)
            || self.scope.len() > 2048
            || self.scope.chars().any(char::is_control)
            || self.refresh_token.as_ref().is_some_and(|s| {
                s.is_empty() || s.len() > 4096 || !s.bytes().all(|b| b.is_ascii_graphic())
            })
            || self.expires_at.is_some() && self.refresh_token.is_none()
            || self.refresh_expires_at.is_some() && self.refresh_token.is_none()
        {
            return Err("invalid private GitHub OAuth session".into());
        }
        Ok(())
    }
}
impl Session {
    fn validate(&self) -> Result<()> {
        if self.schema != 1
            || self.id == 0
            || !valid_login(&self.login)
            || self.token.is_empty()
            || self.token.len() > 4096
            || !self.token.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err("invalid private GitHub session".into());
        }
        if let Some(oauth) = &self.oauth {
            oauth.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Target {
    Repository {
        owner: String,
        repo: String,
    },
    Issue {
        owner: String,
        repo: String,
        number: u64,
        pull: bool,
    },
    Comment {
        owner: String,
        repo: String,
        number: u64,
        pull: bool,
        comment: u64,
    },
    ReviewComment {
        owner: String,
        repo: String,
        number: u64,
        comment: u64,
    },
    File {
        owner: String,
        repo: String,
        path: String,
        revision: String,
        directory: bool,
    },
}
impl Target {
    fn repository(&self) -> (&str, &str) {
        match self {
            Self::Repository { owner, repo }
            | Self::Issue { owner, repo, .. }
            | Self::Comment { owner, repo, .. }
            | Self::ReviewComment { owner, repo, .. }
            | Self::File { owner, repo, .. } => (owner, repo),
        }
    }
    fn issue(&self) -> Option<(u64, bool)> {
        match self {
            Self::Issue { number, pull, .. } | Self::Comment { number, pull, .. } => {
                Some((*number, *pull))
            }
            Self::ReviewComment { number, .. } => Some((*number, true)),
            _ => None,
        }
    }
    fn validate(&self) -> Result<()> {
        let (owner, repo) = self.repository();
        validate_repository(owner, repo)?;
        match self {
            Self::Issue { number, .. } if *number == 0 => {
                return Err("invalid GitHub issue number".into());
            }
            Self::Comment {
                number, comment, ..
            }
            | Self::ReviewComment {
                number, comment, ..
            } if *number == 0 || *comment == 0 => {
                return Err("invalid GitHub comment target".into());
            }
            Self::File { path, revision, .. } => {
                validate_path(path)?;
                validate_sha(revision)?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ListKind {
    Issues,
    Pulls,
    Comments,
    ReviewComments,
    Reviews,
    Changes,
    Files,
    Notifications,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    kind: ListKind,
    target: Option<Target>,
    page: u32,
    seen: VecDeque<String>,
    #[serde(default)]
    pending: Option<Pending>,
    #[serde(default)]
    changes: Option<ChangeSnapshot>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    resource: String,
    offset: usize,
    next_page: Option<u32>,
    context: Value,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Repository {
    owner: String,
    repo: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeSnapshot {
    head_sha: String,
    base_sha: String,
    head_repository: Option<Repository>,
    base_repository: Option<Repository>,
}
impl ChangeSnapshot {
    fn from_pull(data: &Value) -> Result<Self> {
        fn repository(data: &Value) -> Result<Option<Repository>> {
            let Some(name) = data["repo"]["full_name"].as_str() else {
                return Ok(None);
            };
            let (owner, repo) = name.split_once('/').ok_or("invalid PR repository")?;
            validate_repository(owner, repo)?;
            Ok(Some(Repository {
                owner: owner.to_ascii_lowercase(),
                repo: repo.to_ascii_lowercase(),
            }))
        }
        let snapshot = Self {
            head_sha: data["head"]["sha"]
                .as_str()
                .ok_or("PR head SHA is missing")?
                .to_ascii_lowercase(),
            base_sha: data["base"]["sha"]
                .as_str()
                .ok_or("PR base SHA is missing")?
                .to_ascii_lowercase(),
            head_repository: repository(&data["head"])?,
            base_repository: repository(&data["base"])?,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
    fn validate(&self) -> Result<()> {
        validate_sha(&self.head_sha)?;
        validate_sha(&self.base_sha)?;
        for repository in [&self.head_repository, &self.base_repository]
            .into_iter()
            .flatten()
        {
            validate_repository(&repository.owner, &repository.repo)?;
        }
        Ok(())
    }
}
impl Cursor {
    fn validate(&self) -> Result<()> {
        if self.page == 0
            || self.page > 10000
            || self.seen.len() > MAX_SEEN
            || self.seen.iter().any(|s| s.len() > 4096)
            || self.seen.iter().map(String::len).sum::<usize>() > MAX_SEEN_BYTES
            || self.seen.iter().collect::<BTreeSet<_>>().len() != self.seen.len()
        {
            return Err("invalid GitHub continuation bounds".into());
        }
        if let Some(target) = &self.target {
            target.validate()?;
        }
        if self.kind == ListKind::Notifications {
            if self.target.is_some() {
                return Err("notification continuation has a repository target".into());
            }
        } else if self.target.is_none() {
            return Err("GitHub continuation lacks its target".into());
        }
        if self.pending.as_ref().is_some_and(|p| {
            !opaque(&p.resource)
                || p.offset > 1000
                || p.next_page.is_some_and(|n| n <= self.page || n > 10000)
        }) {
            return Err("invalid GitHub pending page".into());
        }
        if let Some(snapshot) = &self.changes {
            if self.kind != ListKind::Changes {
                return Err("unexpected PR snapshot in continuation".into());
            }
            snapshot.validate()?;
        }
        Ok(())
    }
}

enum Parsed {
    Target(Target),
    AmbiguousFile { owner: String, repo: String },
}
fn valid_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 39
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn validate_repository(owner: &str, repo: &str) -> Result<()> {
    if !valid_login(owner)
        || repo.is_empty()
        || repo.len() > 100
        || matches!(repo, "." | "..")
        || !repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Err("invalid GitHub owner/repository".into());
    }
    Ok(())
}
fn opaque(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_sha(sha: &str) -> Result<()> {
    if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("file reference must be frozen to a complete commit SHA".into());
    }
    Ok(())
}
fn validate_path(path: &str) -> Result<()> {
    if path.len() > 2048
        || path.starts_with('/')
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || (!path.is_empty()
            && path
                .split('/')
                .any(|p| p.is_empty() || matches!(p, "." | "..")))
    {
        return Err("invalid repository-relative file path".into());
    }
    Ok(())
}
fn decode_component(value: &str) -> Result<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.bytes();
    while let Some(b) = input.next() {
        if b == b'%' {
            let a = input
                .next()
                .and_then(|v| (v as char).to_digit(16))
                .ok_or("invalid URL escape")?;
            let b = input
                .next()
                .and_then(|v| (v as char).to_digit(16))
                .ok_or("invalid URL escape")?;
            bytes.push((a * 16 + b) as u8);
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).map_err(|_| "GitHub URL path must be UTF-8".into())
}
fn parse_reference(reference: &str) -> Result<Parsed> {
    if reference.is_empty() || reference.len() > 4096 || reference.chars().any(char::is_control) {
        return Err("invalid GitHub reference".into());
    }
    let raw = if let Some(path) = reference.strip_prefix("https://github.com/") {
        path
    } else if reference.contains("://") || reference.starts_with('/') {
        return Err("use an exact https://github.com/ URL or owner/repo".into());
    } else {
        reference
    };
    let (raw, fragment) = raw
        .split_once('#')
        .map_or((raw, None), |(a, b)| (a, Some(b)));
    let (path, query) = raw
        .split_once('?')
        .map_or((raw, None), |(a, b)| (a, Some(b)));
    if query.is_some_and(|q| !q.is_empty()) {
        return Err(
            "GitHub links with query selectors are unsupported; use the canonical content URL"
                .into(),
        );
    }
    let parts: Vec<String> = path
        .trim_end_matches('/')
        .split('/')
        .map(decode_component)
        .collect::<Result<_>>()?;
    if parts.len() < 2 {
        return Err("GitHub reference needs owner/repo".into());
    }
    let owner = parts[0].to_ascii_lowercase();
    let repo = parts[1].to_ascii_lowercase();
    validate_repository(&owner, &repo)?;
    let target = match parts.len() {
        2 => Target::Repository { owner, repo },
        4 if matches!(parts[2].as_str(), "issues" | "pull") => {
            let number = parts[3]
                .parse::<u64>()
                .ok()
                .filter(|v| *v > 0)
                .ok_or("invalid GitHub issue/PR number")?;
            let pull = parts[2] == "pull";
            if let Some(anchor) = fragment {
                if let Some(comment) = anchor.strip_prefix("issuecomment-") {
                    let comment = comment
                        .parse::<u64>()
                        .ok()
                        .filter(|v| *v > 0)
                        .ok_or("invalid issue comment anchor")?;
                    Target::Comment {
                        owner,
                        repo,
                        number,
                        pull,
                        comment,
                    }
                } else if anchor.is_empty() {
                    Target::Issue {
                        owner,
                        repo,
                        number,
                        pull,
                    }
                } else {
                    return Err("unsupported GitHub discussion anchor; inline review comments are available through github_list review_comments".into());
                }
            } else {
                Target::Issue {
                    owner,
                    repo,
                    number,
                    pull,
                }
            }
        }
        n if n >= 4 && matches!(parts[2].as_str(), "blob" | "tree") => {
            if validate_sha(&parts[3]).is_err() {
                return Ok(Parsed::AmbiguousFile { owner, repo });
            }
            let path = parts[4..].join("/");
            validate_path(&path)?;
            if parts[2] == "blob" && path.is_empty() {
                return Err("file URL has no path".into());
            }
            Target::File {
                owner,
                repo,
                path,
                revision: parts[3].to_ascii_lowercase(),
                directory: parts[2] == "tree",
            }
        }
        _ => {
            return Err(
                "unsupported GitHub URL; use a repository, issue, PR or frozen file reference"
                    .into(),
            );
        }
    };
    target.validate()?;
    Ok(Parsed::Target(target))
}

fn limit(params: &Value) -> Result<usize> {
    let limit = number(params, "limit", 10)? as usize;
    if !(1..=20).contains(&limit) {
        return Err("GitHub list limit must be 1..20".into());
    }
    Ok(limit)
}
fn allowed(params: &Value, keys: &[&str]) -> Result<()> {
    let object = params
        .as_object()
        .ok_or("GitHub task parameters must be an object")?;
    if object.keys().any(|key| !keys.contains(&key.as_str())) {
        return Err(
            "unsupported GitHub task parameter; IDs and pages are managed by Runtime".into(),
        );
    }
    Ok(())
}
fn selected(value: &Value, fields: &[&str]) -> Value {
    let mut result = serde_json::Map::new();
    for field in fields {
        if let Some(value) = value.get(*field) {
            result.insert((*field).into(), value.clone());
        }
    }
    Value::Object(result)
}
fn issue_matches(url: &str, owner: &str, repo: &str, number: u64) -> bool {
    url.eq_ignore_ascii_case(&format!(
        "https://api.github.com/repos/{owner}/{repo}/issues/{number}"
    ))
}

impl Runtime {
    pub(crate) fn github_operator_reuse(&self) -> Result<Value> {
        let (client, session) = match self.github_client(true) {
            Err(error)
                if decode_task_error(&error).is_some_and(|v| v["status"] == "needs_auth") =>
            {
                return Ok(json!({"status":"needs_auth"}));
            }
            other => other?,
        };
        let saved = session.ok_or("saved GitHub identity missing")?;
        match client.current_user() {
            Ok(user) => {
                if user["id"].as_u64() != Some(saved.id) {
                    return Err("GitHub saved session identity mismatch".into());
                }
                Ok(
                    json!({"status":"online","reused_session":true,"user":{"id":saved.id,"login":saved.login}}),
                )
            }
            Err(platform_github::Error::LoginRequired) => Ok(json!({"status":"needs_auth"})),
            Err(error) => Err(human_task_error(self.github_sdk_error(error))),
        }
    }

    fn github_session(&self, required: bool) -> Result<Option<Session>> {
        if self.account == "public" {
            return if required {
                Err("GitHub writing needs a saved personal account".into())
            } else {
                Ok(None)
            };
        }
        let path = super::auth::saved_session(&self.state, "github", &self.account)?
            .ok_or("selected GitHub account has no saved token; no public fallback was made")?;
        let mut session: Session = serde_json::from_slice(&github_private(&path, 16384)?)
            .map_err(|_| "invalid private GitHub session")?;
        session.validate()?;
        super::auth::bind_identity(
            path.parent().ok_or("missing GitHub account directory")?,
            &session.id.to_string(),
        )?;
        if let Some(oauth) = session.oauth.as_ref() {
            let now = crate::outbox::now()?;
            if oauth
                .expires_at
                .is_some_and(|expiry| expiry <= now.saturating_add(60))
            {
                if oauth.refresh_expires_at.is_some_and(|expiry| expiry <= now) {
                    return Err(self.github_sdk_error(platform_github::Error::LoginRequired));
                }
                let client = platform_github::auth::Client::new(&oauth.client_id)
                    .map_err(|e| self.github_sdk_error(e))?;
                let tokens = client
                    .refresh(
                        oauth
                            .refresh_token
                            .as_deref()
                            .ok_or("missing private GitHub refresh credential")?,
                    )
                    .map_err(|e| self.github_sdk_error(e))?;
                let metadata = OAuthSession::from_tokens(&tokens, &oauth.client_id, now)?;
                session.token = tokens.access_token;
                session.oauth = Some(metadata);
                // Runtime owns the account-state lock. Rotate both credentials
                // in one private atomic session replacement, never two files.
                self.save_github_session(&session)?;
            }
        }
        Ok(Some(session))
    }
    fn github_client(&self, required: bool) -> Result<(platform_github::Client, Option<Session>)> {
        self.github_check_backoff()?;
        let session = self.github_session(required)?;
        let client = platform_github::Client::new(platform_github::Config {
            token: session.as_ref().map(|s| s.token.clone()),
            max_file_bytes: MAX_BODY_BYTES,
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
        Ok((client, session))
    }
    fn save_github_target(&self, target: &Target) -> Result<String> {
        target.validate()?;
        self.locators
            .put_reusable(&self.scope(Platform::Github), TARGET, target)
    }
    fn load_github_target(&self, id: &str, params: &Value) -> Result<(Scope, Target)> {
        let scope = self.locators.scope_for(TARGET, id)?;
        if scope.platform != Platform::Github {
            return Err("GitHub reference belongs to another platform".into());
        }
        super::tasks::check_account(params, &scope)?;
        let target: Target = self.locators.get(&scope, TARGET, id)?;
        target.validate()?;
        Ok((scope, target))
    }

    /// Human management entry only. The model tool catalog never accepts tokens.
    pub fn github_login(&mut self, token_file: &Path) -> Result<Value> {
        if self.account == "public" {
            return Err("GitHub login needs an explicit --account alias".into());
        }
        self.github_check_backoff().map_err(human_task_error)?;
        let token = String::from_utf8(github_private(token_file, 8192)?)
            .map_err(|_| "GitHub token file must be UTF-8")?
            .trim()
            .to_owned();
        if token.is_empty() || token.len() > 4096 || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("invalid GitHub token file".into());
        }
        let client = platform_github::Client::new(platform_github::Config {
            token: Some(token.clone()),
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
        let user = client
            .current_user()
            .map_err(|e| human_task_error(self.github_sdk_error(e)))?;
        let session = Session {
            schema: 1,
            token,
            oauth: None,
            id: user["id"]
                .as_u64()
                .filter(|id| *id > 0)
                .ok_or("GitHub user response lacks an identity")?,
            login: user["login"]
                .as_str()
                .ok_or("GitHub user response lacks a login")?
                .into(),
        };
        self.save_github_session(&session)?;
        Ok(
            json!({"status":"online","scope":self.scope(Platform::Github),"user":{"id":session.id,"login":session.login},"token_saved":true,"host_credentials_used":false}),
        )
    }
    pub(crate) fn github_oauth_login(
        &self,
        tokens: &platform_github::auth::Tokens,
        client_id: &str,
        received_at: u64,
    ) -> Result<Value> {
        let metadata = OAuthSession::from_tokens(tokens, client_id, received_at)?;
        let client = platform_github::Client::new(platform_github::Config {
            token: Some(tokens.access_token.clone()),
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
        let user = match client.current_user() {
            Ok(user) => user,
            Err(error) => {
                if let Some(delay) = platform_github::auth::retry_after(&error) {
                    return Ok(json!({"status":"pending","poll_after_ms":delay*1000}));
                }
                return Ok(json!({"status":"failed","oauth_error":error.to_string()}));
            }
        };
        let session = Session {
            schema: 1,
            token: tokens.access_token.clone(),
            id: user["id"]
                .as_u64()
                .filter(|id| *id > 0)
                .ok_or("GitHub OAuth identity missing")?,
            login: user["login"]
                .as_str()
                .ok_or("GitHub OAuth login missing")?
                .into(),
            oauth: Some(metadata),
        };
        self.save_github_session(&session)?;
        Ok(
            json!({"status":"online","scope":self.scope(Platform::Github),"user":{"id":session.id,"login":session.login},"token_saved":true,"authorization":"oauth_device","host_credentials_used":false}),
        )
    }
    fn save_github_session(&self, session: &Session) -> Result<()> {
        if self.account == "public" {
            return Err("GitHub login needs a personal account alias".into());
        }
        session.validate()?;
        let directory = account_directory(&self.state, "github", &self.account)?;
        let path = directory.join("session.json");
        if fs::symlink_metadata(&path).is_ok() {
            let old: Session = serde_json::from_slice(&github_private(&path, 16384)?)
                .map_err(|_| "invalid existing GitHub session")?;
            old.validate()?;
            if old.id != session.id {
                return Err("this alias is already bound to another GitHub account".into());
            }
        }
        super::auth::bind_identity(&directory, &session.id.to_string())?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let temporary = directory.join(format!(".session-{}-{nonce}.tmp", std::process::id()));
        let result = (|| {
            save_private_new(
                &temporary,
                &serde_json::to_value(&session).map_err(|e| e.to_string())?,
            )?;
            fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
            File::open(&directory)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub(super) fn call_github_task(&mut self, method: &str, params: &Value) -> Result<Value> {
        let result = match method {
            "github.open" => self.github_open_task(params),
            "github.list" => self.github_list_task(params),
            "github.comment" => self.github_comment_task(params),
            "github.action" => self.github_action_task(params),
            "github.status" => self.github_status_task(params),
            _ => Err("unknown GitHub task".into()),
        };
        match result {
            Err(error) => match decode_task_error(&error) {
                Some(mut result) => {
                    for key in ["operation_id", "target_ref", "continuation"] {
                        if let Some(value) = params.get(key).filter(|v| v.is_string()) {
                            result[key] = value.clone();
                        }
                    }
                    Ok(result)
                }
                None => Err(error),
            },
            result => result,
        }
    }

    fn github_backoff_path(&self) -> PathBuf {
        self.state.join("github-backoff").join(format!(
            "{}.json",
            URL_SAFE_NO_PAD.encode(self.account.as_bytes())
        ))
    }
    fn github_check_backoff(&self) -> Result<()> {
        let path = self.github_backoff_path();
        let directory = path.parent().ok_or("missing GitHub backoff directory")?;
        match fs::symlink_metadata(directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err("GitHub backoff directory must be a real private directory".into()),
            Err(e) => return Err(e.to_string()),
        }
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
            Ok(_) => {}
        }
        let now = crate::outbox::now()?;
        let saved: Backoff = serde_json::from_slice(&github_private(&path, 4096)?)
            .map_err(|_| "invalid private GitHub backoff record")?;
        saved.validate(&self.scope(Platform::Github), now)?;
        if now < saved.retry_at {
            let mut result = saved.response(now, true);
            result["backoff_saved"] = json!(true);
            Err(task_error(result))
        } else {
            Ok(())
        }
    }
    fn github_save_backoff(&self, record: &Backoff) -> Result<()> {
        record.validate(&self.scope(Platform::Github), crate::outbox::now()?)?;
        let path = self.github_backoff_path();
        let directory = path.parent().ok_or("missing GitHub backoff directory")?;
        private_dir(directory)?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let temporary = directory.join(format!(".backoff-{}-{nonce}.tmp", std::process::id()));
        let result = (|| {
            save_private_new(
                &temporary,
                &serde_json::to_value(record).map_err(|e| e.to_string())?,
            )?;
            fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
            File::open(directory)
                .and_then(|file| file.sync_all())
                .map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    fn github_sdk_error(&self, error: platform_github::Error) -> String {
        match error {
            platform_github::Error::RateLimited {
                retry_after_seconds,
                reset_at,
            } => {
                let now = match crate::outbox::now() {
                    Ok(now) => now,
                    Err(_) => {
                        return task_error(
                            json!({"status":"rate_limited","scope":self.scope(Platform::Github),"retry_at":reset_at,"reset_at":reset_at,"retry_after_seconds":retry_after_seconds,"network_retry_started":false,"backoff_saved":false,"detail":"Local clock is unavailable; no automatic retry was started."}),
                        );
                    }
                };
                let record = Backoff::new(
                    self.scope(Platform::Github),
                    now,
                    retry_after_seconds,
                    reset_at,
                );
                let mut response = record.response(now, false);
                match self.github_save_backoff(&record) {
                    Ok(()) => response["backoff_saved"] = json!(true),
                    Err(error) => {
                        response["backoff_saved"] = json!(false);
                        response["backoff_error"] = json!(error);
                    }
                }
                task_error(response)
            }
            platform_github::Error::LoginRequired => task_error(
                json!({"status":"needs_auth","scope":self.scope(Platform::Github),"network_retry_started":false,"detail":"GitHub requires a valid saved token for this account. No login, account fallback or retry was started."}),
            ),
            platform_github::Error::PermissionDenied => task_error(
                json!({"status":"permission_denied","scope":self.scope(Platform::Github),"network_retry_started":false,"detail":"The selected GitHub account does not have permission for this action. No account fallback or retry was started."}),
            ),
            error => error.to_string(),
        }
    }

    fn github_status_task(&self, params: &Value) -> Result<Value> {
        allowed(params, &["account"])?;
        let alias = match params.get("account") {
            None => self.account.as_str(),
            Some(Value::String(v)) => v,
            _ => return Err("account must be an alias string".into()),
        };
        let scope = Scope {
            platform: Platform::Github,
            account: alias.into(),
        };
        scope.validate()?;
        let Some(path) = super::auth::saved_session(&self.state, "github", alias)? else {
            return Ok(json!({"status":"signed_out","scope":scope,"online_checked":false}));
        };
        let session: Session = serde_json::from_slice(&github_private(&path, 8192)?)
            .map_err(|_| "invalid private GitHub session")?;
        session.validate()?;
        Ok(
            json!({"status":"saved_token","scope":scope,"user":{"id":session.id,"login":session.login},"online_checked":false,"token_validity":"not_checked"}),
        )
    }

    fn github_open_task(&mut self, params: &Value) -> Result<Value> {
        allowed(params, &["reference", "target_ref", "account"])?;
        if params.get("reference").is_some() && params.get("target_ref").is_some() {
            return Err("provide reference or target_ref, not both".into());
        }
        let reference = params
            .get("target_ref")
            .or_else(|| params.get("reference"))
            .and_then(Value::as_str)
            .ok_or("missing GitHub reference")?;
        if params.get("target_ref").is_some() || opaque(reference) {
            let (scope, target) = self.load_github_target(reference, params)?;
            return self.with_task_scope(&scope, |rt| rt.github_open_target(&target, reference));
        }
        let parsed = parse_reference(reference)?;
        self.with_platform_account("github", params, false, |rt| match parsed {
            Parsed::Target(target) => { let id = rt.save_github_target(&target)?; rt.github_open_target(&target, &id) }
            Parsed::AmbiguousFile { owner, repo } => {
                let id = rt.save_github_target(&Target::Repository { owner, repo })?;
                Ok(json!({"status":"unsupported","scope":rt.scope(Platform::Github),"target_ref":id,"content_observed":false,"detail":"This blob/tree URL does not contain a complete commit SHA. Branch names can contain slashes, so Runtime did not guess the ref/path split. Use github_list kind=files on this repository with an explicit revision/path to obtain a frozen file target_ref."}))
            }
        })
    }

    fn github_body(&self, item: &mut Value, source: &Value) -> Result<()> {
        let Some(body) = source.get("body").and_then(Value::as_str) else {
            return Ok(());
        };
        if body.len() > MAX_BODY_BYTES {
            item["body"] = json!(body.chars().take(PREVIEW_CHARS).collect::<String>());
            item["body_truncated"] = json!(true);
            item["body_gap"] = json!("body exceeds runtime cache limit");
            return Ok(());
        }
        let preview: String = body.chars().take(PREVIEW_CHARS).collect();
        let truncated = preview.len() < body.len();
        item["body"] = json!(preview);
        item["body_format"] = json!("github_markdown");
        item["body_truncated"] = json!(truncated);
        if truncated {
            let encoded = serde_json::to_vec(&json!({"body":body,"format":"github_markdown"}))
                .map_err(|e| e.to_string())?;
            let resource = self.store.import(
                &self.scope(Platform::Github),
                "github-body",
                "application/json",
                &mut encoded.as_slice(),
            )?;
            item["body_resource"] = json!(resource.id);
            item["expand_tool"] = json!("runtime_expand");
        }
        Ok(())
    }

    fn github_open_target(&mut self, target: &Target, reference: &str) -> Result<Value> {
        target.validate()?;
        let (client, session) = self.github_client(false)?;
        let (owner, repo) = target.repository();
        let scope = self.scope(Platform::Github);
        let mut result = json!({"status":"ok","scope":scope,"target_ref":reference,"authenticated_request":session.is_some(),"media_downloaded":false});
        match target {
            Target::Repository { .. } => {
                let data = client
                    .repository(owner, repo)
                    .map_err(|e| self.github_sdk_error(e))?;
                result["kind"] = json!("repository");
                result["content"] = selected(
                    &data,
                    &[
                        "name",
                        "full_name",
                        "description",
                        "private",
                        "default_branch",
                        "html_url",
                        "language",
                        "stargazers_count",
                        "forks_count",
                        "open_issues_count",
                        "archived",
                        "disabled",
                        "pushed_at",
                        "updated_at",
                        "permissions",
                    ],
                );
                result["available_lists"] = json!(["issues", "pulls", "files"]);
            }
            Target::Issue { number, pull, .. } => {
                let mut data = if *pull {
                    client.pull_request(owner, repo, *number)
                } else {
                    client.issue(owner, repo, *number)
                }
                .map_err(|e| self.github_sdk_error(e))?;
                let actual_pull = *pull || data["is_pull_request"] == true;
                if actual_pull && !*pull {
                    data = client
                        .pull_request(owner, repo, *number)
                        .map_err(|e| self.github_sdk_error(e))?;
                    result["target_ref"] = json!(self.save_github_target(&Target::Issue {
                        owner: owner.into(),
                        repo: repo.into(),
                        number: *number,
                        pull: true
                    })?);
                }
                let mut content = selected(
                    &data,
                    &[
                        "number",
                        "title",
                        "state",
                        "state_reason",
                        "html_url",
                        "created_at",
                        "updated_at",
                        "closed_at",
                        "comments",
                        "draft",
                        "merged",
                        "mergeable",
                        "mergeable_state",
                        "additions",
                        "deletions",
                        "changed_files",
                        "commits",
                        "labels",
                        "assignees",
                        "reactions",
                    ],
                );
                content["author"] = selected(&data["user"], &["id", "login"]);
                self.github_body(&mut content, &data)?;
                result["kind"] = json!(if actual_pull { "pull_request" } else { "issue" });
                result["content"] = content;
                result["can_comment"] = json!(session.is_some());
                result["available_lists"] = if actual_pull {
                    json!(["comments", "review_comments", "reviews", "changes"])
                } else {
                    json!(["comments"])
                };
                if actual_pull {
                    result["head_commit"] = data["head"]["sha"].clone();
                    result["base_commit"] = data["base"]["sha"].clone();
                }
            }
            Target::Comment {
                number, comment, ..
            } => {
                let data = client
                    .read_comment(owner, repo, *comment)
                    .map_err(|e| self.github_sdk_error(e))?;
                if data["id"].as_u64() != Some(*comment)
                    || !data["issue_url"]
                        .as_str()
                        .is_some_and(|url| issue_matches(url, owner, repo, *number))
                {
                    return Err(
                        "GitHub comment does not belong to this exact issue/repository".into(),
                    );
                }
                let mut content = selected(&data, &["id", "html_url", "created_at", "updated_at"]);
                content["author"] = selected(&data["user"], &["id", "login"]);
                self.github_body(&mut content, &data)?;
                result["kind"] = json!("issue_comment");
                result["content"] = content;
                result["reply_semantics"] =
                    json!("new comment in the same issue/PR; no nested thread");
                result["can_comment"] = json!(session.is_some());
            }
            Target::ReviewComment {
                number, comment, ..
            } => {
                let data = client
                    .read_review_comment(owner, repo, *comment)
                    .map_err(|e| self.github_sdk_error(e))?;
                if !review_belongs(&data, owner, repo, *number) {
                    return Err("Review comment belongs to another pull request".into());
                }
                result["kind"] = json!("review_comment");
                result["content"] = data;
                result["inline_reply_supported"] = json!(true);
            }
            Target::File {
                path,
                revision,
                directory,
                ..
            } => {
                if *directory {
                    result["kind"] = json!("directory");
                    result["revision"] = json!(revision);
                    result["path"] = json!(path);
                    result["list_tool"] = json!("github_list");
                    result["list_kind"] = json!("files");
                    result["content_observed"] = json!(false);
                } else {
                    let data = match client.content_file(owner, repo, path, Some(revision)) {
                        Ok(data) => data,
                        Err(
                            platform_github::Error::Limit
                            | platform_github::Error::UnsupportedContent(_),
                        ) => {
                            return Ok(
                                json!({"status":"unsupported","scope":scope,"target_ref":reference,"kind":"file","path":path,"revision":revision,"content_observed":false,"detail":"The selected entry exceeds the 256 KiB file budget or is not an ordinary file. No binary, symlink, submodule or oversized content was presented as read."}),
                            );
                        }
                        Err(error) => return Err(self.github_sdk_error(error)),
                    };
                    return self.github_file_content(target, reference, &data, session.is_some());
                }
            }
        }
        Ok(result)
    }

    fn github_file_content(
        &self,
        target: &Target,
        reference: &str,
        data: &Value,
        authenticated: bool,
    ) -> Result<Value> {
        let Target::File { path, revision, .. } = target else {
            return Err("file content lacks its frozen target".into());
        };
        let mut result = json!({"status":"unsupported","scope":self.scope(Platform::Github),"target_ref":reference,"kind":"file","path":path,"revision":revision,"content_observed":false,"authenticated_request":authenticated,"source":selected(data,&["sha","size","type"])});
        if data["type"].as_str() != Some("file") || data["encoding"].as_str() != Some("base64") {
            result["detail"] = json!(
                "Only ordinary GitHub file content with an explicit base64 payload is supported; symlinks/submodules were not followed."
            );
            return Ok(result);
        }
        if data["size"]
            .as_u64()
            .is_none_or(|n| n > MAX_BODY_BYTES as u64)
        {
            result["detail"] = json!(
                "File exceeds the runtime text limit or has no trustworthy size; content was not attached."
            );
            return Ok(result);
        }
        let encoded = data["content"]
            .as_str()
            .ok_or("GitHub file content is missing")?;
        if encoded.len() > (MAX_BODY_BYTES + 2) / 3 * 4 + MAX_BODY_BYTES / 32 {
            result["detail"] = json!("Encoded file exceeds the runtime text limit");
            return Ok(result);
        }
        let normalized: String = encoded
            .chars()
            .filter(|c| !matches!(c, '\r' | '\n'))
            .collect();
        let bytes = STANDARD
            .decode(normalized)
            .map_err(|_| "invalid GitHub file base64")?;
        if bytes.len() > MAX_BODY_BYTES || data["size"].as_u64() != Some(bytes.len() as u64) {
            return Err("GitHub file payload length differs from metadata".into());
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) if !text.contains('\0') => text,
            _ => {
                result["detail"] =
                    json!("Binary or non-UTF-8 file; no text observation was fabricated.");
                return Ok(result);
            }
        };
        let preview: String = text.chars().take(PREVIEW_CHARS).collect();
        let resource = self.store.import(
            &self.scope(Platform::Github),
            "github-file",
            "text/plain",
            &mut bytes.as_slice(),
        )?;
        result["status"] = json!("ok");
        result["content_observed"] = json!(true);
        result["content"] = json!(preview);
        result["content_truncated"] = json!(result["content"].as_str().unwrap().len() < text.len());
        result["content_resource"] = json!(resource.id);
        result["expand_tool"] = json!("runtime_expand");
        result["observation_scope"] =
            json!("text preview; expand the cached resource for other ranges");
        Ok(result)
    }

    fn github_list_task(&mut self, params: &Value) -> Result<Value> {
        allowed(
            params,
            &[
                "target_ref",
                "continuation",
                "kind",
                "path",
                "revision",
                "limit",
                "account",
            ],
        )?;
        let count = limit(params)?;
        if let Some(reference) = params.get("continuation") {
            if ["target_ref", "path", "revision"]
                .iter()
                .any(|key| params.get(*key).is_some())
            {
                return Err(
                    "continuation cannot be combined with a new target/path/revision".into(),
                );
            }
            let reference = reference
                .as_str()
                .ok_or("continuation must be an opaque reference")?;
            let scope = self.locators.scope_for(CONTINUATION, reference)?;
            if scope.platform != Platform::Github {
                return Err("continuation belongs to another platform".into());
            }
            super::tasks::check_account(params, &scope)?;
            let cursor: Cursor = self.locators.get(&scope, CONTINUATION, reference)?;
            cursor.validate()?;
            if let Some(kind) = params.get("kind") {
                if serde_json::from_value::<ListKind>(kind.clone())
                    .map_err(|_| "unknown GitHub list kind")?
                    != cursor.kind
                {
                    return Err("continuation cannot change its list kind".into());
                }
            }
            return self.with_task_scope(&scope, |rt| rt.github_list_page(cursor, count));
        }
        let kind: ListKind = serde_json::from_value(
            params
                .get("kind")
                .cloned()
                .ok_or("missing GitHub list kind")?,
        )
        .map_err(|_| "unknown GitHub list kind")?;
        if kind == ListKind::Notifications {
            if ["target_ref", "path", "revision"]
                .iter()
                .any(|key| params.get(*key).is_some())
            {
                return Err("notifications use the selected account, not a repository/path".into());
            }
            return self.with_platform_account("github", params, true, |rt| {
                rt.github_list_page(
                    Cursor {
                        kind,
                        target: None,
                        page: 1,
                        seen: VecDeque::new(),
                        pending: None,
                        changes: None,
                    },
                    count,
                )
            });
        }
        let reference = required_str(params, "target_ref")?;
        let (scope, target) = self.load_github_target(reference, params)?;
        if kind != ListKind::Files
            && (params.get("path").is_some() || params.get("revision").is_some())
        {
            return Err("path/revision are only valid for repository files".into());
        }
        match kind {
            ListKind::Comments if target.issue().is_none() => {
                return Err("comments need an issue or PR target".into());
            }
            ListKind::Changes | ListKind::ReviewComments | ListKind::Reviews
                if target.issue().is_none_or(|(_, pull)| !pull) =>
            {
                return Err("changes/review_comments/reviews need a pull request target".into());
            }
            _ => {}
        }
        self.with_task_scope(&scope, |rt| {
            let target = if kind == ListKind::Files {
                rt.github_freeze_files(&target, params)?
            } else {
                target
            };
            rt.github_list_page(
                Cursor {
                    kind,
                    target: Some(target),
                    page: 1,
                    seen: VecDeque::new(),
                    pending: None,
                    changes: None,
                },
                count,
            )
        })
    }

    fn github_freeze_files(&self, target: &Target, params: &Value) -> Result<Target> {
        let (owner, repo) = target.repository();
        if let Target::File { .. } = target {
            if params.get("path").is_some() || params.get("revision").is_some() {
                return Err("a frozen file/directory reference already binds its path and commit; use the repository reference for another path/revision".into());
            }
            return Ok(target.clone());
        }
        if !matches!(target, Target::Repository { .. }) {
            return Err("files need a repository or frozen file/directory target".into());
        }
        let path = match params.get("path") {
            None => "",
            Some(Value::String(v)) => v,
            _ => return Err("path must be text".into()),
        };
        validate_path(path)?;
        let (client, _) = self.github_client(false)?;
        let revision = match params.get("revision") {
            Some(Value::String(v))
                if !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control) =>
            {
                v.clone()
            }
            Some(_) => return Err("invalid GitHub revision".into()),
            None => client
                .repository(owner, repo)
                .map_err(|e| self.github_sdk_error(e))?["default_branch"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("repository has no readable default branch")?
                .to_owned(),
        };
        let resolved = client
            .resolve_revision(owner, repo, &revision)
            .map_err(|e| self.github_sdk_error(e))?;
        let revision = resolved["sha"]
            .as_str()
            .ok_or("resolved revision has no SHA")?
            .to_ascii_lowercase();
        validate_sha(&revision)?;
        Ok(Target::File {
            owner: owner.into(),
            repo: repo.into(),
            path: path.into(),
            revision,
            directory: true,
        })
    }

    fn github_list_page(&mut self, mut cursor: Cursor, count: usize) -> Result<Value> {
        cursor.validate()?;
        let scope = self.scope(Platform::Github);
        let (all, offset, next_page, context, cached_resource) = if let Some(pending) =
            cursor.pending.take()
        {
            let (resource, path) = self.store.resolve(&scope, &pending.resource)?;
            if resource.mime != "application/json" || resource.bytes > 2 * 1024 * 1024 {
                return Err(
                    "saved GitHub page is outside its JSON cache budget; start a new list".into(),
                );
            }
            let bytes = read_bounded(&path, 2 * 1024 * 1024)?;
            if bytes.len() as u64 != resource.bytes {
                return Err("cached GitHub page length differs from its resource metadata".into());
            }
            let all: Vec<Value> =
                serde_json::from_slice(&bytes).map_err(|_| "invalid cached GitHub page")?;
            if all.len() > 1000 || pending.offset > all.len() {
                return Err("cached GitHub page offset is invalid".into());
            }
            (
                all,
                pending.offset,
                pending.next_page,
                pending.context,
                Some(pending.resource),
            )
        } else {
            let (client, _) = self.github_client(cursor.kind == ListKind::Notifications)?;
            let (raw, next_page, mut context) = if cursor.kind == ListKind::Files {
                let Target::File {
                    owner,
                    repo,
                    path,
                    revision,
                    ..
                } = cursor
                    .target
                    .as_ref()
                    .ok_or("files lack their frozen target")?
                else {
                    return Err("files lack their frozen commit/path".into());
                };
                let data = client
                    .contents(owner, repo, path, Some(revision))
                    .map_err(|e| self.github_sdk_error(e))?;
                let (items, directory) = if data["kind"] == "directory" {
                    (
                        data["entries"]
                            .as_array()
                            .ok_or("GitHub directory has no entries")?
                            .clone(),
                        true,
                    )
                } else {
                    (vec![data.clone()], false)
                };
                (
                    items,
                    None,
                    json!({"kind":"files","path":path,"revision":revision,"directory":directory,"directory_limit":data["directory_limit"],"possibly_truncated":data["possibly_truncated"],"coverage":"one directory at one immutable commit; not a recursive repository tree"}),
                )
            } else {
                let page = if cursor.kind == ListKind::Notifications { client.notifications(cursor.page, PAGE_SIZE, true, false) } else {
                    let target = cursor.target.as_ref().ok_or("GitHub list lacks its target")?;
                    let (owner, repo) = target.repository();
                    match cursor.kind {
                        ListKind::Issues => client.issues(owner, repo, "all", cursor.page, PAGE_SIZE),
                        ListKind::Pulls => client.pulls(owner, repo, "all", cursor.page, PAGE_SIZE),
                        ListKind::Comments => client.issue_comments(owner, repo, target.issue().ok_or("comments require issue target")?.0, cursor.page, PAGE_SIZE),
                        ListKind::Reviews => client.reviews(owner, repo, target.issue().filter(|(_, pull)| *pull).ok_or("reviews require PR target")?.0, cursor.page, PAGE_SIZE),
                        ListKind::ReviewComments => client.review_comments(owner, repo, target.issue().filter(|(_, pull)| *pull).ok_or("review comments require PR target")?.0, cursor.page, PAGE_SIZE),
                        ListKind::Changes => {
                            let number = target.issue().filter(|(_, pull)| *pull).ok_or("changes require PR target")?.0;
                            let before = ChangeSnapshot::from_pull(&client.pull_request(owner, repo, number).map_err(|e| self.github_sdk_error(e))?)?;
                            if cursor.changes.as_ref().is_some_and(|saved| saved != &before) { return Err("pull request changed since this continuation; start github_list changes again; no patch from another revision was substituted".into()); }
                            let page = client.pull_files(owner, repo, number, cursor.page, PAGE_SIZE).map_err(|e| self.github_sdk_error(e))?;
                            let after = ChangeSnapshot::from_pull(&client.pull_request(owner, repo, number).map_err(|e| self.github_sdk_error(e))?)?;
                            if before != after { return Err("pull request changed during the changes read; no patch was presented; start the list again".into()); }
                            cursor.changes = Some(after);
                            Ok(page)
                        }
                        _ => return Err("invalid GitHub list route".into()),
                    }
                }.map_err(|e| self.github_sdk_error(e))?;
                if page.page != cursor.page
                    || page.per_page != PAGE_SIZE
                    || page.items.len() > PAGE_SIZE as usize
                {
                    return Err("GitHub returned inconsistent pagination".into());
                }
                let next = match (page.has_more, page.next_page) {
                    (Some(false), _) => None,
                    (_, Some(next)) if next > cursor.page && next <= 10000 => Some(next),
                    (_, Some(_)) => return Err("GitHub pagination did not advance safely".into()),
                    _ => None,
                };
                let context = json!({"kind":cursor.kind,"server_has_more":page.has_more,"pagination_warning":page.pagination_warning,"poll_interval_seconds":page.poll_interval_seconds,"marked_read":false,"pagination_consistency":"server page based; concurrent changes can create gaps","inline_review_comments_included":cursor.kind==ListKind::ReviewComments,"conversation_comments_included":cursor.kind==ListKind::Comments});
                (page.items, next, context)
            };
            if cursor.kind == ListKind::Changes {
                context["snapshot"] = json!(cursor.changes);
                context["server_file_limit"] = json!(3000);
                context["possibly_truncated"] =
                    json!(cursor.page as u64 * u64::from(PAGE_SIZE) >= 3000);
                context["patch_coverage"] = json!(
                    "GitHub patches may be absent or partial; patch_available never implies the complete file was read"
                );
            }
            // Cache the provider's bounded whitelist data, not thousands of
            // newly created refs. Only the items actually returned below are
            // projected, so locator GC cannot consume this response's early refs.
            (raw, 0, next_page, context, None)
        };
        let mut end = offset;
        let mut items = Vec::new();
        let mut duplicates = 0;
        let mut filtered = 0;
        let mut seen_bytes: usize = cursor.seen.iter().map(String::len).sum();
        while end < all.len() && items.len() < count {
            let source = &all[end];
            end += 1;
            if cursor.kind == ListKind::Issues && source["is_pull_request"] == true {
                filtered += 1;
                continue;
            }
            let key = github_item_key(&cursor.kind, source)?;
            if cursor.seen.contains(&key) {
                duplicates += 1;
                continue;
            }
            items.push(self.github_list_item(&cursor, source)?);
            seen_bytes += key.len();
            cursor.seen.push_back(key);
            while cursor.seen.len() > MAX_SEEN || seen_bytes > MAX_SEEN_BYTES {
                let removed = cursor
                    .seen
                    .pop_front()
                    .ok_or("GitHub deduplication invariant")?;
                seen_bytes -= removed.len();
            }
        }
        let mut context = context;
        context["duplicates_removed"] = json!(duplicates);
        context["pulls_filtered_from_issues"] = json!(filtered);
        context["deduplication"] = json!({"mode":"rolling_recent_keys","max_keys":MAX_SEEN,"max_key_bytes":MAX_SEEN_BYTES,"retained_keys":cursor.seen.len(),"detail":"older keys outside the retained window may appear again; this is not permanent exactly-once history"});
        let mut continuation = None;
        if end < all.len() {
            let resource = if let Some(resource) = cached_resource {
                resource
            } else {
                let encoded = serde_json::to_vec(&all).map_err(|e| e.to_string())?;
                if encoded.len() > 2 * 1024 * 1024 {
                    return Err("GitHub projected page exceeds cache budget".into());
                }
                self.store
                    .import(
                        &scope,
                        "github-list-page",
                        "application/json",
                        &mut encoded.as_slice(),
                    )?
                    .id
            };
            cursor.pending = Some(Pending {
                resource,
                offset: end,
                next_page,
                context: context.clone(),
            });
            continuation = Some(self.locators.put(&scope, CONTINUATION, &cursor)?);
        } else if let Some(next) = next_page {
            cursor.page = next;
            cursor.pending = None;
            continuation = Some(self.locators.put(&scope, CONTINUATION, &cursor)?);
        }
        let target_ref = cursor
            .target
            .as_ref()
            .map(|target| self.save_github_target(target))
            .transpose()?;
        let completeness = if cursor.kind == ListKind::Files {
            if context["possibly_truncated"] == true {
                Value::Null
            } else {
                json!(!continuation.is_some())
            }
        } else if context["possibly_truncated"] == true {
            Value::Null
        } else if continuation.is_some() {
            json!(false)
        } else if context["server_has_more"] == false {
            json!(true)
        } else {
            Value::Null
        };
        Ok(
            json!({"status":"ok","scope":scope,"target_ref":target_ref,"items":items,"continuation":continuation,"has_more":continuation.is_some(),"collection_complete":completeness,"context":context,"media_downloaded":false}),
        )
    }

    fn github_list_item(&self, cursor: &Cursor, source: &Value) -> Result<Value> {
        let mut item = match cursor.kind {
            ListKind::Issues | ListKind::Pulls => {
                let target = cursor
                    .target
                    .as_ref()
                    .ok_or("GitHub issue list lacks repository")?;
                let (owner, repo) = target.repository();
                let number = source["number"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .ok_or("GitHub issue number is missing")?;
                let pull = cursor.kind == ListKind::Pulls || source["is_pull_request"] == true;
                let mut item = selected(
                    source,
                    &[
                        "number",
                        "title",
                        "state",
                        "state_reason",
                        "comments",
                        "draft",
                        "html_url",
                        "created_at",
                        "updated_at",
                        "labels",
                        "reactions",
                    ],
                );
                item["target_ref"] = json!(self.save_github_target(&Target::Issue {
                    owner: owner.into(),
                    repo: repo.into(),
                    number,
                    pull
                })?);
                item["kind"] = json!(if pull { "pull_request" } else { "issue" });
                item
            }
            ListKind::Comments | ListKind::ReviewComments => {
                let target = cursor
                    .target
                    .as_ref()
                    .ok_or("GitHub comment list lacks target")?;
                let (owner, repo) = target.repository();
                let (number, pull) = target.issue().ok_or("GitHub comment list lacks issue")?;
                let mut item = selected(
                    source,
                    &[
                        "id",
                        "html_url",
                        "created_at",
                        "updated_at",
                        "reactions",
                        "minimized",
                        "minimized_reason",
                        "path",
                        "line",
                        "start_line",
                        "original_line",
                        "side",
                        "commit_id",
                        "in_reply_to_id",
                        "diff_hunk",
                    ],
                );
                if cursor.kind == ListKind::Comments {
                    let id = source["id"]
                        .as_u64()
                        .filter(|id| *id > 0)
                        .ok_or("GitHub comment identity is missing")?;
                    if !source["issue_url"]
                        .as_str()
                        .is_some_and(|url| issue_matches(url, owner, repo, number))
                    {
                        return Err(
                            "GitHub comment page contains a comment from another issue/repository"
                                .into(),
                        );
                    }
                    item["target_ref"] = json!(self.save_github_target(&Target::Comment {
                        owner: owner.into(),
                        repo: repo.into(),
                        number,
                        pull,
                        comment: id
                    })?);
                    item["reply_semantics"] =
                        json!("new comment in same issue/PR; no nested thread");
                } else {
                    item["issue_target_ref"] = json!(self.save_github_target(&Target::Issue {
                        owner: owner.into(),
                        repo: repo.into(),
                        number,
                        pull
                    })?);
                    let comment = source["id"]
                        .as_u64()
                        .filter(|n| *n > 0)
                        .ok_or("GitHub review comment has no ID")?;
                    item["target_ref"] =
                        json!(self.save_github_target(&Target::ReviewComment {
                            owner: owner.into(),
                            repo: repo.into(),
                            number,
                            comment
                        })?);
                    item["inline_reply_supported"] = json!(true);
                    item["diff_hunk_is_complete_file"] = json!(false);
                }
                item
            }
            ListKind::Reviews => {
                let target = cursor.target.as_ref().ok_or("review list lacks PR")?;
                let mut item = selected(
                    source,
                    &["id", "html_url", "state", "commit_id", "submitted_at"],
                );
                item["kind"] = json!("review");
                item["pull_target_ref"] = json!(self.save_github_target(target)?);
                item
            }
            ListKind::Changes => self.github_change_item(cursor, source)?,
            ListKind::Files => {
                let Target::File {
                    owner,
                    repo,
                    revision,
                    ..
                } = cursor
                    .target
                    .as_ref()
                    .ok_or("directory lacks frozen target")?
                else {
                    return Err("directory lacks frozen commit".into());
                };
                let path = source["path"]
                    .as_str()
                    .ok_or("directory entry lacks path")?;
                validate_path(path)?;
                let mut item = selected(source, &["name", "path", "sha", "size", "type"]);
                let kind = source["type"]
                    .as_str()
                    .ok_or("directory entry lacks type")?;
                let ordinary =
                    matches!(kind, "file" | "dir") && source.get("submodule_git_url").is_none();
                item["read_supported"] = json!(
                    ordinary
                        && (kind == "dir"
                            || source["size"]
                                .as_u64()
                                .is_some_and(|n| n <= MAX_BODY_BYTES as u64))
                );
                if ordinary && kind == "file" && item["read_supported"] == false {
                    item["gap"] = json!(
                        "file exceeds the 256 KiB text reading budget or has no trustworthy size"
                    );
                }
                if ordinary {
                    item["target_ref"] = json!(self.save_github_target(&Target::File {
                        owner: owner.into(),
                        repo: repo.into(),
                        path: path.into(),
                        revision: revision.into(),
                        directory: kind == "dir"
                    })?);
                } else {
                    item["gap"] = json!("symlinks/submodules are not followed");
                }
                item["revision"] = json!(revision);
                item
            }
            ListKind::Notifications => {
                let mut item = selected(
                    source,
                    &["id", "unread", "reason", "updated_at", "last_read_at"],
                );
                item["subject"] = selected(&source["subject"], &["title", "type"]);
                item["repository"] = selected(
                    &source["repository"],
                    &["full_name", "private", "description"],
                );
                if let Some(target) = notification_target(source)? {
                    item["target_ref"] = json!(self.save_github_target(&target)?);
                } else {
                    item["open_supported"] = json!(false);
                    item["gap"] =
                        json!("notification subject is not a supported issue/PR reference");
                }
                item
            }
        };
        if source.get("user").is_some() {
            item["author"] = selected(&source["user"], &["id", "login"]);
        }
        self.github_body(&mut item, source)?;
        Ok(item)
    }

    fn github_change_item(&self, cursor: &Cursor, source: &Value) -> Result<Value> {
        let snapshot = cursor
            .changes
            .as_ref()
            .ok_or("PR changes lack their commit snapshot")?;
        let mut item = selected(
            source,
            &[
                "filename",
                "previous_filename",
                "status",
                "additions",
                "deletions",
                "changes",
                "sha",
                "patch_available",
            ],
        );
        item["patch_complete_file"] = json!(false);
        if let Some(patch) = source["patch"].as_str() {
            let mut projected = json!({});
            self.github_body(&mut projected, &json!({"body":patch}))?;
            item["patch"] = projected["body"].take();
            item["patch_truncated"] = projected["body_truncated"].take();
            if let Some(resource) = projected.get("body_resource") {
                item["patch_resource"] = resource.clone();
                item["expand_tool"] = json!("runtime_expand");
            }
            if let Some(gap) = projected.get("body_gap") {
                item["patch_gap"] = gap.clone();
            }
        } else {
            item["patch_available"] = json!(false);
            item["patch_gap"] = json!(
                "GitHub omitted the patch; binary/large/generated changes cannot be inferred from an empty patch"
            );
        }
        let path = source["filename"]
            .as_str()
            .ok_or("changed file lacks path")?;
        validate_path(path)?;
        if source["status"] == "removed" {
            // PR diffs compare against the merge base, not the current base tip.
            // Until that exact old snapshot is resolved, offering a base-tip
            // file_ref could silently show unrelated content at the same path.
            item["old_file_snapshot_supported"] = json!(false);
            item["file_gap"] = json!(
                "Removed file old snapshot is unsupported: the PR merge-base/blob was not resolved. The current base tip was not substituted."
            );
            return Ok(item);
        }
        let (repository, revision) = (&snapshot.head_repository, &snapshot.head_sha);
        if let Some(repository) = repository {
            item["file_ref"] = json!(self.save_github_target(&Target::File {
                owner: repository.owner.clone(),
                repo: repository.repo.clone(),
                path: path.into(),
                revision: revision.clone(),
                directory: false
            })?);
            item["file_revision"] = json!(revision);
            item["open_tool"] = json!("github_open");
        } else {
            item["file_gap"] = json!(
                "the PR source repository is unavailable; no substitute repository was selected"
            );
        }
        Ok(item)
    }

    fn github_comment_task(&mut self, params: &Value) -> Result<Value> {
        allowed(
            params,
            &["target_ref", "message", "operation_id", "account"],
        )?;
        let reference = required_str(params, "target_ref")?;
        let message = required_str(params, "message")?;
        if message.is_empty() || message.chars().count() > 1000 || message.contains('\0') {
            return Err("GitHub comment must contain 1..1000 characters".into());
        }
        let id = required_str(params, "operation_id")?;
        let (scope, target) = self.load_github_target(reference, params)?;
        if matches!(target, Target::ReviewComment { .. }) {
            return Err("Use github_action review_reply for an inline review comment".into());
        }
        let Some((number, _)) = target.issue() else {
            return Ok(
                json!({"status":"unsupported","scope":scope,"target_ref":reference,"operation_id":id,"request_sent":false,"detail":"GitHub comments need an issue/PR or issue-comment target; no repository/file action was substituted."}),
            );
        };
        if scope.account == "public" {
            return Ok(
                json!({"status":"unsupported","scope":scope,"target_ref":reference,"operation_id":id,"request_sent":false,"detail":"Reopen the issue/PR under a saved personal account before commenting; public references are never silently rebound."}),
            );
        }
        self.with_task_scope(&scope, |rt| {
            rt.github_comment_target(&target, reference, number, message, id)
        })
    }

    fn github_comment_target(
        &mut self,
        target: &Target,
        reference: &str,
        number: u64,
        message: &str,
        id: &str,
    ) -> Result<Value> {
        let scope = self.scope(Platform::Github);
        let outbox = Outbox::open(&self.state)?;
        // This path deliberately precedes session loading, Client construction
        // and every HTTP request. Replaying an ambiguous write never resubmits.
        let existing = if outbox.exists(id)? {
            let mut record = outbox.load(id, &scope)?;
            check_comment_record(&record, target, message)?;
            if record.status == Status::Attempting {
                record.status = Status::Unknown;
                record.error = Some("Runtime restarted or lost the result after recording the one attempt; no automatic retry was made".into());
                outbox.finish(&record)?;
            }
            if record.status != Status::Prepared {
                return Ok(comment_outcome(&record, reference, true));
            }
            Some(record)
        } else {
            None
        };
        let (client, session) = self.github_client(true)?;
        let session = session.ok_or("GitHub comment needs a saved personal account")?;
        let user = client
            .current_user()
            .map_err(|e| self.github_sdk_error(e))?;
        if user["id"].as_u64() != Some(session.id) {
            return Err(
                "saved GitHub token now identifies a different actor; no comment was attempted"
                    .into(),
            );
        }
        let login = user["login"]
            .as_str()
            .filter(|s| valid_login(s))
            .ok_or("GitHub actor login is invalid")?;
        let (owner, repo) = target.repository();
        let mut record = if let Some(record) = existing {
            if record.actor_id != session.id.to_string() {
                return Err(
                    "prepared GitHub operation belongs to another actor; no comment was attempted"
                        .into(),
                );
            }
            record
        } else {
            let mut record = Operation::new(
                id,
                scope,
                session.id.to_string(),
                serde_json::to_value(target).map_err(|e| e.to_string())?,
                message,
            )?;
            record.request = Some(
                json!({"action":"github_issue_comment","owner":owner,"repo":repo,"issue":number,"actor_id":session.id,"actor_login":login,"reply_semantics":"new_issue_comment_no_nested_thread"}),
            );
            outbox.insert(&record)?;
            record
        };
        check_comment_record(&record, target, message)?;
        outbox.begin_attempt(&mut record)?;
        match client.post_issue_comment(owner, repo, number, message) {
            Err(error) => {
                record.status = if matches!(
                    error,
                    platform_github::Error::WriteResultUnknown | platform_github::Error::Network
                ) {
                    Status::Unknown
                } else {
                    Status::Rejected
                };
                let error = self.github_sdk_error(error);
                if let Some(failure) = decode_task_error(&error) {
                    record.error=Some("GitHub rejected the single comment attempt; see the structured provider error".into());
                    record.verification = Some(json!({"verified":false,"provider_error":failure}));
                } else {
                    record.error = Some(error);
                }
                outbox.finish(&record)?;
                return Ok(comment_outcome(&record, reference, false));
            }
            Ok(receipt) => {
                let Some(comment_id) = receipt["id"].as_u64().filter(|id| *id > 0) else {
                    record.status = Status::Unknown;
                    record.error = Some("GitHub accepted a response without a usable exact comment ID; no retry was made".into());
                    outbox.finish(&record)?;
                    return Ok(comment_outcome(&record, reference, false));
                };
                let mut compact =
                    selected(&receipt, &["id", "html_url", "created_at", "updated_at"]);
                compact["author"] = selected(&receipt["user"], &["id", "login"]);
                record.receipt = Some(compact);
                record.status = Status::Sent;
                outbox.finish(&record)?;
                // A receipt ID is only Sent. Verify through the exact read path,
                // never by searching for another comment with equal text.
                match client.read_comment(owner, repo, comment_id) {
                    Ok(readback)
                        if exact_readback(
                            &readback, comment_id, session.id, owner, repo, number, message,
                        ) =>
                    {
                        record.status = Status::Verified;
                        record.verification = Some(
                            json!({"verified":true,"method":"exact_comment_id_get","comment_id":comment_id,"actor_id":session.id,"repository":format!("{owner}/{repo}"),"issue":number,"body_exact":true}),
                        );
                    }
                    Ok(_) => {
                        record.verification = Some(
                            json!({"verified":false,"method":"exact_comment_id_get","comment_id":comment_id,"mismatch":true}),
                        );
                        record.error = Some("exact comment readback did not match ID, actor, repository/issue and body; no verification was claimed".into());
                    }
                    Err(error) => {
                        let error = self.github_sdk_error(error);
                        let mut verification = json!({"verified":false,"method":"exact_comment_id_get","comment_id":comment_id});
                        if let Some(failure) = decode_task_error(&error) {
                            verification["read_error"] = failure;
                            record.error=Some("Comment receipt saved but exact readback was blocked; see the structured read error. Do not resend.".into());
                        } else {
                            record.error = Some(format!(
                                "comment receipt saved but readback unavailable: {error}"
                            ));
                        }
                        record.verification = Some(verification);
                    }
                }
                outbox.finish(&record)?;
            }
        }
        Ok(comment_outcome(&record, reference, false))
    }
}

fn check_comment_record(record: &Operation, target: &Target, message: &str) -> Result<()> {
    let saved: Target = serde_json::from_value(record.target.clone())
        .map_err(|_| "operation_id belongs to another action target")?;
    if &saved != target || record.message != message {
        return Err(
            "operation_id already belongs to another exact GitHub target or message".into(),
        );
    }
    let request = record
        .request
        .as_ref()
        .and_then(Value::as_object)
        .ok_or("saved GitHub comment has no frozen request")?;
    if request.keys().any(|key| {
        !matches!(
            key.as_str(),
            "action" | "owner" | "repo" | "issue" | "actor_id" | "actor_login" | "reply_semantics"
        )
    }) {
        return Err("operation_id belongs to another action".into());
    }
    let (owner, repo) = target.repository();
    let number = target
        .issue()
        .ok_or("saved comment is not an issue target")?
        .0;
    if request.get("action").and_then(Value::as_str) != Some("github_issue_comment")
        || request.get("owner").and_then(Value::as_str) != Some(owner)
        || request.get("repo").and_then(Value::as_str) != Some(repo)
        || request.get("issue").and_then(Value::as_u64) != Some(number)
        || request
            .get("actor_id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .map(|id| id.to_string())
            != Some(record.actor_id.clone())
        || request
            .get("actor_login")
            .and_then(Value::as_str)
            .is_none_or(|s| !valid_login(s))
        || request.get("reply_semantics").and_then(Value::as_str)
            != Some("new_issue_comment_no_nested_thread")
    {
        return Err("saved GitHub comment request does not match its target or actor".into());
    }
    Ok(())
}

fn exact_readback(
    data: &Value,
    id: u64,
    actor: u64,
    owner: &str,
    repo: &str,
    number: u64,
    message: &str,
) -> bool {
    data["id"].as_u64() == Some(id)
        && data["user"]["id"].as_u64() == Some(actor)
        && data["body"].as_str() == Some(message)
        && data["issue_url"]
            .as_str()
            .is_some_and(|url| issue_matches(url, owner, repo, number))
}

fn comment_outcome(record: &Operation, reference: &str, replayed: bool) -> Value {
    let mut outcome = json!({"status":record.status,"scope":record.scope,"target_ref":reference,"operation_id":record.id,"actor_id":record.actor_id,"receipt":record.receipt,"verification":record.verification,"error":record.error,"reused_operation":replayed,"attempt_recorded":record.attempt_started_at.is_some(),"request_sent":match record.status { Status::Prepared | Status::AlreadySatisfied=>Some(false),Status::Sent|Status::Verified|Status::Rejected=>Some(true),_=>None },"readback_verified":record.status==Status::Verified,"unknown":matches!(record.status,Status::Attempting|Status::Unknown),"reply_semantics":"new comment in same issue/PR; no nested thread"});
    if record.status == Status::Rejected {
        if let Some(failure) = record
            .verification
            .as_ref()
            .and_then(|v| v.get("provider_error"))
            .filter(|v| {
                matches!(
                    v["status"].as_str(),
                    Some("rate_limited" | "needs_auth" | "permission_denied")
                )
            })
        {
            outcome["operation_status"] = json!("rejected");
            outcome["same_operation_will_retry"] = json!(false);
            for key in [
                "status",
                "retry_at",
                "reset_at",
                "retry_after_seconds",
                "network_retry_started",
                "remaining_seconds",
            ] {
                if let Some(value) = failure.get(key) {
                    outcome[key] = value.clone();
                }
            }
            outcome["provider_error"] = failure.clone();
        }
    }
    outcome
}

fn github_item_key(kind: &ListKind, item: &Value) -> Result<String> {
    let key = match kind {
        ListKind::Files => format!(
            "file:{}:{}",
            item["path"]
                .as_str()
                .ok_or("GitHub directory item lacks path")?,
            item["sha"].as_str().unwrap_or("")
        ),
        ListKind::Changes => format!(
            "change:{}:{}",
            item["filename"]
                .as_str()
                .ok_or("GitHub change lacks filename")?,
            item["sha"].as_str().unwrap_or("")
        ),
        ListKind::Notifications => format!(
            "notification:{}",
            item["id"]
                .as_str()
                .ok_or("GitHub notification lacks identity")?
        ),
        _ => format!(
            "item:{}",
            item["id"]
                .as_u64()
                .filter(|id| *id > 0)
                .ok_or("GitHub item lacks identity")?
        ),
    };
    if key.len() > 4096 {
        return Err("GitHub item identity exceeds budget".into());
    }
    Ok(key)
}

fn notification_target(source: &Value) -> Result<Option<Target>> {
    let Some(name) = source["repository"]["full_name"].as_str() else {
        return Ok(None);
    };
    let (owner, repo) = name
        .split_once('/')
        .ok_or("notification repository is invalid")?;
    validate_repository(owner, repo)?;
    let Some(url) = source["subject"]["url"].as_str() else {
        return Ok(None);
    };
    let prefix = format!("https://api.github.com/repos/{owner}/{repo}/");
    let Some(path) = url
        .get(..prefix.len())
        .filter(|s| s.eq_ignore_ascii_case(&prefix))
        .and_then(|_| url.get(prefix.len()..))
    else {
        return Ok(None);
    };
    let Some((kind, number)) = path.split_once('/') else {
        return Ok(None);
    };
    let pull = match kind {
        "issues" => false,
        "pulls" => true,
        _ => return Ok(None),
    };
    let Some(number) = number.parse::<u64>().ok().filter(|n| *n > 0) else {
        return Ok(None);
    };
    Ok(Some(Target::Issue {
        owner: owner.to_ascii_lowercase(),
        repo: repo.to_ascii_lowercase(),
        number,
        pull,
    }))
}

fn github_private(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = open_regular(path, limit)?;
    if file
        .metadata()
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err("GitHub credential/session file must be private (0600)".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("GitHub private file exceeds byte limit".into());
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;

fn review_belongs(data: &Value, owner: &str, repo: &str, number: u64) -> bool {
    data["pull_request_url"].as_str().is_some_and(|url| {
        url.eq_ignore_ascii_case(&format!(
            "https://api.github.com/repos/{owner}/{repo}/pulls/{number}"
        ))
    })
}
fn strings(params: &Value, key: &str) -> Result<Vec<String>> {
    params
        .get(key)
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| format!("{key} must be an array of strings"))
        })
        .transpose()
        .map(Option::unwrap_or_default)
}
fn collaboration_outcome(record: &Operation, reference: &str, reused: bool) -> Value {
    json!({"status":record.status,"scope":record.scope,"operation_id":record.id,"target_ref":reference,
        "receipt":record.receipt,"verification":record.verification,"error":record.error,"reused_operation":reused,
        "verified":record.status==Status::Verified,"automatic_resend":false,"request_sent":if record.attempt_started_at.is_none(){Some(false)}else if matches!(record.status,Status::Sent|Status::Verified){Some(true)}else{None}})
}
impl Runtime {
    fn github_action_task(&mut self, params: &Value) -> Result<Value> {
        allowed(
            params,
            &[
                "action",
                "target_ref",
                "operation_id",
                "account",
                "title",
                "message",
                "event",
                "comments",
                "state",
                "labels",
                "assignees",
            ],
        )?;
        let action = required_str(params, "action")?;
        if !matches!(
            action,
            "create_issue"
                | "review"
                | "review_reply"
                | "set_state"
                | "set_labels"
                | "set_assignees"
        ) {
            return Err("unknown GitHub collaboration action".into());
        }
        let reference = required_str(params, "target_ref")?;
        let id = required_str(params, "operation_id")?;
        let (scope, target) = self.load_github_target(reference, params)?;
        if scope.account == "public" {
            return Err(
                "GitHub writes require reopening the target under a saved authenticated account"
                    .into(),
            );
        }
        let mut input = params.clone();
        for key in ["operation_id", "account", "target_ref"] {
            input.as_object_mut().unwrap().remove(key);
        }
        self.with_task_scope(&scope, |rt| {
            rt.github_collaborate_target(&target, reference, id, &input)
        })
    }
    fn github_collaborate_target(
        &mut self,
        target: &Target,
        reference: &str,
        id: &str,
        input: &Value,
    ) -> Result<Value> {
        use platform_github::CollaborationAction as A;
        let action = required_str(input, "action")?;
        match (action,target) {
            ("create_issue",Target::Repository{..})=>{},
            ("review",Target::Issue{pull:true,..})=>{},
            ("review_reply",Target::ReviewComment{..})=>{},
            ("set_state"|"set_labels"|"set_assignees",Target::Issue{..})=>{},
            _=>return Err("GitHub action and exact target type do not match; no alternate action was substituted".into()),
        }
        let scope = self.scope(Platform::Github);
        let outbox = Outbox::open(&self.state)?;
        let target_json = serde_json::to_value(target).map_err(|e| e.to_string())?;
        let mut existing = None;
        if outbox.exists(id)? {
            let mut record = outbox.load(id, &scope)?;
            if record.target != target_json
                || record
                    .request
                    .as_ref()
                    .is_none_or(|r| r["action"] != "github_collaboration" || r["input"] != *input)
            {
                return Err(
                    "operation_id belongs to another exact GitHub action or payload".into(),
                );
            }
            if record.status == Status::Attempting {
                record.status = Status::Unknown;
                record.error = Some(
                    "The previous attempt may have executed; no resubmission was started".into(),
                );
                outbox.finish(&record)?;
            }
            if record.status != Status::Prepared {
                return Ok(collaboration_outcome(&record, reference, true));
            }
            existing = Some(record);
        }
        let (client, session) = self.github_client(true)?;
        let session = session.ok_or("saved GitHub account required")?;
        let actor = client
            .current_user()
            .map_err(|e| self.github_sdk_error(e))?;
        if actor["id"].as_u64() != Some(session.id) {
            return Err("GitHub authenticated actor changed; no write was attempted".into());
        }
        let (owner, repo) = target.repository();
        let number = target.issue().map(|x| x.0);
        let body = input
            .get("message")
            .map(|v| v.as_str().ok_or("message must be text"))
            .transpose()?
            .unwrap_or("")
            .to_owned();
        let command = if let Some(record) = &existing {
            if record.actor_id != session.id.to_string() {
                return Err("prepared action belongs to another actor".into());
            }
            serde_json::from_value::<A>(
                record.request.as_ref().ok_or("missing frozen action")?["command"].clone(),
            )
            .map_err(|_| "invalid frozen GitHub action")?
        } else {
            match action {
                "create_issue" => A::CreateIssue {
                    title: required_str(input, "title")?.into(),
                    body: body.clone(),
                    labels: strings(input, "labels")?,
                    assignees: strings(input, "assignees")?,
                },
                "set_state" => A::SetState {
                    state: required_str(input, "state")?.into(),
                    pull: matches!(target, Target::Issue { pull: true, .. }),
                },
                "set_labels" => A::SetLabels {
                    labels: strings(input, "labels")?,
                },
                "set_assignees" => A::SetAssignees {
                    assignees: strings(input, "assignees")?,
                },
                "review_reply" => {
                    let Target::ReviewComment { comment, .. } = target else {
                        unreachable!()
                    };
                    let source = client
                        .read_review_comment(owner, repo, *comment)
                        .map_err(|e| self.github_sdk_error(e))?;
                    if !review_belongs(&source, owner, repo, number.unwrap()) {
                        return Err("inline comment belongs to another PR".into());
                    }
                    A::ReviewReply {
                        comment_id: *comment,
                        body: body.clone(),
                    }
                }
                "review" => {
                    let pr = client
                        .pull_request(owner, repo, number.unwrap())
                        .map_err(|e| self.github_sdk_error(e))?;
                    let commit = pr
                        .pointer("/head/sha")
                        .and_then(Value::as_str)
                        .ok_or("PR has no head commit")?;
                    validate_sha(commit)?;
                    let mut lines = Vec::new();
                    if let Some(comments) = input.get("comments") {
                        let comments = comments
                            .as_array()
                            .filter(|a| a.len() <= 16)
                            .ok_or("review comments must be an array of at most16")?;
                        for line in comments {
                            allowed(
                                line,
                                &[
                                    "file_ref",
                                    "body",
                                    "line",
                                    "side",
                                    "start_line",
                                    "start_side",
                                ],
                            )?;
                            let (file_scope, file) = self
                                .load_github_target(required_str(line, "file_ref")?, &json!({}))?;
                            if file_scope != scope {
                                return Err(
                                    "review file reference belongs to another account".into()
                                );
                            }
                            let Target::File {
                                owner: fo,
                                repo: fr,
                                path,
                                revision,
                                directory: false,
                            } = file
                            else {
                                return Err(
                                    "inline review needs a returned regular file reference".into(),
                                );
                            };
                            let repo_name = pr
                                .pointer("/head/repo/full_name")
                                .and_then(Value::as_str)
                                .ok_or("PR head repository unavailable")?;
                            if !format!("{fo}/{fr}").eq_ignore_ascii_case(repo_name)
                                || revision != commit
                            {
                                return Err("review file snapshot no longer matches the PR head; reopen changes before reviewing".into());
                            }
                            lines.push(platform_github::ReviewLine {
                                path,
                                body: required_str(line, "body")?.into(),
                                line: u32::try_from(super::number(line, "line", 0)?)
                                    .map_err(|_| "invalid review line")?,
                                side: line
                                    .get("side")
                                    .and_then(Value::as_str)
                                    .unwrap_or("RIGHT")
                                    .into(),
                                start_line: line
                                    .get("start_line")
                                    .map(|_| {
                                        u32::try_from(super::number(line, "start_line", 0)?)
                                            .map_err(|_| String::from("invalid review start"))
                                    })
                                    .transpose()?,
                                start_side: line
                                    .get("start_side")
                                    .map(|v| {
                                        v.as_str().map(str::to_owned).ok_or("invalid start_side")
                                    })
                                    .transpose()?,
                            });
                        }
                    }
                    A::Review {
                        event: input
                            .get("event")
                            .and_then(Value::as_str)
                            .unwrap_or("COMMENT")
                            .into(),
                        body: body.clone(),
                        commit_id: commit.into(),
                        comments: lines,
                    }
                }
                _ => unreachable!(),
            }
        };
        command.validate().map_err(|e| e.to_string())?;
        if let A::Review { commit_id, .. } = &command {
            let pr = client
                .pull_request(owner, repo, number.unwrap())
                .map_err(|e| self.github_sdk_error(e))?;
            if pr.pointer("/head/sha").and_then(Value::as_str) != Some(commit_id) {
                return Err("PR head changed before review submission; nothing was sent".into());
            }
        }
        let mut record = if let Some(record) = existing {
            record
        } else {
            let mut record =
                Operation::new(id, scope, session.id.to_string(), target_json, action)?;
            record.request =
                Some(json!({"action":"github_collaboration","input":input,"command":command}));
            outbox.insert(&record)?;
            record
        };
        outbox.begin_attempt(&mut record)?;
        let receipt = match client.collaborate(owner, repo, number, &command) {
            Ok(value) => value,
            Err(error) => {
                record.status = if matches!(
                    error,
                    platform_github::Error::WriteResultUnknown | platform_github::Error::Network
                ) {
                    Status::Unknown
                } else {
                    Status::Rejected
                };
                record.error = Some(self.github_sdk_error(error));
                outbox.finish(&record)?;
                return Ok(collaboration_outcome(&record, reference, false));
            }
        };
        record.receipt = Some(selected(
            &receipt,
            &[
                "id",
                "number",
                "html_url",
                "state",
                "commit_id",
                "submitted_at",
            ],
        ));
        record.status = Status::Sent;
        outbox.finish(&record)?;
        let readback = match &command {
            A::CreateIssue { .. } => receipt["number"]
                .as_u64()
                .ok_or(platform_github::Error::Schema("created issue number"))
                .and_then(|n| client.issue(owner, repo, n)),
            A::Review { .. } => receipt["id"]
                .as_u64()
                .ok_or(platform_github::Error::Schema("review id"))
                .and_then(|n| client.read_review(owner, repo, number.unwrap(), n)),
            A::ReviewReply { .. } => receipt["id"]
                .as_u64()
                .ok_or(platform_github::Error::Schema("review comment id"))
                .and_then(|n| client.read_review_comment(owner, repo, n)),
            A::SetState { pull: true, .. } => client.pull_request(owner, repo, number.unwrap()),
            _ => client.issue(owner, repo, number.unwrap()),
        };
        match readback {
            Ok(data) => {
                let verified = collaboration_matches(
                    &command, &data, &receipt, session.id, owner, repo, number,
                );
                let inline_verified = match &command {
                    A::Review {
                        comments,
                        commit_id,
                        ..
                    } if !comments.is_empty() => {
                        let review = receipt["id"]
                            .as_u64()
                            .ok_or("missing acknowledged review ID")?;
                        match client.read_review_comments(owner, repo, number.unwrap(), review) {
                            Ok(page) => inline_comments_match(
                                comments,
                                &page.items,
                                session.id,
                                review,
                                commit_id,
                                owner,
                                repo,
                                number.unwrap(),
                            ),
                            Err(_) => false,
                        }
                    }
                    _ => true,
                };
                let verified = verified && inline_verified;
                record.verification = Some(
                    json!({"verified":verified,"exact_readback":true,"action":action,"inline_comments_verified":inline_verified}),
                );
                if verified {
                    record.status = Status::Verified;
                }
                if let A::CreateIssue { .. } = command {
                    if let Some(n) = data["number"].as_u64() {
                        record.receipt.as_mut().unwrap()["target_ref"] =
                            json!(self.save_github_target(&Target::Issue {
                                owner: owner.into(),
                                repo: repo.into(),
                                number: n,
                                pull: false
                            })?);
                    }
                }
            }
            Err(error) => {
                record.error = Some(format!(
                    "write acknowledged; exact readback unavailable: {}",
                    self.github_sdk_error(error)
                ))
            }
        }
        outbox.finish(&record)?;
        Ok(collaboration_outcome(&record, reference, false))
    }
}
fn collaboration_matches(
    action: &platform_github::CollaborationAction,
    data: &Value,
    receipt: &Value,
    actor: u64,
    owner: &str,
    repo: &str,
    number: Option<u64>,
) -> bool {
    use platform_github::CollaborationAction as A;
    let same_id = receipt["id"].as_u64().is_some() && receipt["id"] == data["id"];
    let authored = data["user"]["id"].as_u64() == Some(actor);
    let names = |v: &Value, key: &str, field: &str| {
        let mut a = v[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x[field].as_str().map(|s| s.to_ascii_lowercase()))
            .collect::<Vec<_>>();
        a.sort();
        a
    };
    let wanted = |v: &[String]| {
        let mut a = v.iter().map(|s| s.to_ascii_lowercase()).collect::<Vec<_>>();
        a.sort();
        a
    };
    match action {
        A::CreateIssue {
            title,
            body,
            labels,
            assignees,
        } => {
            same_id
                && authored
                && data["title"] == *title
                && data["body"] == *body
                && names(data, "labels", "name") == wanted(labels)
                && names(data, "assignees", "login") == wanted(assignees)
        }
        A::Review {
            event,
            body,
            commit_id,
            ..
        } => {
            same_id
                && authored
                && data["body"] == *body
                && data["commit_id"] == *commit_id
                && data["state"]
                    == match event.as_str() {
                        "APPROVE" => "APPROVED",
                        "REQUEST_CHANGES" => "CHANGES_REQUESTED",
                        _ => "COMMENTED",
                    }
        }
        A::ReviewReply { comment_id, body } => {
            same_id
                && authored
                && data["body"] == *body
                && data["in_reply_to_id"].as_u64() == Some(*comment_id)
                && review_belongs(data, owner, repo, number.unwrap_or(0))
        }
        A::SetState { state, .. } => data["number"].as_u64() == number && data["state"] == *state,
        A::SetLabels { labels } => {
            data["number"].as_u64() == number && names(data, "labels", "name") == wanted(labels)
        }
        A::SetAssignees { assignees } => {
            data["number"].as_u64() == number
                && names(data, "assignees", "login") == wanted(assignees)
        }
    }
}

fn inline_comments_match(
    expected: &[platform_github::ReviewLine],
    actual: &[Value],
    actor: u64,
    review: u64,
    commit: &str,
    owner: &str,
    repo: &str,
    number: u64,
) -> bool {
    let mut used = BTreeSet::new();
    expected.iter().all(|line| {
        actual.iter().any(|r| {
            let Some(id) = r["id"].as_u64().filter(|id| *id > 0) else {
                return false;
            };
            if used.contains(&id) {
                return false;
            }
            let matches = r["user"]["id"].as_u64() == Some(actor)
                && r["pull_request_review_id"].as_u64() == Some(review)
                && r["body"] == line.body
                && r["path"] == line.path
                && r["commit_id"] == commit
                && r["line"].as_u64() == Some(u64::from(line.line))
                && r["side"] == line.side
                && r["start_line"].as_u64() == line.start_line.map(u64::from)
                && r["start_side"].as_str() == line.start_side.as_deref()
                && review_belongs(r, owner, repo, number);
            if matches {
                used.insert(id);
            }
            matches
        })
    })
}
