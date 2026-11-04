// SPDX-License-Identifier: MIT
//! Pixiv account ownership and a small read-only semantic tool. Images remain
//! lazy account-bound resources, never a request for the model to assemble APIs.
use super::*;
use crate::private_state::account_directory;
use serde::{Deserialize, Serialize};
const TARGET: &str = "task-pixiv-target";
const MEDIA: &str = "task-pixiv-media";
const CURSOR: &str = "task-pixiv-feed";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    schema: u32,
    user: platform_pixiv::User,
    session: platform_pixiv::Session,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    artwork: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Media {
    url: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedCursor {
    source: String,
    viewer: String,
    topic: Option<String>,
    snapshot: String,
    offset: usize,
    #[serde(default)]
    seen: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedSnapshot {
    schema: u32,
    source: String,
    viewer: String,
    recommendations: platform_pixiv::HomeRecommendations,
    #[serde(default)]
    next_page: Option<u32>,
    #[serde(default)]
    provider_has_more: Option<bool>,
}
fn fetch_feed_snapshot(
    saved: Saved,
    source: &str,
    topic: Option<&str>,
    page: u32,
    seen: &[String],
) -> Result<FeedSnapshot> {
    let client = platform_pixiv::Client::new(saved.session);
    if client.me().map_err(err)?.id != saved.user.id {
        return Err("Pixiv recommendation viewer differs".into());
    }
    let (mut recommendations, next_page, provider_has_more) = match source {
        "recommended" => (
            platform_pixiv::home_recommendations(&client.home().map_err(err)?, topic)
                .map_err(err)?,
            None,
            None,
        ),
        "following" => {
            let update = client.following(page).map_err(err)?;
            (
                platform_pixiv::HomeRecommendations {
                    items: update.items,
                    available_topics: Vec::new(),
                    topic: None,
                },
                update.next_page,
                Some(update.provider_has_more),
            )
        }
        _ => return Err("unsupported Pixiv feed source".into()),
    };
    recommendations.items.retain(|row| {
        !row["id"]
            .as_str()
            .is_some_and(|id| seen.iter().any(|known| known == id))
    });
    Ok(FeedSnapshot {
        schema: 1,
        source: source.into(),
        viewer: saved.user.id,
        recommendations,
        next_page,
        provider_has_more,
    })
}
fn err(e: platform_pixiv::Error) -> String {
    format!("pixiv-request:{}", e.code())
}
fn saved(state: &Path, account: &str) -> Result<Option<Saved>> {
    let Some(path) = super::auth::saved_session(state, "pixiv", account)? else {
        return Ok(None);
    };
    let file = open_regular(&path, 16384)?;
    if file
        .metadata()
        .map_err(|_| "cannot inspect Pixiv session")?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err("Pixiv session must be private0600".into());
    }
    let value: Saved = serde_json::from_slice(&read_bounded(&path, 16384)?)
        .map_err(|_| "invalid private Pixiv session")?;
    value.user.validate().map_err(err)?;
    if value.schema != 1 {
        return Err("invalid private Pixiv session".into());
    }
    Ok(Some(value))
}
fn artwork_id(reference: &str) -> Result<String> {
    let value = if let Some(path) = reference.strip_prefix("https://www.pixiv.net/") {
        let path = path.split(['?', '#']).next().unwrap_or("");
        path.strip_prefix("artworks/")
            .or_else(|| path.strip_prefix("en/artworks/"))
            .ok_or("not a Pixiv artwork URL")?
    } else {
        reference
    };
    platform_pixiv::id(value).map_err(err)?;
    Ok(value.into())
}
impl Runtime {
    pub(super) fn pixiv_task_matches(&self, method: &str, params: &Value) -> bool {
        if !method.starts_with("social.") {
            return false;
        }
        params["platform"] == "pixiv"
            || params["continuation"]
                .as_str()
                .is_some_and(|r| self.locators.scope_for(CURSOR, r).is_ok())
            || ["reference", "target_ref"]
                .iter()
                .filter_map(|key| params[*key].as_str())
                .any(|r| {
                    r.starts_with("https://www.pixiv.net/")
                        || self
                            .locators
                            .scope_for(TARGET, r)
                            .is_ok_and(|scope| scope.platform == Platform::Pixiv)
                })
    }
    pub(super) fn call_pixiv_social(&mut self, method: &str, params: &Value) -> Result<Value> {
        if params.get("platform").is_some_and(|v| v != "pixiv") {
            return Err("platform differs from the Pixiv target reference".into());
        }
        let mut request = params.clone();
        match method {
            "social.open" => {
                request["action"] = json!("open");
                if let Some(reference) = params["reference"]
                    .as_str()
                    .filter(|r| self.locators.scope_for(TARGET, r).is_ok())
                {
                    request["target_ref"] = json!(reference);
                }
            }
            "social.discover" => {
                if params
                    .get("source")
                    .is_some_and(|v| !matches!(v.as_str(), Some("recommended" | "following")))
                {
                    if params.get("continuation").is_some() {
                        return Err("Pixiv feed continuation source differs".into());
                    }
                    return Ok(
                        json!({"status":"unsupported","platform":"pixiv","detail":"Recommended reads the personalized account homepage; Following reads the separate feed of followed authors. Public rankings are not a substitute."}),
                    );
                }
                request["action"] = json!("home");
            }
            "social.profile" => request["action"] = json!("me"),
            _ => {
                return Ok(
                    json!({"status":"unsupported","platform":"pixiv","detail":"This Pixiv tool currently supports profile, recommendation snapshots, opening artwork metadata and on-demand images. Writes, comments and ugoira decoding are unavailable."}),
                );
            }
        }
        self.call_pixiv("pixiv.read", &request)
    }
    pub fn pixiv_import_login(&mut self, path: &Path) -> Result<Value> {
        if self.account == "public" {
            return Err("Pixiv login needs a personal alias".into());
        }
        let file = open_regular(path, 32768)?;
        if file
            .metadata()
            .map_err(|_| "cannot inspect Pixiv input")?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("Pixiv input must be private0600".into());
        }
        let input = String::from_utf8(read_bounded(path, 32768)?)
            .map_err(|_| "invalid Pixiv private input")?;
        let session = platform_pixiv::Session::from_cookie_header(input.trim()).map_err(err)?;
        let user = platform_pixiv::Client::new(session.clone())
            .me()
            .map_err(err)?;
        let directory = account_directory(&self.state, "pixiv", &self.account)?;
        super::auth::bind_identity(&directory, &user.id)?;
        let value = Saved {
            schema: 1,
            user: user.clone(),
            session,
        };
        let pending = directory.join("session.pending");
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&pending)
                .map_err(|_| "cannot prepare private Pixiv session")?;
            use std::io::Write;
            file.write_all(
                &serde_json::to_vec(&value).map_err(|_| "cannot encode private Pixiv session")?,
            )
            .and_then(|_| file.sync_all())
            .map_err(|_| "cannot save private Pixiv session")?;
            fs::rename(&pending, directory.join("session.json"))
                .map_err(|_| "cannot commit private Pixiv session")?;
            File::open(&directory)
                .and_then(|f| f.sync_all())
                .map_err(|_| "cannot sync private Pixiv session")?;
            Ok(
                json!({"status":"online","identity":{"id":user.id,"label":user.name},"user":user,"session_saved":true,"online_checked":true}),
            )
        })();
        let _ = fs::remove_file(pending);
        result
    }
    pub(crate) fn pixiv_operator_reuse(&mut self) -> Result<Value> {
        let Some(saved) = saved(&self.state, &self.account)? else {
            return Ok(json!({"status":"needs_auth"}));
        };
        match platform_pixiv::Client::new(saved.session).me() {
            Ok(user) => {
                if user.id != saved.user.id {
                    return Err("Pixiv session identity mismatch".into());
                }
                let directory = account_directory(&self.state, "pixiv", &self.account)?;
                super::auth::bind_identity(&directory, &user.id)?;
                Ok(
                    json!({"status":"online","reused_session":true,"identity":{"id":user.id,"label":user.name},"user":user}),
                )
            }
            Err(platform_pixiv::Error::LoginRequired) => Ok(json!({"status":"needs_auth"})),
            Err(e) => Err(err(e)),
        }
    }
    pub(super) fn call_pixiv(&mut self, method: &str, params: &Value) -> Result<Value> {
        if method == "pixiv.status" {
            return Ok(match saved(&self.state, &self.account)? {
                Some(saved) => {
                    json!({"status":"saved_session","user":saved.user,"online_checked":false})
                }
                None => json!({"status":"signed_out"}),
            });
        }
        if method == "pixiv.me" {
            return self.pixiv_operator_reuse();
        }
        if method == "pixiv.media" {
            let reference = required_str(params, "asset")?;
            let scope = self.locators.scope_for(MEDIA, reference)?;
            if scope != self.scope(Platform::Pixiv) {
                return Err("Pixiv media belongs to another account".into());
            }
            let media: Media = self.locators.get(&scope, MEDIA, reference)?;
            let saved =
                saved(&self.state, &self.account)?.ok_or("Pixiv authentication required")?;
            let path = self.state.join("work").join(format!("pixiv-{reference}"));
            let result = (|| {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                    .map_err(|_| "cannot create Pixiv media file")?;
                let mime = platform_pixiv::Client::new(saved.session)
                    .download(
                        &media.url,
                        Budget::default().max_resource_bytes.min(64 * 1024 * 1024),
                        &mut file,
                    )
                    .map_err(err)?;
                let mime = download_mime(&path, Some(&mime), "image")?;
                let resource = self.store.import(
                    &scope,
                    "pixiv-image",
                    &mime,
                    &mut File::open(&path).map_err(|_| "cannot read Pixiv media")?,
                )?;
                Ok(json!({"resource":resource,"media_observed":false}))
            })();
            let _ = fs::remove_file(path);
            return result;
        }
        if method != "pixiv.read" {
            return Err("unsupported Pixiv read operation".into());
        }
        if let Some(reference) = params.get("continuation").and_then(Value::as_str) {
            let scope = self.locators.scope_for(CURSOR, reference)?;
            tasks::check_account(params, &scope)?;
            if scope.platform != Platform::Pixiv
                || params.get("platform").is_some_and(|v| v != "pixiv")
                || params["action"] != "home"
            {
                return Err("Pixiv feed continuation kind/platform mismatch".into());
            }
            let cursor: FeedCursor = self.locators.get(&scope, CURSOR, reference)?;
            return self.with_task_scope(&scope, |rt| rt.pixiv_home_page(params, Some(cursor)));
        }
        if let Some(reference) = params.get("target_ref").and_then(Value::as_str) {
            let scope = self.locators.scope_for(TARGET, reference)?;
            tasks::check_account(params, &scope)?;
            let target: Target = self.locators.get(&scope, TARGET, reference)?;
            return self.with_task_scope(&scope, |runtime| {
                runtime.pixiv_read(params, Some(target.artwork))
            });
        }
        self.with_platform_account("pixiv", params, true, |runtime| {
            runtime.pixiv_read(params, None)
        })
    }
    fn pixiv_read(&mut self, params: &Value, target: Option<String>) -> Result<Value> {
        let action = params["action"]
            .as_str()
            .ok_or("Pixiv read needs an action")?;
        if action == "me" {
            return self.pixiv_operator_reuse();
        }
        if action == "home" {
            return self.pixiv_home_page(params, None);
        }
        let saved = saved(&self.state, &self.account)?.ok_or("Pixiv authentication required")?;
        let client = platform_pixiv::Client::new(saved.session);
        let scope = self.scope(Platform::Pixiv);
        if action != "open" {
            return Err("unsupported Pixiv read action".into());
        }
        let artwork = target
            .map(Ok)
            .unwrap_or_else(|| artwork_id(required_str(params, "reference")?))?;
        let raw = client.artwork(&artwork).map_err(err)?;
        let mut item = platform_pixiv::artwork_view(&raw).map_err(err)?;
        if item["id"] != artwork {
            return Err("Pixiv artwork identity mismatch".into());
        }
        item["target_ref"] = json!(self.locators.put(
            &scope,
            TARGET,
            &Target {
                artwork: artwork.clone()
            }
        )?);
        let raw_pages = client.pages(&artwork).map_err(err)?;
        let rows = raw_pages.as_array().ok_or("Pixiv page schema changed")?;
        let mut images = Vec::new();
        for (index, row) in rows.iter().take(20).enumerate() {
            for quality in ["regular", "original"] {
                if let Some(url) = row["urls"][quality]
                    .as_str()
                    .filter(|u| u.starts_with("https://i.pximg.net/"))
                {
                    images.push(json!({"page":index+1,"quality":quality,"media_ref":self.locators.put(&scope,MEDIA,&Media{url:url.into()})?,"width":row.get("width"),"height":row.get("height")}));
                }
            }
        }
        item["images"] = json!(images);
        item["images_truncated"] = json!(rows.len() > 20);
        item["animation_supported"] = json!(false);
        Ok(json!({"status":"ok","scope":scope,"content":item,"images_fetched":false}))
    }
    fn pixiv_home_page(&mut self, params: &Value, cursor: Option<FeedCursor>) -> Result<Value> {
        let mut cached = cursor.is_some();
        let limit = number(params, "limit", 10)? as usize;
        if !(1..=20).contains(&limit) {
            return Err("Pixiv limit must be 1..20".into());
        }
        let source = cursor
            .as_ref()
            .map(|v| v.source.as_str())
            .or_else(|| params["source"].as_str())
            .unwrap_or("recommended")
            .to_owned();
        if !matches!(source.as_str(), "recommended" | "following")
            || params
                .get("source")
                .is_some_and(|v| v.as_str() != Some(source.as_str()))
        {
            return Err("Pixiv recommendation source differs".into());
        }
        let requested_topic = params
            .get("topic")
            .map(|v| v.as_str().ok_or("Pixiv topic must be a string"))
            .transpose()?;
        if source == "following" && requested_topic.is_some() {
            return Err(
                "Pixiv topics select personalized recommendations, not followed-author updates"
                    .into(),
            );
        }
        let saved = saved(&self.state, &self.account)?.ok_or("Pixiv authentication required")?;
        let scope = self.scope(Platform::Pixiv);
        let mut seen = cursor.as_ref().map(|v| v.seen.clone()).unwrap_or_default();
        if seen.len() > 512 || seen.iter().any(|id| platform_pixiv::id(id).is_err()) {
            return Err("invalid Pixiv feed seen state".into());
        }
        let (mut snapshot, snapshot_ref, offset) = if let Some(cursor) = cursor {
            if cursor.source != source
                || cursor.viewer != saved.user.id
                || requested_topic.is_some_and(|v| Some(v) != cursor.topic.as_deref())
            {
                return Err("Pixiv feed continuation source, topic or viewer differs".into());
            }
            let (_, path) = self.store.resolve(&scope, &cursor.snapshot)?;
            let mut snapshot: FeedSnapshot =
                serde_json::from_slice(&read_bounded(&path, 2 * 1024 * 1024)?)
                    .map_err(|_| "invalid Pixiv recommendation snapshot")?;
            if snapshot.schema != 1
                || snapshot.source != cursor.source
                || snapshot.viewer != cursor.viewer
                || snapshot.recommendations.topic != cursor.topic
                || cursor.offset > snapshot.recommendations.items.len()
            {
                return Err("Pixiv recommendation snapshot differs".into());
            }
            if seen.len() >= 512 {
                snapshot.next_page = None;
            }
            if cursor.offset == snapshot.recommendations.items.len() && snapshot.next_page.is_some()
            {
                if source != "following" {
                    return Err("Pixiv recommendations have no stable provider page cursor".into());
                }
                let page = snapshot.next_page.unwrap();
                cached = false;
                (
                    fetch_feed_snapshot(saved, &source, None, page, &seen)?,
                    None,
                    0,
                )
            } else {
                (snapshot, Some(cursor.snapshot), cursor.offset)
            }
        } else {
            (
                fetch_feed_snapshot(saved, &source, requested_topic, 1, &seen)?,
                None,
                0,
            )
        };
        for row in &snapshot.recommendations.items {
            let id = row["id"].as_str().ok_or("Pixiv artwork has no ID")?;
            if seen.len() < 512 && !seen.iter().any(|known| known == id) {
                seen.push(id.into());
            }
        }
        if seen.len() >= 512 {
            snapshot.next_page = None;
        }
        let rows = &snapshot.recommendations.items;
        if rows.len() > 512 || offset > rows.len() {
            return Err("invalid Pixiv recommendation offset".into());
        }
        let end = offset.saturating_add(limit).min(rows.len());
        let mut items = Vec::new();
        for row in &rows[offset..end] {
            let mut item = platform_pixiv::artwork_view(row).map_err(err)?;
            let artwork = item["id"]
                .as_str()
                .ok_or("Pixiv artwork has no ID")?
                .to_owned();
            item["target_ref"] = json!(self.locators.put(&scope, TARGET, &Target { artwork })?);
            if let Some(url) = row["url"]
                .as_str()
                .filter(|u| u.starts_with("https://i.pximg.net/"))
            {
                item["preview_media_ref"] = json!(self.locators.put(
                    &scope,
                    MEDIA,
                    &Media { url: url.into() }
                )?);
            }
            items.push(item);
        }
        let continuation = if end < rows.len() || snapshot.next_page.is_some() {
            let snapshot_ref = match snapshot_ref {
                Some(value) => value,
                None => {
                    let bytes = serde_json::to_vec(&snapshot)
                        .map_err(|_| "cannot encode Pixiv recommendation snapshot")?;
                    self.store
                        .import(
                            &scope,
                            "pixiv-feed",
                            "application/json",
                            &mut bytes.as_slice(),
                        )?
                        .id
                }
            };
            Some(self.locators.put(
                &scope,
                CURSOR,
                &FeedCursor {
                    source: snapshot.source.clone(),
                    viewer: snapshot.viewer.clone(),
                    topic: snapshot.recommendations.topic.clone(),
                    snapshot: snapshot_ref,
                    offset: end,
                    seen,
                },
            )?)
        } else {
            None
        };
        let following = source == "following";
        let stop_reason = if continuation.is_some() {
            "page_complete"
        } else if snapshot.provider_has_more == Some(true) {
            "cursor_budget"
        } else if following {
            "provider_end"
        } else {
            "homepage_batch_end"
        };
        Ok(
            json!({"status":"ok","scope":scope,"items":items,"has_more":continuation.is_some(),"provider_has_more":snapshot.provider_has_more,"continuation":continuation,"pagination_supported":true,"pagination_kind":if following{"provider_pages_with_cached_tails"}else{"verified_home_snapshot"},"source":if following{"pixiv_following_updates"}else{"pixiv_home_recommendations"},"signed_in_request":true,"viewer_verified":true,"recommendation_kind":if following{"followed_author_updates"}else{"personalized_home"},"cached":cached,"topic":snapshot.recommendations.topic,"available_topics":snapshot.recommendations.available_topics,"feed_mode":"safe","images_fetched":false,"refresh_supported":true,"stop_reason":stop_reason,"pagination_note":"Recommended preserves the account homepage section and optional recommended topic. Following reads a separate followed-author update feed. Continuation binds source, account, viewer and topic, keeps page tails, and deduplicates followed-author pages; omit it to refresh."}),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn homepage_continuation_preserves_order_topic_viewer_and_account_without_network() {
        let state =
            std::env::temp_dir().join(format!("pixiv-feed-pagination-{}", std::process::id()));
        private_dir(&state).unwrap();
        let directory = account_directory(&state, "pixiv", "alice").unwrap();
        save_private_new(&directory.join("session.json"),&json!({"schema":1,"user":{"id":"42","name":"Synthetic"},"session":{"phpsessid":"synthetic"}})).unwrap();
        let mut rt = Runtime::open(&state, "public", None).unwrap();
        let scope = Scope {
            platform: Platform::Pixiv,
            account: "alice".into(),
        };
        let snapshot = FeedSnapshot { schema:1,source:"recommended".into(),viewer:"42".into(),next_page:None,provider_has_more:None,recommendations:platform_pixiv::HomeRecommendations {
            items:(1..=3).map(|id|json!({"id":id.to_string(),"title":"Synthetic recommendation","xRestrict":0,"url":"https://i.pximg.net/synthetic.png"})).collect(),available_topics:vec!["space".into()],topic:Some("space".into()),
        }};
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let resource = rt
            .store
            .import(
                &scope,
                "pixiv-recommendations",
                "application/json",
                &mut bytes.as_slice(),
            )
            .unwrap();
        let reference = rt
            .locators
            .put(
                &scope,
                CURSOR,
                &FeedCursor {
                    source: "recommended".into(),
                    viewer: "42".into(),
                    topic: Some("space".into()),
                    snapshot: resource.id.clone(),
                    offset: 0,
                    seen: Vec::new(),
                },
            )
            .unwrap();
        let first = rt
            .call(
                "social.discover",
                &json!({"continuation":reference,"limit":1}),
            )
            .unwrap();
        assert_eq!(first["items"][0]["id"], "1");
        assert_eq!(first["topic"], "space");
        assert_eq!(first["images_fetched"], false);
        assert_eq!(first["scope"]["account"], "alice");
        let second = rt
            .call(
                "social.discover",
                &json!({"continuation":first["continuation"],"limit":20}),
            )
            .unwrap();
        assert_eq!(
            second["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["2", "3"]
        );
        assert_eq!(second["has_more"], false);
        for extra in [
            json!({"source":"popular"}),
            json!({"topic":"another"}),
            json!({"account":"bob"}),
            json!({"platform":"reddit"}),
        ] {
            let mut params = extra;
            params["continuation"] = json!(reference);
            assert!(rt.call("social.discover", &params).is_err());
        }
        let wrong_viewer = rt
            .locators
            .put(
                &scope,
                CURSOR,
                &FeedCursor {
                    source: "recommended".into(),
                    viewer: "99".into(),
                    topic: Some("space".into()),
                    snapshot: resource.id,
                    offset: 0,
                    seen: Vec::new(),
                },
            )
            .unwrap();
        assert!(
            rt.call("social.discover", &json!({"continuation":wrong_viewer}))
                .unwrap_err()
                .contains("viewer differs")
        );
        let following = FeedSnapshot {
            schema: 1,
            source: "following".into(),
            viewer: "42".into(),
            next_page: None,
            provider_has_more: Some(false),
            recommendations: platform_pixiv::HomeRecommendations {
                items: vec![
                    json!({"id":"4","title":"Synthetic followed author update","xRestrict":0}),
                ],
                available_topics: Vec::new(),
                topic: None,
            },
        };
        let bytes = serde_json::to_vec(&following).unwrap();
        let resource = rt
            .store
            .import(
                &scope,
                "pixiv-following",
                "application/json",
                &mut bytes.as_slice(),
            )
            .unwrap();
        let reference = rt
            .locators
            .put(
                &scope,
                CURSOR,
                &FeedCursor {
                    source: "following".into(),
                    viewer: "42".into(),
                    topic: None,
                    snapshot: resource.id,
                    offset: 0,
                    seen: Vec::new(),
                },
            )
            .unwrap();
        assert!(
            rt.call(
                "social.discover",
                &json!({"continuation":reference,"source":"recommended"})
            )
            .is_err()
        );
        let page = rt
            .call("social.discover", &json!({"continuation":reference}))
            .unwrap();
        assert_eq!(page["source"], "pixiv_following_updates");
        assert_eq!(page["recommendation_kind"], "followed_author_updates");
        assert_eq!(page["items"][0]["id"], "4");
        assert!(!state.join("outbox").exists());
        drop(rt);
        fs::remove_dir_all(state).unwrap();
    }
    #[test]
    fn safe_recommendations_filter_before_limiting_and_skip_unknown_restrictions() {
        let raw = json!({"page":{"recommend":{"ids":["1","2","3","4","5","6"]}},"thumbnails":{"illust":[
            {"id":"1","xRestrict":1},{"id":"2","xRestrict":2},
            {"id":"3"},{"id":"4","xRestrict":"0"},
            {"id":"5","xRestrict":0},{"id":"6","xRestrict":0}
        ]}});
        let recommendations = platform_pixiv::home_recommendations(&raw, None).unwrap();
        let ids = recommendations
            .items
            .iter()
            .take(1)
            .map(|row| row["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["5"]);
        assert!(platform_pixiv::home_recommendations(&json!({"thumbnails":{}}), None).is_err());
    }
    #[test]
    fn exact_pixiv_artwork_references_cannot_become_arbitrary_requests() {
        assert_eq!(
            artwork_id("https://www.pixiv.net/en/artworks/42?lang=en").unwrap(),
            "42"
        );
        for reference in [
            "https://www.pixiv.net.evil/artworks/42",
            "https://www.pixiv.net/ajax/user/extra",
            "https://www.pixiv.net/artworks/42/extra",
            "42?evil",
        ] {
            assert!(artwork_id(reference).is_err());
        }
    }
    #[test]
    fn shared_social_and_media_tools_keep_pixiv_ownership_and_unsupported_writes_explicit() {
        let state =
            std::env::temp_dir().join(format!("pixiv-task-ownership-{}", std::process::id()));
        private_dir(&state).unwrap();
        let mut runtime = Runtime::open(&state, "public", None).unwrap();
        let scope = Scope {
            platform: Platform::Pixiv,
            account: "alice".into(),
        };
        let reference = runtime
            .locators
            .put(
                &scope,
                TARGET,
                &Target {
                    artwork: "42".into(),
                },
            )
            .unwrap();
        let media = runtime
            .locators
            .put(
                &scope,
                MEDIA,
                &Media {
                    url: "https://i.pximg.net/synthetic.png".into(),
                },
            )
            .unwrap();
        assert_eq!(runtime.media_reference_scope(&media).unwrap(), scope);
        assert!(
            runtime
                .call(
                    "social.open",
                    &json!({"reference":reference,"account":"bob"})
                )
                .unwrap_err()
                .contains("account differs")
        );
        assert!(
            runtime
                .call(
                    "social.open",
                    &json!({"reference":reference,"platform":"bilibili"})
                )
                .unwrap_err()
                .contains("platform differs")
        );
        assert!(
            runtime
                .call("media.read", &json!({"reference":media,"account":"bob"}))
                .unwrap_err()
                .contains("account differs")
        );
        assert_eq!(
            runtime
                .call("social.like", &json!({"target_ref":reference}))
                .unwrap()["status"],
            "unsupported"
        );
        assert_eq!(
            runtime
                .call(
                    "social.discover",
                    &json!({"platform":"pixiv","source":"popular"})
                )
                .unwrap()["status"],
            "unsupported"
        );
        assert!(!state.join("outbox").exists());
        drop(runtime);
        fs::remove_dir_all(state).unwrap();
    }
}
