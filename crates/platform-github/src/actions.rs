// SPDX-License-Identifier: MIT
//! Fixed GitHub collaboration operations. Each call submits at most one write.
//! Primary protocol: docs.github.com/rest/issues/issues, pulls/reviews and pulls/comments.
use crate::{Client, Error, Result, model, repo_path, valid_id};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewLine {
    pub path: String,
    pub body: String,
    pub line: u32,
    pub side: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_side: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CollaborationAction {
    CreateIssue {
        title: String,
        body: String,
        #[serde(default)]
        labels: Vec<String>,
        #[serde(default)]
        assignees: Vec<String>,
    },
    Review {
        event: String,
        body: String,
        commit_id: String,
        #[serde(default)]
        comments: Vec<ReviewLine>,
    },
    ReviewReply {
        comment_id: u64,
        body: String,
    },
    SetState {
        state: String,
        pull: bool,
    },
    SetLabels {
        labels: Vec<String>,
    },
    SetAssignees {
        assignees: Vec<String>,
    },
}
fn text(s: &str, max: usize, empty: bool) -> Result<()> {
    if (!empty && s.trim().is_empty()) || s.len() > max || s.contains('\0') {
        Err(Error::ValidationFailed)
    } else {
        Ok(())
    }
}
fn names(values: &[String], labels: bool) -> Result<()> {
    if values.len() > 20 {
        return Err(Error::Limit);
    }
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        text(value, if labels { 100 } else { 100 }, false)?;
        if value.chars().any(char::is_control) || !seen.insert(value.to_ascii_lowercase()) {
            return Err(Error::ValidationFailed);
        }
        if !labels {
            crate::component(value, false)?;
        }
    }
    Ok(())
}
impl CollaborationAction {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::CreateIssue {
                title,
                body,
                labels,
                assignees,
            } => {
                text(title, 256, false)?;
                text(body, 16384, true)?;
                names(labels, true)?;
                names(assignees, false)?;
            }
            Self::Review {
                event,
                body,
                commit_id,
                comments,
            } => {
                if !matches!(event.as_str(), "COMMENT" | "APPROVE" | "REQUEST_CHANGES")
                    || comments.len() > 16
                    || commit_id.len() != 40
                    || !commit_id.bytes().all(|c| c.is_ascii_hexdigit())
                {
                    return Err(Error::ValidationFailed);
                }
                text(
                    body,
                    16384,
                    event == "APPROVE" || (event == "COMMENT" && !comments.is_empty()),
                )?;
                for c in comments {
                    crate::contents_path("fixture", "repo", &c.path)?;
                    text(&c.body, 4096, false)?;
                    if c.line == 0
                        || !matches!(c.side.as_str(), "LEFT" | "RIGHT")
                        || c.start_line.is_some_and(|n| n == 0 || n > c.line)
                        || c.start_line.is_some() != c.start_side.is_some()
                        || c.start_side.as_ref().is_some_and(|s| s != &c.side)
                    {
                        return Err(Error::ValidationFailed);
                    }
                }
            }
            Self::ReviewReply { comment_id, body } => {
                valid_id(*comment_id)?;
                text(body, 16384, false)?;
            }
            Self::SetState { state, .. } => {
                if !matches!(state.as_str(), "open" | "closed") {
                    return Err(Error::ValidationFailed);
                }
            }
            Self::SetLabels { labels } => names(labels, true)?,
            Self::SetAssignees { assignees } => names(assignees, false)?,
        }
        if serde_json::to_vec(self)
            .map_err(|_| Error::ValidationFailed)?
            .len()
            > 48 * 1024
        {
            return Err(Error::Limit);
        }
        Ok(())
    }
    pub fn action_name(&self) -> &'static str {
        match self {
            Self::CreateIssue { .. } => "create_issue",
            Self::Review { .. } => "review",
            Self::ReviewReply { .. } => "review_reply",
            Self::SetState { .. } => "set_state",
            Self::SetLabels { .. } => "set_labels",
            Self::SetAssignees { .. } => "set_assignees",
        }
    }
}
fn request(
    owner: &str,
    repo: &str,
    number: Option<u64>,
    action: &CollaborationAction,
) -> Result<(&'static str, String, Value, u16)> {
    action.validate()?;
    let root = repo_path(owner, repo)?;
    let n = || {
        let n = number.ok_or(Error::InvalidId)?;
        valid_id(n)?;
        Ok::<u64, Error>(n)
    };
    Ok(match action {
        CollaborationAction::CreateIssue {
            title,
            body,
            labels,
            assignees,
        } => (
            "POST",
            format!("{root}/issues"),
            json!({"title":title,"body":body,"labels":labels,"assignees":assignees}),
            201,
        ),
        CollaborationAction::Review {
            event,
            body,
            commit_id,
            comments,
        } => (
            "POST",
            format!("{root}/pulls/{}/reviews", n()?),
            json!({"event":event,"body":body,"commit_id":commit_id,"comments":comments}),
            200,
        ),
        CollaborationAction::ReviewReply { comment_id, body } => (
            "POST",
            format!("{root}/pulls/{}/comments/{comment_id}/replies", n()?),
            json!({"body":body}),
            201,
        ),
        CollaborationAction::SetState { state, pull } => (
            "PATCH",
            format!("{root}/{}/{}", if *pull { "pulls" } else { "issues" }, n()?),
            json!({"state":state}),
            200,
        ),
        CollaborationAction::SetLabels { labels } => (
            "PUT",
            format!("{root}/issues/{}/labels", n()?),
            json!({"labels":labels}),
            200,
        ),
        CollaborationAction::SetAssignees { assignees } => (
            "PATCH",
            format!("{root}/issues/{}", n()?),
            json!({"assignees":assignees}),
            200,
        ),
    })
}
fn project_review(v: &Value) -> Result<Value> {
    let id = v["id"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or(Error::Schema("review id"))?;
    let user = model::user(v.get("user").ok_or(Error::Schema("review actor"))?, false)?;
    let mut out = json!({"id":id,"user":user});
    for key in [
        "state",
        "body",
        "commit_id",
        "html_url",
        "submitted_at",
        "pull_request_url",
    ] {
        if let Some(value) = v.get(key) {
            out[key] = value.clone();
        }
    }
    Ok(out)
}
impl Client {
    pub fn collaborate(
        &self,
        owner: &str,
        repo: &str,
        number: Option<u64>,
        action: &CollaborationAction,
    ) -> Result<Value> {
        let (method, path, body, status) = request(owner, repo, number, action)?;
        let v = self.transport.write_json(method, &path, &body, status)?;
        match action {
            CollaborationAction::CreateIssue { .. } => {
                model::issue(&v, false).map_err(|_| Error::WriteResultUnknown)
            }
            CollaborationAction::Review { .. } => {
                project_review(&v).map_err(|_| Error::WriteResultUnknown)
            }
            CollaborationAction::ReviewReply { .. } => {
                model::comment(&v, true).map_err(|_| Error::WriteResultUnknown)
            }
            CollaborationAction::SetState { pull, .. } => {
                model::issue(&v, *pull).map_err(|_| Error::WriteResultUnknown)
            }
            CollaborationAction::SetLabels { .. } => {
                let a = v
                    .as_array()
                    .filter(|a| a.len() <= 1000)
                    .ok_or(Error::WriteResultUnknown)?;
                Ok(
                    json!({"labels":a.iter().filter_map(|x|x["name"].as_str()).collect::<Vec<_>>() }),
                )
            }
            CollaborationAction::SetAssignees { .. } => {
                model::issue(&v, false).map_err(|_| Error::WriteResultUnknown)
            }
        }
    }
    pub fn read_review(&self, owner: &str, repo: &str, number: u64, id: u64) -> Result<Value> {
        valid_id(number)?;
        valid_id(id)?;
        project_review(&self.get(
            &format!("{}/pulls/{number}/reviews/{id}", repo_path(owner, repo)?),
            &[],
        )?)
    }
    pub fn read_review_comment(&self, owner: &str, repo: &str, id: u64) -> Result<Value> {
        valid_id(id)?;
        let v = model::comment(
            &self.get(
                &format!("{}/pulls/comments/{id}", repo_path(owner, repo)?),
                &[],
            )?,
            true,
        )?;
        if v["id"].as_u64() != Some(id) {
            return Err(Error::Schema("review comment identity"));
        }
        Ok(v)
    }
    pub fn read_review_comments(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        id: u64,
    ) -> Result<crate::Page> {
        valid_id(number)?;
        valid_id(id)?;
        self.list(
            &format!(
                "{}/pulls/{number}/reviews/{id}/comments",
                repo_path(owner, repo)?
            ),
            &[],
            1,
            100,
            100,
            |v| model::comment(v, true),
        )
    }
    pub fn reviews(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        page: u32,
        per_page: u32,
    ) -> Result<crate::Page> {
        valid_id(number)?;
        self.list(
            &format!("{}/pulls/{number}/reviews", repo_path(owner, repo)?),
            &[],
            page,
            per_page,
            100,
            project_review,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_action_paths_and_wire_payloads_preserve_semantics() {
        let a = CollaborationAction::Review {
            event: "COMMENT".into(),
            body: "review".into(),
            commit_id: "a".repeat(40),
            comments: vec![ReviewLine {
                path: "src/lib.rs".into(),
                body: "explain this".into(),
                line: 9,
                side: "RIGHT".into(),
                start_line: Some(8),
                start_side: Some("RIGHT".into()),
            }],
        };
        let (m, p, b, s) = request("o", "r", Some(7), &a).unwrap();
        assert_eq!(
            (m, p.as_str(), s),
            ("POST", "/repos/o/r/pulls/7/reviews", 200)
        );
        assert_eq!(b["comments"][0]["start_line"], 8);
        let a = CollaborationAction::ReviewReply {
            comment_id: 12,
            body: "reply".into(),
        };
        let (_, p, _, s) = request("o", "r", Some(7), &a).unwrap();
        assert_eq!(
            (p.as_str(), s),
            ("/repos/o/r/pulls/7/comments/12/replies", 201)
        );
        let a = CollaborationAction::SetLabels { labels: vec![] };
        let (m, p, b, s) = request("o", "r", Some(7), &a).unwrap();
        assert_eq!(
            (m, p.as_str(), s),
            ("PUT", "/repos/o/r/issues/7/labels", 200)
        );
        assert_eq!(b["labels"], json!([]));
    }
    #[test]
    fn invalid_review_coordinates_and_repo_names_fail_before_write() {
        let mut a = CollaborationAction::Review {
            event: "APPROVE".into(),
            body: "".into(),
            commit_id: "f".repeat(40),
            comments: vec![],
        };
        assert!(a.validate().is_ok());
        if let CollaborationAction::Review { comments, .. } = &mut a {
            comments.push(ReviewLine {
                path: "../secret".into(),
                body: "text".into(),
                line: 0,
                side: "RIGHT".into(),
                start_line: None,
                start_side: None,
            });
        }
        assert!(a.validate().is_err());
        assert!(
            request(
                "evil/x",
                "r",
                Some(1),
                &CollaborationAction::SetState {
                    state: "closed".into(),
                    pull: false
                }
            )
            .is_err()
        );
        assert!(
            CollaborationAction::SetAssignees {
                assignees: vec!["a".into(), "A".into()]
            }
            .validate()
            .is_err()
        );
    }
}
