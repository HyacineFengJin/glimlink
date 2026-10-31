// SPDX-License-Identifier: MIT
//! Bounded GitHub REST adapter. No browser, arbitrary endpoints, automatic write
//! retry, Link following, account discovery, or durable operation policy.
//! Response strings (including Markdown and URLs) remain untrusted platform data.
mod actions;
mod model;
pub use actions::{CollaborationAction, ReviewLine};
mod pagination;
mod transport;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;
pub use transport::Config;

pub const API_VERSION: &str = "2026-03-10";
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    InvalidPath,
    InvalidPage,
    InvalidId,
    LoginRequired,
    PermissionDenied,
    NotFound,
    RateLimited {
        retry_after_seconds: Option<u64>,
        reset_at: Option<u64>,
    },
    NotModified,
    ValidationFailed,
    RedirectDenied,
    HttpStatus(u16),
    Network,
    Limit,
    Schema(&'static str),
    UnsupportedContent(&'static str),
    /// The one POST may have been accepted. Caller must reconcile, never retry blindly.
    WriteResultUnknown,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => {
                f.write_str("GitHub resource not found or not visible to this account")
            }
            Self::LoginRequired => f.write_str("GitHub authentication required or token invalid"),
            Self::PermissionDenied => f.write_str("GitHub permission denied for this resource"),
            Self::Schema(reason) => write!(f, "unexpected GitHub response: {reason}"),
            Self::UnsupportedContent(reason) => write!(f, "unsupported GitHub content: {reason}"),
            Self::HttpStatus(status) => write!(f, "GitHub HTTP status {status}"),
            _ => write!(f, "GitHub adapter: {self:?}"),
        }
    }
}
impl std::error::Error for Error {}

/// `None` has_more means the current page is valid but pagination could not be
/// established. It must not be presented as the end of the collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<Value>,
    pub has_more: Option<bool>,
    pub next_page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagination_warning: Option<String>,
    pub page: u32,
    pub per_page: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_interval_seconds: Option<u64>,
}

pub struct Client {
    transport: transport::Transport,
}
impl Client {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            transport: transport::Transport::new(config)?,
        })
    }
    pub fn has_token(&self) -> bool {
        self.transport.has_token()
    }
    pub fn current_user(&self) -> Result<Value> {
        self.transport.require_token()?;
        model::user(&self.get("/user", &[])?, true)
    }
    pub fn user(&self, login: &str) -> Result<Value> {
        component(login, false)?;
        let value = model::user(&self.get(&format!("/users/{}", encode(login)), &[])?, true)?;
        if value["login"]
            .as_str()
            .is_none_or(|v| !v.eq_ignore_ascii_case(login))
        {
            return Err(Error::Schema("user identity"));
        }
        Ok(value)
    }
    pub fn repository(&self, owner: &str, repo: &str) -> Result<Value> {
        let path = repo_path(owner, repo)?;
        let value = model::repository(&self.get(&path, &[])?)?;
        let expected = format!("{owner}/{repo}");
        if value["full_name"]
            .as_str()
            .is_none_or(|s| !s.eq_ignore_ascii_case(&expected))
        {
            return Err(Error::Schema("repository identity"));
        }
        Ok(value)
    }
    pub fn issue(&self, owner: &str, repo: &str, number: u64) -> Result<Value> {
        valid_id(number)?;
        let value = model::issue(
            &self.get(&format!("{}/issues/{number}", repo_path(owner, repo)?), &[])?,
            false,
        )?;
        match_number(&value, number)?;
        Ok(value)
    }
    pub fn pull_request(&self, owner: &str, repo: &str, number: u64) -> Result<Value> {
        valid_id(number)?;
        let value = model::issue(
            &self.get(&format!("{}/pulls/{number}", repo_path(owner, repo)?), &[])?,
            true,
        )?;
        match_number(&value, number)?;
        Ok(value)
    }
    /// Freeze a branch/tag/ref to an immutable commit SHA before reading contents.
    pub fn resolve_revision(&self, owner: &str, repo: &str, revision: &str) -> Result<Value> {
        valid_ref(Some(revision))?;
        let raw = self.transport.get_bytes(
            &format!("{}/commits/{}", repo_path(owner, repo)?, encode(revision)),
            &[],
            "application/vnd.github.sha",
            128,
        )?;
        let sha = std::str::from_utf8(&raw)
            .map_err(|_| Error::Schema("commit sha"))?
            .trim();
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Schema("commit sha"));
        }
        Ok(json!({"sha":sha.to_ascii_lowercase()}))
    }
    pub fn issues(
        &self,
        owner: &str,
        repo: &str,
        state: &str,
        page: u32,
        per_page: u32,
    ) -> Result<Page> {
        valid_state(state)?;
        self.list(
            &format!("{}/issues", repo_path(owner, repo)?),
            &[("state", state.to_owned())],
            page,
            per_page,
            100,
            |v| model::issue(v, false),
        )
    }
    pub fn pulls(
        &self,
        owner: &str,
        repo: &str,
        state: &str,
        page: u32,
        per_page: u32,
    ) -> Result<Page> {
        valid_state(state)?;
        self.list(
            &format!("{}/pulls", repo_path(owner, repo)?),
            &[("state", state.to_owned())],
            page,
            per_page,
            100,
            |v| model::issue(v, true),
        )
    }
    pub fn issue_comments(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        page: u32,
        per_page: u32,
    ) -> Result<Page> {
        valid_id(number)?;
        self.list(
            &format!("{}/issues/{number}/comments", repo_path(owner, repo)?),
            &[],
            page,
            per_page,
            100,
            |v| model::comment(v, false),
        )
    }
    pub fn review_comments(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        page: u32,
        per_page: u32,
    ) -> Result<Page> {
        valid_id(number)?;
        self.list(
            &format!("{}/pulls/{number}/comments", repo_path(owner, repo)?),
            &[],
            page,
            per_page,
            100,
            |v| model::comment(v, true),
        )
    }
    pub fn pull_files(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        page: u32,
        per_page: u32,
    ) -> Result<Page> {
        valid_id(number)?;
        let mut result = self.list(
            &format!("{}/pulls/{number}/files", repo_path(owner, repo)?),
            &[],
            page,
            per_page,
            100,
            model::pull_file,
        )?;
        if result.next_page.is_none()
            && u64::from(page - 1) * u64::from(per_page) + result.items.len() as u64 >= 3000
        {
            result.has_more = None;
            result.pagination_warning =
                Some("pagination_unavailable: GitHub limits PR file listings to 3000 files".into());
        }
        Ok(result)
    }
    /// Notification reads do not mark threads read. GitHub currently supports
    /// classic PATs, not fine-grained tokens, for this endpoint.
    pub fn notifications(
        &self,
        page: u32,
        per_page: u32,
        all: bool,
        participating: bool,
    ) -> Result<Page> {
        self.transport.require_token()?;
        self.list(
            "/notifications",
            &[
                ("all", all.to_string()),
                ("participating", participating.to_string()),
            ],
            page,
            per_page,
            50,
            model::notification,
        )
    }
    /// Directory entries or file metadata. Inline content is deliberately not
    /// returned; use content_file to read at most 1 MiB of actual file bytes.
    pub fn contents(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        git_ref: Option<&str>,
    ) -> Result<Value> {
        let api_path = contents_path(owner, repo, path)?;
        let query = ref_query(git_ref)?;
        match self
            .transport
            .get_json(&api_path, &query, "application/vnd.github.object+json")
        {
            Ok(raw) => model::contents(&raw.value, path, git_ref, self.transport.max_file_bytes()),
            // Inline Base64 can exceed the JSON wire cap even when raw file
            // bytes fit the file cap. A bounded parent directory supplies file
            // metadata without increasing the wire cap or guessing MIME types.
            Err(Error::Limit) if !path.is_empty() => {
                let parent = path
                    .rsplit_once('/')
                    .map(|(parent, _)| parent)
                    .unwrap_or("");
                let raw = self.transport.get_json(
                    &contents_path(owner, repo, parent)?,
                    &query,
                    "application/vnd.github.object+json",
                )?;
                let directory =
                    model::contents(&raw.value, parent, git_ref, self.transport.max_file_bytes())?;
                let entry = directory["entries"]
                    .as_array()
                    .ok_or(Error::Schema("parent directory"))?
                    .iter()
                    .find(|v| v["path"].as_str() == Some(path))
                    .ok_or(Error::UnsupportedContent(
                        "bounded parent directory did not expose file metadata",
                    ))?;
                if entry["type"] != "file" {
                    return Err(Error::Limit);
                }
                model::contents(entry, path, git_ref, self.transport.max_file_bytes())
            }
            Err(error) => Err(error),
        }
    }
    /// Both metadata and raw requests use the same fixed API path and revision.
    /// Callers should pass a frozen commit SHA for cross-call consistency.
    pub fn content_file(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        git_ref: Option<&str>,
    ) -> Result<Value> {
        if path.is_empty() {
            return Err(Error::UnsupportedContent("directory is not a file"));
        }
        let metadata = self.contents(owner, repo, path, git_ref)?;
        model::file_size(&metadata, self.transport.max_file_bytes())?;
        let raw = self
            .transport
            .get_raw(&contents_path(owner, repo, path)?, &ref_query(git_ref)?)?;
        model::file_with_content(metadata, &raw, self.transport.max_file_bytes())
    }
    /// The adapter sends exactly one POST, never follows redirects, and performs
    /// no automatic reconciliation or retry. Durable identity belongs to Runtime.
    pub fn post_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<Value> {
        self.transport.require_token()?;
        valid_id(number)?;
        if body.is_empty() || body.len() > 64 * 1024 || body.contains('\0') {
            return Err(Error::ValidationFailed);
        }
        let path = format!("{}/issues/{number}/comments", repo_path(owner, repo)?);
        let raw = self.transport.post_json(&path, &json!({"body":body}))?;
        let value = model::comment(&raw, false).map_err(|_| Error::WriteResultUnknown)?;
        if comment_issue_number(&value, owner, repo).ok() != Some(number)
            || value["body"].as_str() != Some(body)
        {
            return Err(Error::WriteResultUnknown);
        }
        Ok(value)
    }
    pub fn read_comment(&self, owner: &str, repo: &str, id: u64) -> Result<Value> {
        valid_id(id)?;
        let raw = self.get(
            &format!("{}/issues/comments/{id}", repo_path(owner, repo)?),
            &[],
        )?;
        let value = model::comment(&raw, false)?;
        if value["id"].as_u64() != Some(id) {
            return Err(Error::Schema("comment identity"));
        }
        comment_issue_number(&value, owner, repo)?;
        Ok(value)
    }
    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        Ok(self
            .transport
            .get_json(path, query, "application/vnd.github+json")?
            .value)
    }
    fn list(
        &self,
        path: &str,
        query: &[(&str, String)],
        page: u32,
        per_page: u32,
        max_page_size: u32,
        project: impl Fn(&Value) -> Result<Value>,
    ) -> Result<Page> {
        if !(1..=pagination::MAX_PAGE).contains(&page) || !(1..=max_page_size).contains(&per_page) {
            return Err(Error::InvalidPage);
        }
        let mut query = query.to_vec();
        query.push(("page", page.to_string()));
        query.push(("per_page", per_page.to_string()));
        let raw = self
            .transport
            .get_json(path, &query, "application/vnd.github+json")?;
        let array = raw
            .value
            .as_array()
            .ok_or(Error::Schema("page is not an array"))?;
        if array.len() > per_page as usize {
            return Err(Error::Schema("page exceeds requested limit"));
        }
        let items = array.iter().map(project).collect::<Result<Vec<_>>>()?;
        let next = pagination::next_page(raw.link.as_deref(), path, &query, page, per_page);
        Ok(Page {
            items,
            has_more: next.has_more,
            next_page: next.next_page,
            pagination_warning: next.warning,
            page,
            per_page,
            poll_interval_seconds: raw.poll_interval_seconds,
        })
    }
}

fn match_number(value: &Value, expected: u64) -> Result<()> {
    if value["number"].as_u64() == Some(expected) {
        Ok(())
    } else {
        Err(Error::Schema("issue number"))
    }
}
fn valid_id(id: u64) -> Result<()> {
    if id > 0 && id <= i64::MAX as u64 {
        Ok(())
    } else {
        Err(Error::InvalidId)
    }
}
fn valid_state(state: &str) -> Result<()> {
    if matches!(state, "open" | "closed" | "all") {
        Ok(())
    } else {
        Err(Error::InvalidPath)
    }
}
fn component(value: &str, repo: bool) -> Result<()> {
    if value.is_empty()
        || value.len() > 100
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || (repo && b == b'.'))
    {
        Err(Error::InvalidPath)
    } else {
        Ok(())
    }
}
fn repo_path(owner: &str, repo: &str) -> Result<String> {
    component(owner, false)?;
    component(repo, true)?;
    Ok(format!("/repos/{}/{}", encode(owner), encode(repo)))
}
fn valid_ref(revision: Option<&str>) -> Result<()> {
    if revision.is_some_and(|s| s.is_empty() || s.len() > 1024 || s.chars().any(|c| c.is_control()))
    {
        Err(Error::InvalidPath)
    } else {
        Ok(())
    }
}
fn ref_query(revision: Option<&str>) -> Result<Vec<(&'static str, String)>> {
    valid_ref(revision)?;
    Ok(revision
        .map(|r| vec![("ref", r.to_owned())])
        .unwrap_or_default())
}
fn contents_path(owner: &str, repo: &str, path: &str) -> Result<String> {
    if path.len() > 4096
        || path.chars().any(|c| c.is_control() || c == '\\')
        || (!path.is_empty()
            && path
                .split('/')
                .any(|s| s.is_empty() || matches!(s, "." | "..")))
    {
        return Err(Error::InvalidPath);
    }
    let base = format!("{}/contents", repo_path(owner, repo)?);
    Ok(if path.is_empty() {
        base
    } else {
        format!(
            "{base}/{}",
            path.split('/').map(encode).collect::<Vec<_>>().join("/")
        )
    })
}
fn encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 15) as usize] as char);
        }
    }
    out
}
fn comment_issue_number(value: &Value, owner: &str, repo: &str) -> Result<u64> {
    let url = value["issue_url"]
        .as_str()
        .ok_or(Error::Schema("comment issue url"))?;
    let uri = transport::fixed_api_uri(url).map_err(|_| Error::Schema("comment issue url"))?;
    let prefix = format!("{}/issues/", repo_path(owner, repo)?);
    let path = uri.path();
    if path.len() <= prefix.len()
        || path
            .get(..prefix.len())
            .is_none_or(|p| !p.eq_ignore_ascii_case(&prefix))
        || uri.query().is_some()
    {
        return Err(Error::Schema("comment repository"));
    }
    let id = path[prefix.len()..]
        .parse()
        .map_err(|_| Error::Schema("comment issue number"))?;
    valid_id(id)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_paths_encode_content_and_reject_traversal() {
        assert_eq!(
            contents_path("octocat", "r.name", "中文/a %?#.md").unwrap(),
            "/repos/octocat/r.name/contents/%E4%B8%AD%E6%96%87/a%20%25%3F%23.md"
        );
        for bad in ["../token", "a/../b", "/root", "a//b", "a/", "a\\b", "a\nb"] {
            assert!(contents_path("o", "r", bad).is_err(), "{bad}");
        }
        for bad in ["x/y", "x@y", "x?foo", "%2f", ".."] {
            assert!(repo_path(bad, "r").is_err());
        }
        assert_eq!(encode("feature/a?x=1"), "feature%2Fa%3Fx%3D1");
    }
    #[test]
    fn comment_identity_is_repository_and_number_bound() {
        let value = json!({"issue_url":"https://api.github.com/repos/Owner/Repo/issues/19"});
        assert_eq!(comment_issue_number(&value, "owner", "repo").unwrap(), 19);
        assert!(comment_issue_number(&value, "owner", "elsewhere").is_err());
        assert!(
            comment_issue_number(
                &json!({"issue_url":"https://api.github.com@evil.test/repos/owner/repo/issues/19"}),
                "owner",
                "repo"
            )
            .is_err()
        );
    }
    #[test]
    fn secrets_do_not_appear_in_debug_and_missing_token_fails_before_io() {
        let config = Config {
            token: Some("secret_fixture_token".into()),
            ..Config::default()
        };
        assert!(!format!("{config:?}").contains("secret_fixture_token"));
        let client = Client::new(Config::default()).unwrap();
        assert_eq!(client.current_user().unwrap_err(), Error::LoginRequired);
        assert_eq!(
            client.post_issue_comment("o", "r", 1, "hello").unwrap_err(),
            Error::LoginRequired
        );
        assert_eq!(
            client.notifications(1, 20, false, false).unwrap_err(),
            Error::LoginRequired
        );
    }
}
