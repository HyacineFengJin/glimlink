// SPDX-License-Identifier: MIT
//! One AI-facing emote panel, with account-bound package and continuation refs.
use super::*;
use crate::private_state::LocatorBatch;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const PACKAGE: &str = "task-emote-package";
const CONTINUATION: &str = "task-emotes";
const PACKAGES: &str = "bilibili.emote_packages";
const TOKENS: &str = "bilibili.emotes";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmoteContinuation {
    route: String,
    params: Value,
}
impl EmoteContinuation {
    fn validate(&self) -> Result<()> {
        if !matches!(self.route.as_str(), PACKAGES | TOKENS) {
            return Err("saved emote continuation route is invalid".into());
        }
        let object = self
            .params
            .as_object()
            .ok_or("saved emote parameters must be an object")?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "offset" | "limit" | "package_id" | "text"))
        {
            return Err("saved emote continuation contains invalid parameters".into());
        }
        if number(&self.params, "offset", 0)? > 4096 {
            return Err("saved emote offset exceeds bounds".into());
        }
        task_limit(&self.params)?;
        match self.route.as_str() {
            PACKAGES => {
                if self.params.get("text").is_some() {
                    return Err("package continuation contains a token selector".into());
                }
                if let Some(id) = self.params.get("package_id") {
                    if id.as_u64().is_none_or(|id| id == 0) {
                        return Err("invalid saved package identity".into());
                    }
                }
            }
            TOKENS => {
                if self.params.get("package_id").is_some() {
                    return Err("token continuation contains a package selector".into());
                }
                token_text(&self.params)?;
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}

fn task_limit(params: &Value) -> Result<u64> {
    let limit = number(params, "limit", 24)?;
    if !(1..=64).contains(&limit) {
        return Err("emote limit must be 1..64".into());
    }
    Ok(limit)
}
fn token_text(params: &Value) -> Result<&str> {
    let text = required_str(params, "text")?;
    if text.is_empty() || text.len() > 256 || text.chars().any(char::is_control) {
        return Err("text must be an exact nonempty emote token".into());
    }
    Ok(text)
}
fn task_selection(params: &Value) -> Result<()> {
    let object = params
        .as_object()
        .ok_or("emote parameters must be an object")?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "account" | "package_ref" | "text" | "continuation" | "limit"
        )
    }) {
        return Err("use package_ref/text or a continuation; provider IDs and offsets are managed by Runtime".into());
    }
    let selectors = ["package_ref", "text", "continuation"]
        .iter()
        .filter(|key| params.get(**key).is_some())
        .count();
    if selectors > 1 {
        return Err("package_ref, text and continuation are mutually exclusive".into());
    }
    task_limit(params)?;
    Ok(())
}

struct Projection<'a, 'b> {
    scope: &'a Scope,
    assets: LocatorBatch<'b>,
    package_refs: BTreeMap<u64, String>,
    media_refs: BTreeMap<String, String>,
}
impl Projection<'_, '_> {
    fn package_ref(&mut self, id: u64) -> Result<String> {
        if id == 0 {
            return Err("emote package has a zero identity".into());
        }
        if let Some(reference) = self.package_refs.get(&id) {
            return Ok(reference.clone());
        }
        let reference = self.assets.put(self.scope, PACKAGE, &id)?;
        self.package_refs.insert(id, reference.clone());
        Ok(reference)
    }
    fn media(&mut self, node: &mut Value) -> Result<()> {
        let object = node
            .as_object_mut()
            .ok_or("emote projection must be an object")?;
        let url = object.remove("url");
        let Some(url) = url.filter(|value| !value.is_null()) else {
            return Ok(());
        };
        let url = url.as_str().ok_or("emote media locator must be text")?;
        // Some kaomoji catalog variants put the literal text into `url`.
        // It is an emote token, not a network address.
        if object.get("type").and_then(Value::as_u64) == Some(4)
            && !url.starts_with("https://")
            && !url.starts_with("http://")
            && !url.starts_with("//")
        {
            return Ok(());
        }
        platform_bilibili::validate_resource_url(url)
            .map_err(|_| "emote media locator is outside the permitted CDN")?;
        let reference = if let Some(reference) = self.media_refs.get(url) {
            reference.clone()
        } else {
            let resource = MediaResource {
                url: url.into(),
                backup_urls: vec![],
                expected_bytes: None,
                mime_type: None,
            };
            let reference = self.assets.put(self.scope, "bilibili-media", &resource)?;
            self.media_refs.insert(url.into(), reference.clone());
            reference
        };
        object.insert("media_ref".into(), json!(reference));
        object.insert("read_tool".into(), json!("media_read"));
        object.insert("media_fetched".into(), json!(false));
        Ok(())
    }
    fn token(&mut self, node: &mut Value) -> Result<()> {
        self.media(node)?;
        if let Some(id) = node
            .get("package_id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
        {
            node["package_ref"] = json!(self.package_ref(id)?);
        }
        Ok(())
    }
    fn package(&mut self, node: &mut Value) -> Result<()> {
        self.media(node)?;
        if let Some(items) = node.get_mut("emote") {
            for item in items
                .as_array_mut()
                .ok_or("package entries must be a list")?
            {
                self.token(item)?;
            }
        }
        let id = node
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or("emote package identity is missing")?;
        node["package_ref"] = json!(self.package_ref(id)?);
        Ok(())
    }
}

impl Runtime {
    pub(super) fn task_emotes(&mut self, params: &Value) -> Result<Value> {
        task_selection(params)?;
        if let Some(id) = params.get("continuation") {
            let id = id
                .as_str()
                .ok_or("continuation must be an opaque reference")?;
            let scope = self.locators.scope_for(CONTINUATION, id)?;
            if scope.platform != Platform::Bilibili {
                return Err("emote continuation has an invalid provider".into());
            }
            super::tasks::check_account(params, &scope)?;
            let mut saved: EmoteContinuation = self.locators.get(&scope, CONTINUATION, id)?;
            saved.validate()?;
            if params.get("limit").is_some() {
                saved.params["limit"] = json!(task_limit(params)?);
            }
            return self.with_task_scope(&scope, |runtime| {
                runtime.emote_task_page(&saved.route, &saved.params)
            });
        }
        if let Some(id) = params.get("package_ref") {
            let id = id
                .as_str()
                .ok_or("package_ref must be an opaque reference")?;
            let scope = self.locators.scope_for(PACKAGE, id)?;
            if scope.platform != Platform::Bilibili {
                return Err("emote package has an invalid provider".into());
            }
            super::tasks::check_account(params, &scope)?;
            let package: u64 = self.locators.get(&scope, PACKAGE, id)?;
            if package == 0 {
                return Err("saved package identity is invalid".into());
            }
            let query = json!({"package_id":package,"offset":0,"limit":task_limit(params)?});
            return self
                .with_task_scope(&scope, |runtime| runtime.emote_task_page(PACKAGES, &query));
        }
        let query = if params.get("text").is_some() {
            json!({"text":token_text(params)?,"offset":0,"limit":task_limit(params)?})
        } else {
            json!({"offset":0,"limit":task_limit(params)?})
        };
        let route = if params.get("text").is_some() {
            TOKENS
        } else {
            PACKAGES
        };
        self.with_platform_account("bilibili", params, true, |runtime| {
            runtime.emote_task_page(route, &query)
        })
    }

    fn emote_task_page(&mut self, route: &str, query: &Value) -> Result<Value> {
        let mut request = query.clone();
        // A missing saved session for a referenced alias must fail rather than
        // letting the lower-level helper select a different sole account.
        request["account"] = json!(self.account);
        let response = match route {
            PACKAGES => self.call_comment_emote_packages(&request)?,
            TOKENS => self.call_comment_emotes(&request)?,
            _ => return Err("invalid emote task route".into()),
        };
        let scope = self.scope(Platform::Bilibili);
        self.project_emote_page(&scope, route, query, response)
    }

    fn project_emote_page(
        &self,
        scope: &Scope,
        route: &str,
        query: &Value,
        mut response: Value,
    ) -> Result<Value> {
        if let Some(returned) = response.get("scope") {
            let returned: Scope = serde_json::from_value(returned.clone())
                .map_err(|_| "emote page scope is invalid")?;
            if &returned != scope {
                return Err("emote page came from a different account scope".into());
            }
        }
        let total = response
            .get("total")
            .and_then(Value::as_u64)
            .ok_or("emote page total is missing")?;
        let next = match response.get("next_offset") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .filter(|next| *next > number(query, "offset", 0).unwrap_or(0) && *next <= 4096)
                    .ok_or("emote continuation did not advance")?,
            ),
        };
        let mut package_refs = BTreeMap::new();
        // Reuse package capabilities across panel reads; the AI does not need
        // to replace references merely because it looked at the panel again.
        for offset in (0..2048).step_by(128) {
            let records = self.locators.list(scope, offset, 128)?;
            let done = records.len() < 128;
            for record in records {
                if record["kind"] != PACKAGE {
                    continue;
                }
                if let Some(id) = record["asset"].as_str() {
                    if let Ok(package) = self.locators.get::<u64>(scope, PACKAGE, id) {
                        if package > 0 {
                            package_refs.entry(package).or_insert_with(|| id.to_owned());
                        }
                    }
                }
            }
            if done {
                break;
            }
        }
        let mut projection = Projection {
            scope,
            assets: self.locators.batch(),
            package_refs,
            media_refs: BTreeMap::new(),
        };
        let mut result = json!({"status":"ok","scope":scope,"total":total,"media_downloaded":false,
            "flags_note":"added is the service panel flag, not purchase history; unlocked=false alone does not mean unavailable; media type/size alone does not prove animation."});
        match route {
            PACKAGES => {
                let packages = response["packages"]
                    .as_array_mut()
                    .ok_or("emote packages are missing")?;
                if query.get("package_id").is_some() {
                    if packages.len() != 1 {
                        return Err("selected package response is ambiguous".into());
                    }
                    let mut package = packages.remove(0);
                    if package["id"] != query["package_id"] {
                        return Err("selected package identity changed".into());
                    }
                    let mut items = package
                        .as_object_mut()
                        .ok_or("package object expected")?
                        .remove("emote")
                        .ok_or("selected package entries are missing")?;
                    for item in items
                        .as_array_mut()
                        .ok_or("package entries must be a list")?
                    {
                        projection.token(item)?;
                    }
                    projection.package(&mut package)?;
                    result["package"] = package;
                    result["items"] = items;
                } else {
                    for package in packages.iter_mut() {
                        projection.package(package)?;
                    }
                    result["packages"] = json!(packages);
                }
            }
            TOKENS => {
                let items = response["items"]
                    .as_array_mut()
                    .ok_or("emote entries are missing")?;
                for item in items.iter_mut() {
                    projection.token(item)?;
                }
                result["items"] = json!(items);
            }
            _ => return Err("invalid emote projection route".into()),
        }
        result["continuation"] = if let Some(offset) = next {
            let mut params = query.clone();
            params["offset"] = json!(offset);
            let saved = EmoteContinuation {
                route: route.into(),
                params,
            };
            saved.validate()?;
            json!(projection.assets.put(scope, CONTINUATION, &saved)?)
        } else {
            Value::Null
        };
        projection.assets.commit();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    fn lab() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "emote-client-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }
    #[test]
    fn panel_projection_hides_urls_preserves_flags_and_binds_packages() {
        let root = lab();
        let runtime = Runtime::open(&root.join("state"), "a", None).unwrap();
        let scope = runtime.scope(Platform::Bilibili);
        let source = json!({"packages":[{"id":9,"text":"series","url":"https://i0.hdslb.com/bfs/emote/package.png",
            "flags":{"added":true,"unlocked":false,"no_access":null},"emote_count":25}],"total":1,"next_offset":null});
        let result = runtime
            .project_emote_page(
                &scope,
                PACKAGES,
                &json!({"offset":0,"limit":24}),
                source.clone(),
            )
            .unwrap();
        let package = &result["packages"][0];
        assert!(package.get("url").is_none());
        assert_eq!(package["flags"]["unlocked"], false);
        let reference = package["package_ref"].as_str().unwrap();
        assert_eq!(
            runtime
                .locators
                .get::<u64>(&scope, PACKAGE, reference)
                .unwrap(),
            9
        );
        let media = package["media_ref"].as_str().unwrap();
        let stored: MediaResource = runtime
            .locators
            .get(&scope, "bilibili-media", media)
            .unwrap();
        assert!(stored.url.ends_with("package.png"));
        assert!(!result.to_string().contains("https://"));
        let again = runtime
            .project_emote_page(&scope, PACKAGES, &json!({"offset":0,"limit":24}), source)
            .unwrap();
        assert_eq!(again["packages"][0]["package_ref"], reference);
        let other = Scope {
            platform: Platform::Bilibili,
            account: "b".into(),
        };
        assert!(
            runtime
                .locators
                .get::<u64>(&other, PACKAGE, reference)
                .is_err()
        );
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn token_projection_keeps_exact_text_and_opaque_continuation_without_eager_media() {
        let root = lab();
        let runtime = Runtime::open(&root.join("state"), "a", None).unwrap();
        let scope = runtime.scope(Platform::Bilibili);
        let query = json!({"package_id":9,"offset":0,"limit":1});
        let source = json!({"packages":[{"id":9,"text":"series","url":null,"flags":{"added":true},"emote_count":2,
            "emote":[{"id":7,"package_id":9,"text":"[series_hello]","url":"https://i0.hdslb.com/bfs/emote/hello.gif",
                "type":3,"meta":{"size":2},"flags":{"unlocked":false}}]}],"total":2,"next_offset":1});
        let result = runtime
            .project_emote_page(&scope, PACKAGES, &query, source)
            .unwrap();
        assert_eq!(result["items"][0]["text"], "[series_hello]");
        assert_eq!(result["items"][0]["read_tool"], "media_read");
        assert_eq!(result["media_downloaded"], false);
        assert!(!result.to_string().contains("https://"));
        let saved: EmoteContinuation = runtime
            .locators
            .get(
                &scope,
                CONTINUATION,
                result["continuation"].as_str().unwrap(),
            )
            .unwrap();
        assert_eq!(saved.params["offset"], 1);
        assert_eq!(saved.params["package_id"], 9);
        assert!(saved.validate().is_ok());
        let source = json!({"items":[{"text":"_(:з」∠)_","type":4,"url":"_(:з」∠)_","flags":{"unlocked":false}}],"total":1,"next_offset":null});
        let result = runtime
            .project_emote_page(
                &scope,
                TOKENS,
                &json!({"text":"_(:з」∠)_","offset":0,"limit":24}),
                source,
            )
            .unwrap();
        assert!(result["items"][0].get("media_ref").is_none());
        assert!(result["items"][0].get("url").is_none());
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn selectors_and_saved_continuations_cannot_retarget_or_call_write_routes() {
        assert!(task_selection(&json!({"package_ref":"x","text":"[doge]"})).is_err());
        assert!(task_selection(&json!({"offset":1})).is_err());
        assert!(task_selection(&json!({"limit":65})).is_err());
        assert!(
            EmoteContinuation {
                route: "bilibili.like".into(),
                params: json!({"offset":1,"limit":24})
            }
            .validate()
            .is_err()
        );
        assert!(
            EmoteContinuation {
                route: PACKAGES.into(),
                params: json!({"package_id":9,"offset":1,"limit":24,"account":"other"})
            }
            .validate()
            .is_err()
        );
        assert!(
            EmoteContinuation {
                route: TOKENS.into(),
                params: json!({"package_id":9,"offset":1,"limit":24,"text":"[doge]"})
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn invalid_media_or_scope_does_not_leave_partial_private_capabilities() {
        let root = lab();
        let runtime = Runtime::open(&root.join("state"), "a", None).unwrap();
        let scope = runtime.scope(Platform::Bilibili);
        let query = json!({"offset":0,"limit":24});
        let response = json!({"packages":[{"id":1,"text":"good","url":"https://i0.hdslb.com/bfs/emote/ok.png"},
            {"id":2,"text":"bad","url":"https://evil.example/secret.png"}],"total":2,"next_offset":null});
        assert!(
            runtime
                .project_emote_page(&scope, PACKAGES, &query, response)
                .is_err()
        );
        assert!(runtime.locators.list(&scope, 0, 128).unwrap().is_empty());
        let response = json!({"scope":{"platform":"bilibili","account":"b"},"packages":[],"total":0,"next_offset":null});
        assert!(
            runtime
                .project_emote_page(&scope, PACKAGES, &query, response)
                .is_err()
        );
        assert!(runtime.locators.list(&scope, 0, 128).unwrap().is_empty());
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }
}
