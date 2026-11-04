// SPDX-License-Identifier: MIT
//! Read-only Pixiv website adapter. Uses an operator-provided web session,
//! never mobile-app OAuth credentials, browser execution or password capture.
use html5gum::{Token, Tokenizer, emitters::default::DefaultEmitter};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fmt,
    io::{Read, Write},
    time::Duration,
};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidInput,
    LoginRequired,
    VerificationRequired,
    RateLimited,
    NotFound,
    ProtocolChanged,
    Network,
    Limit,
}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::LoginRequired => "needs_auth",
            Self::VerificationRequired => "verification_required",
            Self::RateLimited => "rate_limited",
            Self::NotFound => "not_found",
            Self::ProtocolChanged => "protocol_changed",
            Self::Network => "network_error",
            Self::Limit => "limit_exceeded",
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pixiv adapter: {}", self.code())
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "RawSession")]
pub struct Session {
    phpsessid: String,
}
#[derive(Deserialize)]
struct RawSession {
    phpsessid: String,
}
impl TryFrom<RawSession> for Session {
    type Error = Error;
    fn try_from(v: RawSession) -> Result<Self> {
        Self::from_cookie_header(&format!("PHPSESSID={}", v.phpsessid))
    }
}
impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Session([redacted])")
    }
}
impl Session {
    pub fn from_cookie_header(input: &str) -> Result<Self> {
        if input.len() > 32768 || input.bytes().any(|b| b.is_ascii_control()) {
            return Err(Error::InvalidInput);
        }
        let mut value = None;
        for field in input.split(';').take(256) {
            if let Some((name, secret)) = field.trim().split_once('=') {
                if name == "PHPSESSID" {
                    if value.is_some()
                        || secret.is_empty()
                        || secret.len() > 4096
                        || !secret
                            .bytes()
                            .all(|b| b.is_ascii_graphic() && !b";\"\\,".contains(&b))
                    {
                        return Err(Error::InvalidInput);
                    }
                    value = Some(secret.into());
                }
            }
        }
        Ok(Self {
            phpsessid: value.ok_or(Error::InvalidInput)?,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct User {
    pub id: String,
    pub name: String,
}
/// Only the account homepage's explicitly ordered recommendation section.
/// `thumbnails.illust` is a shared index containing rankings and other modules.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HomeRecommendations {
    pub items: Vec<Value>,
    pub available_topics: Vec<String>,
    pub topic: Option<String>,
}
pub fn home_recommendations(raw: &Value, topic: Option<&str>) -> Result<HomeRecommendations> {
    if topic.is_some_and(|v| v.is_empty() || v.len() > 256 || v.chars().any(char::is_control)) {
        return Err(Error::InvalidInput);
    }
    let page = raw
        .get("page")
        .and_then(Value::as_object)
        .ok_or(Error::ProtocolChanged)?;
    let mut topics = Vec::new();
    let mut selected = None;
    if let Some(groups) = page.get("recommendByTag") {
        let groups = groups.as_array().ok_or(Error::ProtocolChanged)?;
        if groups.len() > 64 {
            return Err(Error::Limit);
        }
        for group in groups {
            let tag = group["tag"]
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control))
                .ok_or(Error::ProtocolChanged)?;
            if topics.iter().any(|known| known == tag) {
                return Err(Error::ProtocolChanged);
            }
            topics.push(tag.to_owned());
            if topic == Some(tag) {
                selected = Some(group);
            }
        }
    }
    let section = match topic {
        None => page.get("recommend").ok_or(Error::ProtocolChanged)?,
        Some(_) => selected.ok_or(Error::NotFound)?,
    };
    let ids = section["ids"].as_array().ok_or(Error::ProtocolChanged)?;
    Ok(HomeRecommendations {
        items: ordered_feed_items(raw, ids)?,
        available_topics: topics,
        topic: topic.map(str::to_owned),
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FollowingUpdates {
    pub items: Vec<Value>,
    pub next_page: Option<u32>,
    pub provider_has_more: bool,
}
pub fn following_updates(raw: &Value, page: u32) -> Result<FollowingUpdates> {
    if !(1..=100).contains(&page) {
        return Err(Error::InvalidInput);
    }
    let ids = raw
        .pointer("/page/ids")
        .and_then(Value::as_array)
        .ok_or(Error::ProtocolChanged)?;
    let last = raw
        .pointer("/page/isLastPage")
        .and_then(Value::as_bool)
        .ok_or(Error::ProtocolChanged)?;
    Ok(FollowingUpdates {
        items: ordered_feed_items(raw, ids)?,
        next_page: (!last && page < 100).then_some(page + 1),
        provider_has_more: !last,
    })
}
fn ordered_feed_items(raw: &Value, ids: &[Value]) -> Result<Vec<Value>> {
    use std::collections::{BTreeMap, BTreeSet};
    let rows = raw
        .pointer("/thumbnails/illust")
        .and_then(Value::as_array)
        .ok_or(Error::ProtocolChanged)?;
    if ids.len() > 512 || rows.len() > 2048 {
        return Err(Error::Limit);
    }
    let mut index = BTreeMap::new();
    for row in rows {
        if row["isAdContainer"] == true {
            continue;
        }
        let key = row["id"].as_str().ok_or(Error::ProtocolChanged)?;
        id(key)?;
        if let Some(previous) = index.insert(key, row) {
            if previous != row {
                return Err(Error::ProtocolChanged);
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for key in ids {
        let key = key
            .as_str()
            .map(str::to_owned)
            .or_else(|| key.as_u64().map(|v| v.to_string()))
            .ok_or(Error::ProtocolChanged)?;
        id(&key)?;
        if !seen.insert(key.clone()) {
            continue;
        }
        let row = *index.get(key.as_str()).ok_or(Error::ProtocolChanged)?;
        // Keep the existing safe feed policy; missing flags never imply safe.
        if row["xRestrict"].as_u64() != Some(0) || row["isMasked"] == true {
            continue;
        }
        items.push(row.clone());
    }
    Ok(items)
}
impl User {
    pub fn validate(&self) -> Result<()> {
        id(&self.id)?;
        if self.name.is_empty() || self.name.len() > 512 || self.name.chars().any(char::is_control)
        {
            return Err(Error::ProtocolChanged);
        }
        Ok(())
    }
}
pub fn id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 20
        || value.starts_with('0')
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        Err(Error::InvalidInput)
    } else {
        Ok(())
    }
}

pub struct Client {
    session: Session,
    http: ureq::Agent,
}
impl Client {
    pub fn new(session: Session) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .max_idle_connections(0)
            .build();
        Self {
            session,
            http: config.into(),
        }
    }
    fn bytes(&self, path: &str, accept: &str) -> Result<Vec<u8>> {
        let mut response = self
            .http
            .get(format!("https://www.pixiv.net{path}"))
            .header("Cookie", format!("PHPSESSID={}", self.session.phpsessid))
            .header("Referer", "https://www.pixiv.net/")
            .header("Accept", accept)
            .header("User-Agent", "Glimlink/0.1 (Pixiv web-session client)")
            .call()
            .map_err(|_| Error::Network)?;
        status(response.status().as_u16())?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Network)?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(Error::Limit);
        }
        Ok(bytes)
    }
    /// Verify identity from the authenticated HTML response's userData, never
    /// from a PHPSESSID prefix or an arbitrary public profile lookup.
    pub fn me(&self) -> Result<User> {
        let bytes = self.bytes("/", "text/html")?;
        let html = std::str::from_utf8(&bytes).map_err(|_| Error::ProtocolChanged)?;
        if let Some(actor) = identity_next_id(html)? {
            match identity_html(html) {
                Ok(legacy) if legacy.id != actor => return Err(Error::ProtocolChanged),
                Err(Error::LoginRequired) => return Err(Error::LoginRequired),
                _ => {}
            }
            // SSR identifies the session's own actor. The public profile is
            // used only for its display name, never as authentication proof.
            let profile = self.ajax(&format!("/ajax/user/{actor}?full=0&lang=en"))?;
            return identity_profile(&actor, &profile);
        }
        identity_html(html)
    }
    fn ajax(&self, path: &str) -> Result<Value> {
        let bytes = self.bytes(path, "application/json")?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::ProtocolChanged)?;
        if value["error"] == true {
            return Err(Error::VerificationRequired);
        }
        if value["error"] != false {
            return Err(Error::ProtocolChanged);
        }
        value.get("body").cloned().ok_or(Error::ProtocolChanged)
    }
    pub fn home(&self) -> Result<Value> {
        // The website accepts `all`; `safe` produces HTTP 400. The Runtime
        // retains only explicitly unrestricted entries before exposing a feed.
        self.ajax("/ajax/top/illust?mode=all&lang=en")
    }
    pub fn following(&self, page: u32) -> Result<FollowingUpdates> {
        if !(1..=100).contains(&page) {
            return Err(Error::InvalidInput);
        }
        following_updates(
            &self.ajax(&format!(
                "/ajax/follow_latest/illust?p={page}&mode=all&lang=en"
            ))?,
            page,
        )
    }
    pub fn artwork(&self, artwork: &str) -> Result<Value> {
        id(artwork)?;
        self.ajax(&format!("/ajax/illust/{artwork}?lang=en"))
    }
    pub fn pages(&self, artwork: &str) -> Result<Value> {
        id(artwork)?;
        self.ajax(&format!("/ajax/illust/{artwork}/pages?lang=en"))
    }
    /// Only a saved Pixiv CDN URL is accepted. Platform cookies are never sent
    /// to the CDN, and bytes stream into the Runtime resource store.
    pub fn download(&self, url: &str, limit: u64, output: &mut impl Write) -> Result<String> {
        if !url.starts_with("https://i.pximg.net/")
            || url.len() > 4096
            || url.chars().any(char::is_control)
            || url.contains(['?', '#', '\\'])
            || !(1..=64 * 1024 * 1024).contains(&limit)
        {
            return Err(Error::InvalidInput);
        }
        let mut response = self
            .http
            .get(url)
            .header("Referer", "https://www.pixiv.net/")
            .call()
            .map_err(|_| Error::Network)?;
        status(response.status().as_u16())?;
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .to_owned();
        if !matches!(
            mime.as_str(),
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ) {
            return Err(Error::ProtocolChanged);
        }
        let mut reader = response.body_mut().as_reader();
        let mut total = 0u64;
        let mut buffer = [0; 32768];
        loop {
            let n = reader.read(&mut buffer).map_err(|_| Error::Network)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > limit {
                return Err(Error::Limit);
            }
            output.write_all(&buffer[..n]).map_err(|_| Error::Network)?;
        }
        if total == 0 {
            return Err(Error::ProtocolChanged);
        }
        Ok(mime)
    }
}
fn status(status: u16) -> Result<()> {
    match status {
        200 => Ok(()),
        301 | 302 | 303 | 307 | 308 | 401 => Err(Error::LoginRequired),
        400 => Err(Error::ProtocolChanged),
        403 => Err(Error::VerificationRequired),
        404 => Err(Error::NotFound),
        429 => Err(Error::RateLimited),
        _ => Err(Error::Network),
    }
}
fn identity_profile(actor: &str, profile: &Value) -> Result<User> {
    let returned = profile["userId"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| profile["userId"].as_u64().map(|id| id.to_string()))
        .ok_or(Error::ProtocolChanged)?;
    if actor != returned {
        return Err(Error::ProtocolChanged);
    }
    let user = User {
        id: actor.into(),
        name: profile["name"]
            .as_str()
            .ok_or(Error::ProtocolChanged)?
            .into(),
    };
    user.validate()?;
    Ok(user)
}
fn identity_next_id(input: &str) -> Result<Option<String>> {
    if input.len() > 2 * 1024 * 1024 {
        return Err(Error::Limit);
    }
    let mut emitter = DefaultEmitter::default();
    emitter.naively_switch_states(true);
    let mut capture = false;
    let mut seen = false;
    let mut bytes = Vec::new();
    let mut actor = None;
    for (index, token) in Tokenizer::new_with_emitter(input, emitter).enumerate() {
        if index >= 65536 {
            return Err(Error::Limit);
        }
        match token.map_err(|_| Error::ProtocolChanged)? {
            Token::StartTag(tag) if tag.name.as_ref() == b"script" => {
                let attr = |key: &[u8]| {
                    tag.attributes
                        .iter()
                        .find(|(k, _)| k.as_ref() == key)
                        .map(|(_, v)| v.value.as_ref())
                };
                if attr(b"id") == Some(b"__NEXT_DATA__".as_slice()) {
                    if seen || attr(b"type") != Some(b"application/json".as_slice()) {
                        return Err(Error::ProtocolChanged);
                    }
                    seen = true;
                    capture = true;
                }
            }
            Token::String(value) if capture => {
                if bytes.len() + value.len() > 1024 * 1024 {
                    return Err(Error::Limit);
                }
                bytes.extend_from_slice(value.as_ref());
            }
            Token::EndTag(tag) if capture && tag.name.as_ref() == b"script" => {
                capture = false;
                let value: Value =
                    serde_json::from_slice(&bytes).map_err(|_| Error::ProtocolChanged)?;
                let props = value
                    .pointer("/props/pageProps")
                    .ok_or(Error::ProtocolChanged)?;
                match props["isLoggedIn"].as_bool() {
                    Some(false) => return Err(Error::LoginRequired),
                    Some(true) => {}
                    _ => return Err(Error::ProtocolChanged),
                }
                let value = &props["gaUserData"]["userId"];
                let own = value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_u64().map(|v| v.to_string()))
                    .ok_or(Error::ProtocolChanged)?;
                id(&own)?;
                actor = Some(own);
            }
            _ => {}
        }
    }
    if capture {
        return Err(Error::ProtocolChanged);
    }
    Ok(actor)
}
fn identity_html(input: &str) -> Result<User> {
    if input.len() > 2 * 1024 * 1024 {
        return Err(Error::Limit);
    }
    let mut found = None;
    let mut emitter = DefaultEmitter::default();
    emitter.naively_switch_states(true);
    for token in Tokenizer::new_with_emitter(input, emitter).take(65536) {
        if let Token::StartTag(tag) = token.map_err(|_| Error::ProtocolChanged)? {
            if tag.name.as_ref() != b"meta" {
                continue;
            }
            let attr = |name: &[u8]| {
                tag.attributes
                    .iter()
                    .find(|(k, _)| k.as_ref() == name)
                    .map(|(_, v)| String::from_utf8_lossy(&v.value).into_owned())
            };
            if attr(b"name").as_deref() != Some("global-data")
                || attr(b"id").as_deref() != Some("meta-global-data")
            {
                continue;
            }
            if found.is_some() {
                return Err(Error::ProtocolChanged);
            }
            let data: Value =
                serde_json::from_str(&attr(b"content").ok_or(Error::ProtocolChanged)?)
                    .map_err(|_| Error::ProtocolChanged)?;
            if data["userData"].is_null() {
                return Err(Error::LoginRequired);
            }
            let user = User {
                id: data["userData"]["id"]
                    .as_str()
                    .ok_or(Error::ProtocolChanged)?
                    .into(),
                name: data["userData"]["name"]
                    .as_str()
                    .ok_or(Error::ProtocolChanged)?
                    .into(),
            };
            user.validate()?;
            found = Some(user);
        }
    }
    found.ok_or(Error::ProtocolChanged)
}

/// Small public projection. Unknown provider fields and account tokens are not
/// propagated; image URLs stay inside the host's lazy media locators.
pub fn artwork_view(raw: &Value) -> Result<Value> {
    let artwork = raw["id"]
        .as_str()
        .or_else(|| raw["illustId"].as_str())
        .ok_or(Error::ProtocolChanged)?;
    id(artwork)?;
    Ok(
        json!({"id":artwork,"title":raw.get("title").or_else(||raw.get("illustTitle")),"description_html":raw.get("description"),"creator":{"id":raw.get("userId"),"name":raw.get("userName")},"page_count":raw.get("pageCount"),"width":raw.get("width"),"height":raw.get("height"),"kind":raw.get("illustType"),"counts":{"likes":raw.get("likeCount"),"bookmarks":raw.get("bookmarkCount"),"views":raw.get("viewCount"),"comments":raw.get("commentCount")},"created_at":raw.get("createDate"),"tags":raw.pointer("/tags/tags").or_else(||raw.get("tags"))}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn followed_author_feed_uses_numeric_ordered_ids_and_explicit_last_page() {
        let raw = json!({"page":{"ids":[3,1,3],"isLastPage":false},"thumbnails":{"illust":[{"id":"1","xRestrict":0},{"id":"3","xRestrict":0},{"id":"99","xRestrict":0}]}});
        let page = following_updates(&raw, 1).unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|v| v["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["3", "1"]
        );
        assert_eq!(page.next_page, Some(2));
        assert!(page.provider_has_more);
        let mut last = raw.clone();
        last["page"]["isLastPage"] = json!(true);
        assert!(following_updates(&last, 2).unwrap().next_page.is_none());
        assert!(
            following_updates(&json!({"page":{"ids":[]},"thumbnails":{"illust":[]}}), 1).is_err()
        );
        assert!(following_updates(&raw, 101).is_err());
    }
    #[test]
    fn homepage_uses_only_the_selected_personal_section_in_server_order() {
        let raw = json!({"page":{"recommend":{"ids":["3","1","3","4"]},"recommendByTag":[{"tag":"space","ids":["2","3"]}]},"thumbnails":{"illust":[
            {"id":"99","title":"ranking not a recommendation","xRestrict":0},
            {"id":"1","xRestrict":0},{"id":"2","xRestrict":0},
            {"id":"3","xRestrict":0},{"id":"4","xRestrict":1}
        ]}});
        let main = home_recommendations(&raw, None).unwrap();
        assert_eq!(
            main.items
                .iter()
                .map(|v| v["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["3", "1"]
        );
        assert_eq!(main.available_topics, vec!["space"]);
        let topic = home_recommendations(&raw, Some("space")).unwrap();
        assert_eq!(
            topic
                .items
                .iter()
                .map(|v| v["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["2", "3"]
        );
        assert_eq!(
            home_recommendations(&raw, Some("arbitrary search")).unwrap_err(),
            Error::NotFound
        );
        let mut missing = raw.clone();
        missing["page"]["recommend"]["ids"] = json!(["100"]);
        assert_eq!(
            home_recommendations(&missing, None).unwrap_err(),
            Error::ProtocolChanged
        );
        assert_eq!(
            home_recommendations(&json!({"thumbnails":raw["thumbnails"]}), None).unwrap_err(),
            Error::ProtocolChanged
        );
    }
    #[test]
    fn modern_identity_requires_logged_in_own_actor_not_a_public_creator() {
        let html = r#"<script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"isLoggedIn":true,"gaUserData":{"userId":"42","token":"synthetic-private"},"artist":{"userId":"99"}}}}</script>"#;
        let actor = identity_next_id(html).unwrap().unwrap();
        assert_eq!(actor, "42");
        let user = identity_profile(
            &actor,
            &json!({"userId":"42","name":"Synthetic","extra":"not projected"}),
        )
        .unwrap();
        assert_eq!(user.id, "42");
        assert!(!serde_json::to_string(&user).unwrap().contains("extra"));
        assert!(identity_profile(&actor, &json!({"userId":"99","name":"another"})).is_err());
        assert_eq!(
            identity_next_id(&html.replace("\"isLoggedIn\":true", "\"isLoggedIn\":false"))
                .unwrap_err(),
            Error::LoginRequired
        );
        assert!(
            identity_next_id(&html.replace("\"userId\":\"42\"", "\"userId\":\"42/path\"")).is_err()
        );
        assert!(identity_next_id(&format!("{html}{html}")).is_err());
        assert!(identity_next_id(&html.replace("</script>", "")).is_err());
        assert!(
            identity_next_id("<script>var publicAuthor={userId:99}</script>")
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn identity_comes_from_authenticated_user_data_not_cookie_or_creator() {
        let html = r#"<meta id="meta-global-data" name="global-data" content="{&quot;token&quot;:&quot;private-csrf&quot;,&quot;userData&quot;:{&quot;id&quot;:&quot;42&quot;,&quot;name&quot;:&quot;猫 &amp; 花&quot;}}"><meta name="preload-data" content='{"illust":{"1":{"userId":"99"}}}'>"#;
        let user = identity_html(html).unwrap();
        assert_eq!(user.id, "42");
        assert_eq!(user.name, "猫 & 花");
        assert!(!serde_json::to_string(&user).unwrap().contains("private"));
        assert_eq!(
            identity_html(
                r#"<meta id="meta-global-data" name="global-data" content='{"userData":null}'>"#
            )
            .unwrap_err(),
            Error::LoginRequired
        );
        assert!(identity_html(&format!("{html}{html}")).is_err());
        assert!(identity_html(&format!("<script>const source={html:?};</script>")).is_err());
    }
    #[test]
    fn private_cookie_and_id_validation_rejects_ambiguous_or_injected_input() {
        assert!(Session::from_cookie_header("irrelevant=gone; PHPSESSID=42_synthetic").is_ok());
        for bad in [
            "PHPSESSID=",
            "PHPSESSID=one; PHPSESSID=two",
            "PHPSESSID=one\r\nOther: bad",
        ] {
            assert!(Session::from_cookie_header(bad).is_err());
        }
        for bad in ["0", "42/other", "42?x=1", "42\n"] {
            assert!(id(bad).is_err());
        }
    }
}
