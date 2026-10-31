// SPDX-License-Identifier: MIT
//! Intentional response projections. URLs are data and are never request targets.
use crate::{Error, Result, valid_id};
use serde_json::{Map, Value, json};

fn object(value: &Value, keys: &[&str]) -> Result<Map<String, Value>> {
    let raw = value.as_object().ok_or(Error::Schema("expected object"))?;
    Ok(keys
        .iter()
        .filter_map(|k| raw.get(*k).map(|v| ((*k).to_owned(), v.clone())))
        .collect())
}
fn id(value: &Value, key: &'static str) -> Result<u64> {
    let id = value[key].as_u64().ok_or(Error::Schema(key))?;
    valid_id(id).map_err(|_| Error::Schema(key))?;
    Ok(id)
}
fn text<'a>(value: &'a Value, key: &'static str) -> Result<&'a str> {
    value[key].as_str().ok_or(Error::Schema(key))
}
fn optional_user(value: Option<&Value>) -> Result<Value> {
    match value {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(v) => user(v, false),
    }
}
pub(crate) fn user(value: &Value, profile: bool) -> Result<Value> {
    id(value, "id")?;
    text(value, "login")?;
    let mut out = object(
        value,
        &[
            "id",
            "login",
            "type",
            "html_url",
            "avatar_url",
            "site_admin",
        ],
    )?;
    if profile {
        out.extend(object(
            value,
            &[
                "name",
                "company",
                "blog",
                "location",
                "bio",
                "public_repos",
                "public_gists",
                "followers",
                "following",
                "created_at",
                "updated_at",
                "hireable",
            ],
        )?);
    }
    Ok(Value::Object(out))
}
pub(crate) fn repository(value: &Value) -> Result<Value> {
    id(value, "id")?;
    text(value, "name")?;
    text(value, "full_name")?;
    let mut out = object(
        value,
        &[
            "id",
            "name",
            "full_name",
            "private",
            "description",
            "html_url",
            "default_branch",
            "language",
            "stargazers_count",
            "watchers_count",
            "forks_count",
            "open_issues_count",
            "subscribers_count",
            "archived",
            "disabled",
            "visibility",
            "created_at",
            "updated_at",
            "pushed_at",
            "fork",
            "has_issues",
            "has_discussions",
        ],
    )?;
    out.insert("owner".into(), optional_user(value.get("owner"))?);
    if let Some(license) = value.get("license").filter(|v| !v.is_null()) {
        out.insert(
            "license".into(),
            Value::Object(object(license, &["key", "name", "spdx_id"])?),
        );
    }
    Ok(Value::Object(out))
}
fn reactions(value: &Value) -> Result<Value> {
    Ok(Value::Object(object(
        value,
        &[
            "total_count",
            "+1",
            "-1",
            "laugh",
            "hooray",
            "confused",
            "heart",
            "rocket",
            "eyes",
        ],
    )?))
}
pub(crate) fn issue(value: &Value, pull: bool) -> Result<Value> {
    id(value, "id")?;
    id(value, "number")?;
    text(value, "title")?;
    let mut out = object(
        value,
        &[
            "id",
            "number",
            "title",
            "body",
            "state",
            "state_reason",
            "locked",
            "active_lock_reason",
            "comments",
            "html_url",
            "created_at",
            "updated_at",
            "closed_at",
            "author_association",
        ],
    )?;
    out.insert("user".into(), optional_user(value.get("user"))?);
    if let Some(raw) = value.get("assignees") {
        out.insert(
            "assignees".into(),
            Value::Array(
                raw.as_array()
                    .ok_or(Error::Schema("assignees"))?
                    .iter()
                    .map(|v| user(v, false))
                    .collect::<Result<Vec<_>>>()?,
            ),
        );
    }
    if let Some(raw) = value.get("labels") {
        out.insert(
            "labels".into(),
            Value::Array(
                raw.as_array()
                    .ok_or(Error::Schema("labels"))?
                    .iter()
                    .map(|v| {
                        if v.is_string() {
                            Ok(json!({"name":v}))
                        } else {
                            Ok(Value::Object(object(
                                v,
                                &["id", "name", "color", "description", "default"],
                            )?))
                        }
                    })
                    .collect::<Result<Vec<_>>>()?,
            ),
        );
    }
    if let Some(v) = value.get("reactions") {
        out.insert("reactions".into(), reactions(v)?);
    }
    if let Some(v) = value.get("milestone").filter(|v| !v.is_null()) {
        out.insert(
            "milestone".into(),
            Value::Object(object(
                v,
                &[
                    "id",
                    "number",
                    "title",
                    "description",
                    "state",
                    "due_on",
                    "html_url",
                ],
            )?),
        );
    }
    let is_pull = pull || value.get("pull_request").is_some_and(|v| v.is_object());
    out.insert("is_pull_request".into(), json!(is_pull));
    if let Some(v) = value.get("pull_request") {
        out.insert(
            "pull_request".into(),
            Value::Object(object(
                v,
                &["url", "html_url", "diff_url", "patch_url", "merged_at"],
            )?),
        );
    }
    if pull {
        out.extend(object(
            value,
            &[
                "draft",
                "merged",
                "mergeable",
                "mergeable_state",
                "merge_commit_sha",
                "merged_at",
                "commits",
                "additions",
                "deletions",
                "changed_files",
                "review_comments",
                "maintainer_can_modify",
                "rebaseable",
            ],
        )?);
        for key in ["head", "base"] {
            if let Some(v) = value.get(key) {
                let mut branch = object(v, &["ref", "sha", "label"])?;
                branch.insert("user".into(), optional_user(v.get("user"))?);
                if let Some(repo) = v.get("repo").filter(|r| !r.is_null()) {
                    branch.insert("repo".into(), repository(repo)?);
                }
                out.insert(key.into(), Value::Object(branch));
            }
        }
        if let Some(v) = value.get("requested_reviewers") {
            out.insert(
                "requested_reviewers".into(),
                Value::Array(
                    v.as_array()
                        .ok_or(Error::Schema("requested reviewers"))?
                        .iter()
                        .map(|u| user(u, false))
                        .collect::<Result<Vec<_>>>()?,
                ),
            );
        }
    }
    Ok(Value::Object(out))
}
pub(crate) fn comment(value: &Value, review: bool) -> Result<Value> {
    id(value, "id")?;
    if !value["body"].is_string() && !value["body"].is_null() {
        return Err(Error::Schema("comment body"));
    }
    let mut out = object(
        value,
        &[
            "id",
            "body",
            "html_url",
            "issue_url",
            "created_at",
            "updated_at",
            "author_association",
            "minimized",
            "minimized_reason",
        ],
    )?;
    out.insert("user".into(), optional_user(value.get("user"))?);
    if let Some(v) = value.get("reactions") {
        out.insert("reactions".into(), reactions(v)?);
    }
    if review {
        out.extend(object(
            value,
            &[
                "diff_hunk",
                "path",
                "line",
                "start_line",
                "original_line",
                "original_start_line",
                "side",
                "start_side",
                "position",
                "original_position",
                "commit_id",
                "original_commit_id",
                "in_reply_to_id",
                "pull_request_review_id",
                "pull_request_url",
                "subject_type",
            ],
        )?);
    }
    Ok(Value::Object(out))
}
pub(crate) fn pull_file(value: &Value) -> Result<Value> {
    text(value, "filename")?;
    text(value, "status")?;
    let mut out = object(
        value,
        &[
            "sha",
            "filename",
            "status",
            "additions",
            "deletions",
            "changes",
            "patch",
            "previous_filename",
            "blob_url",
        ],
    )?;
    out.insert("patch_available".into(), json!(value["patch"].is_string()));
    Ok(Value::Object(out))
}
pub(crate) fn notification(value: &Value) -> Result<Value> {
    if value["id"]
        .as_str()
        .is_none_or(|s| s.is_empty() || s.len() > 100 || !s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(Error::Schema("notification id"));
    }
    let mut out = object(
        value,
        &["id", "unread", "reason", "updated_at", "last_read_at"],
    )?;
    out.insert(
        "subject".into(),
        Value::Object(object(
            &value["subject"],
            &["title", "type", "url", "latest_comment_url"],
        )?),
    );
    out.insert("repository".into(), repository(&value["repository"])?);
    Ok(Value::Object(out))
}
fn content_entry(value: &Value) -> Result<Value> {
    text(value, "name")?;
    text(value, "path")?;
    text(value, "type")?;
    if value["size"].as_u64().is_none() {
        return Err(Error::Schema("content size"));
    }
    let out = object(
        value,
        &[
            "name",
            "path",
            "sha",
            "size",
            "type",
            "html_url",
            "submodule_git_url",
            "target",
        ],
    )?;
    Ok(Value::Object(out))
}
pub(crate) fn contents(
    value: &Value,
    path: &str,
    git_ref: Option<&str>,
    max_file_bytes: usize,
) -> Result<Value> {
    let entries = if value.is_array() {
        value.as_array()
    } else if value["type"] == "dir" {
        value["entries"].as_array()
    } else {
        None
    };
    if let Some(entries) = entries {
        if entries.len() > 1000 {
            return Err(Error::Schema("directory entry limit"));
        }
        let entries = entries
            .iter()
            .map(content_entry)
            .collect::<Result<Vec<_>>>()?;
        return Ok(
            json!({"kind":"directory","type":"dir","path":path,"git_ref":git_ref,"possibly_truncated":entries.len()==1000,"directory_limit":1000,"entries":entries}),
        );
    }
    let mut out = content_entry(value)?
        .as_object()
        .cloned()
        .ok_or(Error::Schema("contents"))?;
    if value["path"].as_str() != Some(path) {
        return Err(Error::Schema("content path identity"));
    }
    let kind = match value["type"].as_str() {
        Some("file") if !value["submodule_git_url"].is_string() => "file",
        Some("file" | "submodule") => "submodule",
        Some("symlink") => "symlink",
        _ => return Err(Error::Schema("content type")),
    };
    out.insert("kind".into(), json!(kind));
    out.insert("git_ref".into(), json!(git_ref));
    out.insert(
        "readable".into(),
        json!(
            kind == "file"
                && value["size"]
                    .as_u64()
                    .is_some_and(|s| s <= max_file_bytes as u64)
        ),
    );
    Ok(Value::Object(out))
}

pub(crate) fn file_size(metadata: &Value, max_bytes: usize) -> Result<u64> {
    if metadata["kind"] != "file" || metadata["type"] != "file" {
        return Err(Error::UnsupportedContent(
            "only regular repository files can be read",
        ));
    }
    let size = metadata["size"]
        .as_u64()
        .ok_or(Error::Schema("file size"))?;
    if size > max_bytes as u64 {
        return Err(Error::Limit);
    }
    Ok(size)
}
pub(crate) fn file_with_content(
    mut metadata: Value,
    bytes: &[u8],
    max_bytes: usize,
) -> Result<Value> {
    let size = file_size(&metadata, max_bytes)?;
    if bytes.len() > max_bytes {
        return Err(Error::Limit);
    }
    if bytes.len() as u64 != size {
        return Err(Error::Schema("file changed or raw length differs"));
    }
    use base64::Engine;
    metadata["encoding"] = json!("base64");
    metadata["content"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projections_keep_semantics_and_drop_unneeded_fields() {
        let issue=issue(&json!({"id":9,"number":7,"title":"hostile body","body":"untrusted","pull_request":{"html_url":"https://github.com/o/r/pull/7","unneeded":"discard"},"user":null,"token":"never return","reactions":{"+1":2,"total_count":2,"url":"discard"}}),false).unwrap();
        assert_eq!(issue["is_pull_request"], true);
        assert_eq!(issue["reactions"]["+1"], 2);
        assert!(issue.get("token").is_none());
        assert!(issue["pull_request"].get("unneeded").is_none());
        assert!(issue["reactions"].get("url").is_none());
    }
    #[test]
    fn contents_separates_directory_metadata_and_regular_file_bytes() {
        let dir=contents(&json!({"type":"dir","entries":[{"type":"file","name":"a","path":"d/a","size":3,"sha":"abc","download_url":"https://evil.test"}]}),"d",Some("commit"),3).unwrap();
        assert_eq!(dir["kind"], "directory");
        assert!(dir["entries"][0].get("download_url").is_none());
        let file = contents(
            &json!({"type":"file","name":"a","path":"a","size":4,"sha":"abc","content":"c2VjcmV0"}),
            "a",
            Some("commit"),
            3,
        )
        .unwrap();
        assert_eq!(file["readable"], false);
        assert!(file.get("content").is_none());
        let sub=contents(&json!({"type":"file","name":"a","path":"a","size":0,"sha":"abc","submodule_git_url":"git://untrusted"}),"a",None,3).unwrap();
        assert_eq!(sub["kind"], "submodule");
        assert_eq!(sub["readable"], false);
        assert!(
            contents(
                &json!({"type":"file","name":"a","path":"elsewhere","size":0}),
                "a",
                None,
                3
            )
            .is_err()
        );
    }
    #[test]
    fn unavailable_diff_is_not_an_empty_successful_diff() {
        let file =
            pull_file(&json!({"filename":"binary.png","status":"modified","changes":1})).unwrap();
        assert_eq!(file["patch_available"], false);
        assert!(file.get("patch").is_none());
    }
    #[test]
    fn raw_file_budget_counts_bytes_before_base64_and_rejects_mismatched_data() {
        use base64::Engine;
        let bytes = vec![0xff; crate::MAX_BODY_BYTES];
        let metadata = contents(
            &json!({"type":"file","name":"data","path":"data","size":bytes.len(),"sha":"abc"}),
            "data",
            Some("frozen"),
            crate::MAX_BODY_BYTES,
        )
        .unwrap();
        let file = file_with_content(metadata.clone(), &bytes, crate::MAX_BODY_BYTES).unwrap();
        assert!(file["content"].as_str().unwrap().len() > crate::MAX_BODY_BYTES);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(file["content"].as_str().unwrap())
                .unwrap(),
            bytes
        );
        assert!(
            file_with_content(
                metadata.clone(),
                &bytes[..bytes.len() - 1],
                crate::MAX_BODY_BYTES
            )
            .is_err()
        );
        assert_eq!(
            file_with_content(metadata, &bytes, crate::MAX_BODY_BYTES - 1).unwrap_err(),
            Error::Limit
        );
        assert!(
            file_with_content(
                json!({"kind":"directory","type":"dir","size":0}),
                &[],
                crate::MAX_BODY_BYTES
            )
            .is_err()
        );
    }
}
