// SPDX-License-Identifier: MIT
//! Account-received activity, separate from GitHub's algorithmic home feed.
//! REST protocol: docs.github.com/en/rest/activity/events.
use crate::{Client, Error, Page, Result, component, encode, repo_path, valid_id};
use serde_json::{Value, json};

fn text<'a>(value: &'a Value, key: &'static str, max: usize) -> Result<&'a str> {
    let text = value[key].as_str().ok_or(Error::Schema(key))?;
    if text.is_empty() || text.len() > max || text.chars().any(char::is_control) {
        return Err(Error::Schema(key));
    }
    Ok(text)
}
fn number(value: &Value, key: &str) -> Option<u64> {
    value[key]
        .as_u64()
        .filter(|number| valid_id(*number).is_ok())
}
fn subject(event_type: &str, payload: &Value) -> Value {
    let issue = &payload["issue"];
    let pull = &payload["pull_request"];
    let comment = &payload["comment"];
    let issue_number = number(issue, "number");
    let pull_number = match (number(pull, "number"), number(payload, "number")) {
        (Some(a), Some(b)) if a != b => None,
        (a, b) => a.or(b),
    };
    let comment_id = number(comment, "id");
    let title = |value: &Value| {
        value["title"]
            .as_str()
            .map(|s| s.chars().take(320).collect::<String>())
    };
    match event_type {
        "IssuesEvent" => issue_number.map(|number| json!({"kind":"issue","number":number,"pull":issue["pull_request"].is_object(),"title":title(issue)})),
        "PullRequestEvent" | "PullRequestReviewEvent" => pull_number.map(|number| json!({"kind":"issue","number":number,"pull":true,"title":title(pull)})),
        "IssueCommentEvent" => issue_number.zip(comment_id).map(|(number,comment)| json!({"kind":"comment","number":number,"pull":issue["pull_request"].is_object(),"comment":comment,"title":title(issue)})),
        "PullRequestReviewCommentEvent" => pull_number.zip(comment_id).map(|(number,comment)| json!({"kind":"review_comment","number":number,"comment":comment,"title":title(pull)})),
        _ => None,
    }.unwrap_or(Value::Null)
}
fn event(value: &Value) -> Result<Value> {
    let event_id = text(value, "id", 32)?;
    if !event_id.bytes().all(|b| b.is_ascii_digit()) || event_id.bytes().all(|b| b == b'0') {
        return Err(Error::Schema("event id"));
    }
    let event_type = text(value, "type", 64)?;
    if !event_type.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Error::Schema("event type"));
    }
    let created_at = text(value, "created_at", 40)?;
    let actor = &value["actor"];
    let actor_id = number(actor, "id").ok_or(Error::Schema("event actor id"))?;
    let actor_login = text(actor, "login", 128)?;
    let repo = &value["repo"];
    let repo_id = number(repo, "id").ok_or(Error::Schema("event repository id"))?;
    let name = text(repo, "name", 201)?;
    let (owner, repository) = name
        .split_once('/')
        .ok_or(Error::Schema("event repository name"))?;
    repo_path(owner, repository).map_err(|_| Error::Schema("event repository path"))?;
    let payload = &value["payload"];
    let action = payload["action"]
        .as_str()
        .filter(|s| s.len() <= 64 && !s.chars().any(char::is_control));
    // No payload body, email, installation, diff, token, raw URL or commit list
    // leaves this boundary. Detail is fetched only after selecting a target.
    Ok(
        json!({"id":event_id,"type":event_type,"actor":{"id":actor_id,"login":actor_login},"repo":{"id":repo_id,"name":name},"created_at":created_at,"public":value["public"].as_bool(),"action":action,"subject":subject(event_type,payload)}),
    )
}
impl Client {
    /// Read the authenticated account's received activity. Never calls the
    /// anonymous `/events` or `/received_events/public` alternatives.
    pub fn received_events(&self, login: &str, page: u32) -> Result<Page> {
        self.transport.require_token()?;
        component(login, false)?;
        if !(1..=15).contains(&page) {
            return Err(Error::InvalidPage);
        }
        let mut result = self.list(
            &format!("/users/{}/received_events", encode(login)),
            &[],
            page,
            20,
            20,
            event,
        )?;
        if page == 15 && result.next_page.is_some() {
            result.next_page = None;
            result.has_more = Some(false);
            result.pagination_warning =
                Some("event_window_limit: at most 300 events from the past 30 days".into());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(kind: &str, payload: Value) -> Value {
        json!({"id":"123456","type":kind,"actor":{"id":7,"login":"SyntheticUser","email":"private-email","token":"private-token"},"repo":{"id":8,"name":"owner/repository","url":"https://private.invalid"},"public":true,"created_at":"2026-11-04T00:00:00Z","payload":payload})
    }
    #[test]
    fn activity_projection_keeps_actionable_subjects_and_discards_private_payloads() {
        let raw = sample(
            "IssueCommentEvent",
            json!({"action":"created","issue":{"number":42,"title":"Synthetic issue","body":"private-issue-body"},"comment":{"id":99,"body":"private-comment-body","email":"private-comment-email"},"installation":{"token":"private-installation-token"}}),
        );
        let item = event(&raw).unwrap();
        assert_eq!(
            item["subject"],
            json!({"kind":"comment","number":42,"pull":false,"comment":99,"title":"Synthetic issue"})
        );
        let output = item.to_string();
        for private in ["private-", "payload", "installation", "body", "url"] {
            assert!(!output.contains(private));
        }
        let pull=event(&sample("PullRequestReviewCommentEvent",json!({"number":3,"pull_request":{"number":3,"title":"Synthetic pull"},"comment":{"id":77}}))).unwrap();
        assert_eq!(pull["subject"]["kind"], "review_comment");
        assert_eq!(pull["subject"]["comment"], 77);
        let conflicting = event(&sample(
            "PullRequestEvent",
            json!({"number":3,"pull_request":{"number":4,"title":"Ambiguous target"}}),
        ))
        .unwrap();
        assert_eq!(
            conflicting["subject"],
            Value::Null,
            "conflicting numbers must not create a misleading issue target"
        );
    }
    #[test]
    fn unknown_event_types_stay_metadata_only_and_paths_cannot_escape_the_repository() {
        let unknown = event(&sample(
            "FutureEvent",
            json!({"arbitrary":{"token":"secret"}}),
        ))
        .unwrap();
        assert_eq!(unknown["subject"], Value::Null);
        let mut raw = sample("WatchEvent", json!({"action":"started"}));
        raw["repo"]["name"] = json!("owner/repo/extra");
        assert!(event(&raw).is_err());
        raw = sample("WatchEvent", Value::Null);
        raw["id"] = json!("not-an-event-id");
        assert!(event(&raw).is_err());
        let client = Client::new(crate::Config::default()).unwrap();
        assert_eq!(
            client.received_events("SyntheticUser", 1).unwrap_err(),
            Error::LoginRequired
        );
    }
}
