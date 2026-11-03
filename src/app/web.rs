// SPDX-License-Identifier: MIT
//! Account-bound Reddit/Discord reading through the common social tools.
//! The browser stays on the operator; this module contains only bounded Rust
//! requests, private session persistence, projections and lazy media references.
use super::*;
use crate::private_state::account_directory;
use platform_web::{Site, User};
use serde::{Deserialize, Serialize};
const TARGET: &str = "task-web-target";
const CURSOR: &str = "task-web-cursor";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    schema: u32,
    user: User,
    session: platform_web::Session,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Target {
    RedditPost { post: String },
    DiscordGuild { id: String },
    DiscordChannel { id: String },
}
impl Target {
    fn site(&self) -> Site {
        match self {
            Self::RedditPost { .. } => Site::Reddit,
            _ => Site::Discord,
        }
    }
    fn validate(&self) -> Result<()> {
        match self {
            Self::RedditPost { post } => platform_web::id(Site::Reddit, post),
            Self::DiscordGuild { id } | Self::DiscordChannel { id } => {
                platform_web::id(Site::Discord, id)
            }
        }
        .map_err(|e| e.to_string())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    site: Site,
    action: String,
    target: Option<Target>,
    after: Option<String>,
    snapshot: Option<String>,
    offset: usize,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Media {
    url: String,
}
fn platform(site: Site) -> Platform {
    match site {
        Site::Reddit => Platform::Reddit,
        Site::Discord => Platform::Discord,
    }
}
fn error(site: Site, e: platform_web::Error) -> String {
    format!("web-request:{}:{}", site.name(), e.code())
}
fn media_kind(site: Site) -> &'static str {
    match site {
        Site::Reddit => "task-reddit-media",
        Site::Discord => "task-discord-media",
    }
}
fn saved(state: &Path, site: Site, account: &str) -> Result<Option<Saved>> {
    let Some(path) = super::auth::saved_session(state, site.name(), account)? else {
        return Ok(None);
    };
    let file = open_regular(&path, 32768)?;
    if file
        .metadata()
        .map_err(|_| "cannot inspect website session")?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err("website session must be private0600".into());
    }
    let value: Saved = serde_json::from_slice(&read_bounded(&path, 32768)?)
        .map_err(|_| "invalid private website session")?;
    if value.schema != 1 || value.session.site() != site {
        return Err("website session platform mismatch".into());
    }
    value.user.validate(site).map_err(|e| error(site, e))?;
    Ok(Some(value))
}
fn parse_reference(reference: &str) -> Result<Target> {
    if let Some(path) = reference
        .strip_prefix("https://www.reddit.com/")
        .or_else(|| reference.strip_prefix("https://reddit.com/"))
        .or_else(|| reference.strip_prefix("https://old.reddit.com/"))
    {
        let path = path.split(['?', '#']).next().unwrap_or("");
        let parts: Vec<_> = path.trim_matches('/').split('/').collect();
        let post = if parts.first() == Some(&"comments") {
            parts.get(1)
        } else if parts.first() == Some(&"r") && parts.get(2) == Some(&"comments") {
            parts.get(3)
        } else {
            None
        }
        .ok_or("use a Reddit post URL")?;
        let target = Target::RedditPost {
            post: (*post).into(),
        };
        target.validate()?;
        return Ok(target);
    }
    if let Some(path) = reference.strip_prefix("https://discord.com/channels/") {
        let path = path.split(['?', '#']).next().unwrap_or("");
        let parts: Vec<_> = path.trim_matches('/').split('/').collect();
        let target = match parts.as_slice() {
            [guild] => Target::DiscordGuild {
                id: (*guild).into(),
            },
            [guild, channel] | [guild, channel, _] => {
                if *guild != "@me" {
                    platform_web::id(Site::Discord, guild).map_err(|e| e.to_string())?
                }
                Target::DiscordChannel {
                    id: (*channel).into(),
                }
            }
            _ => return Err("use a Discord guild/channel URL".into()),
        };
        target.validate()?;
        return Ok(target);
    }
    Err("unsupported website content URL".into())
}
impl Runtime {
    pub fn web_import_login(&mut self, name: &str, path: &Path) -> Result<Value> {
        let site = match name {
            "reddit" => Site::Reddit,
            "discord" => Site::Discord,
            _ => return Err("unsupported website login".into()),
        };
        if self.account == "public" {
            return Err("website sign-in needs a personal account alias".into());
        }
        let file = open_regular(path, 32768)?;
        if file
            .metadata()
            .map_err(|_| "cannot inspect website input")?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("website login input must be private0600".into());
        }
        let input = String::from_utf8(read_bounded(path, 32768)?)
            .map_err(|_| "website input must be UTF-8")?;
        let session = platform_web::Session::from_private_input(site, input.trim())
            .map_err(|e| error(site, e))?;
        let user = platform_web::Client::new(session.clone())
            .me()
            .map_err(|e| error(site, e))?;
        let dir = account_directory(&self.state, name, &self.account)?;
        super::auth::bind_identity(&dir, &user.id)?;
        let record = Saved {
            schema: 1,
            user: user.clone(),
            session,
        };
        let pending = dir.join("session.pending");
        let result = (|| {
            use std::io::Write;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&pending)
                .map_err(|_| "cannot prepare website session")?;
            file.write_all(
                &serde_json::to_vec(&record)
                    .map_err(|_| "cannot encode private website session")?,
            )
            .and_then(|_| file.sync_all())
            .map_err(|_| "cannot save website session")?;
            fs::rename(&pending, dir.join("session.json"))
                .map_err(|_| "cannot commit website session")?;
            File::open(&dir)
                .and_then(|f| f.sync_all())
                .map_err(|_| "cannot sync website session")?;
            Ok(
                json!({"status":"online","identity":{"id":user.id,"label":user.name},"user":user,"session_saved":true,"online_checked":true}),
            )
        })();
        let _ = fs::remove_file(pending);
        result
    }
    pub(crate) fn web_operator_reuse(&self, name: &str) -> Result<Value> {
        let site = match name {
            "reddit" => Site::Reddit,
            "discord" => Site::Discord,
            _ => return Err("unsupported website login".into()),
        };
        let Some(saved) = saved(&self.state, site, &self.account)? else {
            return Ok(json!({"status":"needs_auth"}));
        };
        match platform_web::Client::new(saved.session).me() {
            Ok(user) => {
                if user.id != saved.user.id {
                    return Err("website session identity mismatch".into());
                }
                super::auth::bind_identity(
                    &account_directory(&self.state, name, &self.account)?,
                    &user.id,
                )?;
                Ok(
                    json!({"status":"online","identity":{"id":user.id,"label":user.name},"user":user,"reused_session":true}),
                )
            }
            Err(platform_web::Error::LoginRequired) => Ok(json!({"status":"needs_auth"})),
            Err(e) => Err(error(site, e)),
        }
    }
    pub(super) fn call_web(&mut self, method: &str, params: &Value) -> Result<Value> {
        let (name, action) = method.split_once('.').ok_or("website operation missing")?;
        let site = match name {
            "reddit" => Site::Reddit,
            "discord" => Site::Discord,
            _ => return Err("unsupported website".into()),
        };
        if action == "status" {
            return Ok(match saved(&self.state, site, &self.account)? {
                Some(v) => json!({"status":"saved_session","user":v.user,"online_checked":false}),
                None => json!({"status":"signed_out"}),
            });
        }
        if action == "me" {
            return self.web_operator_reuse(name);
        }
        if action == "media" {
            return self.web_download(site, required_str(params, "asset")?);
        }
        Err("unsupported website management operation".into())
    }
    pub(super) fn web_task_matches(&self, method: &str, params: &Value) -> bool {
        method.starts_with("social.")
            && (matches!(params["platform"].as_str(), Some("reddit" | "discord"))
                || ["reference", "target_ref"]
                    .iter()
                    .filter_map(|key| params[*key].as_str())
                    .any(|r| {
                        parse_reference(r).is_ok() || self.locators.scope_for(TARGET, r).is_ok()
                    })
                || params["continuation"]
                    .as_str()
                    .is_some_and(|r| self.locators.scope_for(CURSOR, r).is_ok()))
    }
    pub(super) fn call_web_social(&mut self, method: &str, params: &Value) -> Result<Value> {
        if let Some(reference) = params["continuation"].as_str() {
            let scope = self.locators.scope_for(CURSOR, reference)?;
            tasks::check_account(params, &scope)?;
            let cursor: Cursor = self.locators.get(&scope, CURSOR, reference)?;
            if params
                .get("platform")
                .is_some_and(|v| v != cursor.site.name())
            {
                return Err("platform differs from website continuation".into());
            }
            if cursor.action != method || platform(cursor.site) != scope.platform {
                return Err("website continuation kind/platform mismatch".into());
            }
            return self.with_task_scope(&scope, |rt| {
                rt.web_read(
                    cursor.site,
                    method,
                    params,
                    cursor.target.as_ref(),
                    cursor.after.as_deref(),
                    cursor.snapshot.as_deref(),
                    cursor.offset,
                )
            });
        }
        let reference = params["target_ref"]
            .as_str()
            .or_else(|| params["reference"].as_str());
        if let Some(reference) = reference {
            if let Ok(scope) = self.locators.scope_for(TARGET, reference) {
                tasks::check_account(params, &scope)?;
                let target: Target = self.locators.get(&scope, TARGET, reference)?;
                target.validate()?;
                if platform(target.site()) != scope.platform {
                    return Err("website target platform mismatch".into());
                }
                if params
                    .get("platform")
                    .is_some_and(|v| v != target.site().name())
                {
                    return Err("platform differs from website target reference".into());
                }
                return self.with_task_scope(&scope, |rt| {
                    rt.web_read(target.site(), method, params, Some(&target), None, None, 0)
                });
            }
            let target = parse_reference(reference)?;
            if params
                .get("platform")
                .is_some_and(|v| v != target.site().name())
            {
                return Err("platform differs from website URL".into());
            }
            return self.with_platform_account(target.site().name(), params, true, |rt| {
                rt.web_read(target.site(), method, params, Some(&target), None, None, 0)
            });
        }
        let site = match params["platform"].as_str() {
            Some("reddit") => Site::Reddit,
            Some("discord") => Site::Discord,
            _ => return Err("select a website platform".into()),
        };
        self.with_platform_account(site.name(), params, true, |rt| {
            rt.web_read(site, method, params, None, None, None, 0)
        })
    }
    fn web_read(
        &mut self,
        site: Site,
        method: &str,
        params: &Value,
        target: Option<&Target>,
        after: Option<&str>,
        snapshot: Option<&str>,
        offset: usize,
    ) -> Result<Value> {
        let scope = self.scope(platform(site));
        let count = number(params, "limit", 10)?;
        if !(1..=20).contains(&count) {
            return Err("website list limit must be 1..20".into());
        }
        if method == "social.profile" {
            return self.web_operator_reuse(site.name());
        }
        let supported = match site {
            Site::Reddit => matches!(
                method,
                "social.discover" | "social.open" | "social.discussion"
            ),
            Site::Discord => matches!(method, "social.chats" | "social.messages" | "social.open"),
        };
        if !supported {
            return Ok(
                json!({"status":"unsupported","platform":site.name(),"detail":"This website adapter currently provides account verification and bounded reads. No write was attempted."}),
            );
        }
        let saved =
            saved(&self.state, site, &self.account)?.ok_or("website authentication required")?;
        let client = platform_web::Client::new(saved.session);
        if let Some(snapshot) = snapshot {
            let (_, path) = self.store.resolve(&scope, snapshot)?;
            let rows: Vec<Value> = serde_json::from_slice(&read_bounded(&path, 2 * 1024 * 1024)?)
                .map_err(|_| "invalid website list snapshot")?;
            return self.web_discord_page(
                &scope,
                method,
                target,
                &rows,
                offset,
                count as usize,
                Some(snapshot),
            );
        }
        match site {
            Site::Reddit => {
                if method == "social.discover" {
                    if params.get("source").is_some_and(|v| v != "recommended") {
                        return Ok(
                            json!({"status":"unsupported","detail":"Reddit currently provides the signed-in homepage feed."}),
                        );
                    }
                    let raw = client
                        .reddit_home(after, count)
                        .map_err(|e| error(site, e))?;
                    let rows = raw
                        .pointer("/data/children")
                        .and_then(Value::as_array)
                        .ok_or("Reddit listing schema changed")?;
                    let mut items = Vec::new();
                    for row in rows.iter().take(count as usize) {
                        if row["kind"] == "t3" {
                            let mut item = self.web_reddit_post(&row["data"])?;
                            if let Some(text) = item["text"].as_str() {
                                let truncated =
                                    text.chars().count() > 640 || item["text_truncated"] == true;
                                item["text"] = json!(text.chars().take(640).collect::<String>());
                                item["text_truncated"] = json!(truncated);
                            }
                            items.push(item)
                        }
                    }
                    let next = raw
                        .pointer("/data/after")
                        .and_then(Value::as_str)
                        .filter(|s| platform_web::fullname(s).is_ok());
                    let continuation = next
                        .map(|after| {
                            self.locators.put(
                                &scope,
                                CURSOR,
                                &Cursor {
                                    site,
                                    action: method.into(),
                                    target: None,
                                    after: Some(after.into()),
                                    snapshot: None,
                                    offset: 0,
                                },
                            )
                        })
                        .transpose()?;
                    return Ok(
                        json!({"status":"ok","scope":scope,"items":items,"continuation":continuation,"has_more":next.is_some(),"media_downloaded":false}),
                    );
                }
                let Some(Target::RedditPost { post }) = target else {
                    return Err("select a returned Reddit post target".into());
                };
                let raw = client
                    .reddit_thread(post, count)
                    .map_err(|e| error(site, e))?;
                let post_raw = raw
                    .pointer("/0/data/children/0/data")
                    .ok_or("Reddit post response changed")?;
                let content = self.web_reddit_post(post_raw)?;
                if content["id"] != *post {
                    return Err("Reddit post identity mismatch".into());
                }
                let mut comments = Vec::new();
                let mut more = false;
                let rows = raw
                    .pointer("/1/data/children")
                    .and_then(Value::as_array)
                    .ok_or("Reddit comments response changed")?;
                fn walk(
                    rows: &[Value],
                    out: &mut Vec<Value>,
                    more: &mut bool,
                    limit: usize,
                    depth: u8,
                ) {
                    for row in rows {
                        if out.len() >= limit {
                            *more = true;
                            break;
                        }
                        if row["kind"] == "more" {
                            *more = true;
                            continue;
                        }
                        if row["kind"] != "t1" {
                            continue;
                        }
                        let d = &row["data"];
                        out.push(json!({"id":d.get("id"),"parent_id":d.get("parent_id"),"author":platform_web::text(&d["author"],128),"text":platform_web::text(&d["body"],1600),"text_truncated":d["body"].as_str().is_some_and(|s|s.chars().count()>1600),"score":d.get("score"),"created_at":d.get("created_utc")}));
                        if depth < 3 {
                            if let Some(children) = d
                                .pointer("/replies/data/children")
                                .and_then(Value::as_array)
                            {
                                walk(children, out, more, limit, depth + 1)
                            }
                        }
                    }
                }
                walk(rows, &mut comments, &mut more, count as usize, 0);
                Ok(
                    json!({"status":"ok","scope":scope,"content":content,"items":comments,"has_more":more,"continuation":null,"pagination_supported":false,"media_downloaded":false}),
                )
            }
            Site::Discord => {
                if method == "social.messages"
                    || matches!(target, Some(Target::DiscordChannel { .. }))
                {
                    let Some(Target::DiscordChannel { id }) = target else {
                        return Err("select a Discord channel/chat target".into());
                    };
                    let rows = client
                        .discord_messages(id, after, count)
                        .map_err(|e| error(site, e))?;
                    let rows = rows
                        .as_array()
                        .filter(|v| v.len() <= 100)
                        .ok_or("Discord messages response changed")?;
                    let mut items = Vec::new();
                    for row in rows.iter().take(count as usize) {
                        let mut item = json!({"id":row.get("id"),"author":{"id":row.pointer("/author/id"),"name":platform_web::text(row.pointer("/author/global_name").filter(|v|v.is_string()).or_else(||row.pointer("/author/username")).unwrap_or(&Value::Null),128)},"text":platform_web::text(&row["content"],1600),"text_truncated":row["content"].as_str().is_some_and(|s|s.chars().count()>1600),"created_at":row.get("timestamp"),"edited_at":row.get("edited_timestamp"),"reply_to":row.pointer("/message_reference/message_id")});
                        let mut media = Vec::new();
                        if let Some(attachments) = row["attachments"].as_array() {
                            for a in attachments.iter().take(10) {
                                if let Some(url) = a["url"]
                                    .as_str()
                                    .filter(|u| platform_web::media_url(site, u))
                                {
                                    media.push(json!({"name":platform_web::text(&a["filename"],256),"mime":a.get("content_type"),"media_ref":self.locators.put(&scope,media_kind(site),&Media{url:url.into()})?}));
                                }
                            }
                        }
                        item["media"] = json!(media);
                        items.push(item);
                    }
                    let next = if rows.len() == count as usize {
                        rows.last()
                            .and_then(|row| row["id"].as_str())
                            .filter(|s| platform_web::id(site, s).is_ok())
                    } else {
                        None
                    };
                    if next == after && next.is_some() {
                        return Err("Discord history cursor did not advance".into());
                    }
                    let continuation = next
                        .map(|next| {
                            self.locators.put(
                                &scope,
                                CURSOR,
                                &Cursor {
                                    site,
                                    action: if method == "social.open" {
                                        "social.messages".into()
                                    } else {
                                        method.into()
                                    },
                                    target: target.cloned(),
                                    after: Some(next.into()),
                                    snapshot: None,
                                    offset: 0,
                                },
                            )
                        })
                        .transpose()?;
                    return Ok(
                        json!({"status":"ok","scope":scope,"items":items,"continuation":continuation,"next_tool":"social.messages","has_more":next.is_some(),"media_downloaded":false}),
                    );
                }
                let raw = match target {
                    Some(Target::DiscordGuild { id }) => client.discord_channels(id),
                    None if params["kind"] == "guilds" || params["kind"] == "groups" => {
                        client.discord_guilds()
                    }
                    None => client.discord_chats(),
                    _ => return Err("unsupported Discord selection".into()),
                }
                .map_err(|e| error(site, e))?;
                let raw = raw
                    .as_array()
                    .filter(|v| v.len() <= 1000)
                    .ok_or("Discord channel list response changed")?;
                let mut rows = Vec::new();
                for row in raw {
                    let guild = target.is_none()
                        && matches!(params["kind"].as_str(), Some("guilds" | "groups"));
                    let id = row["id"].as_str().ok_or("Discord list identity missing")?;
                    platform_web::id(site, id).map_err(|e| error(site, e))?;
                    let recipients=row["recipients"].as_array().map(|v|v.iter().take(20).map(|u|json!({"id":u.get("id"),"name":u.get("global_name").filter(|v|v.is_string()).or_else(||u.get("username"))})).collect::<Vec<_>>()).unwrap_or_default();
                    rows.push(json!({"id":id,"name":platform_web::text(&row["name"],256),"kind":if guild{"guild"}else{"channel"},"type":row.get("type"),"recipients":recipients,"position":row.get("position")}));
                }
                self.web_discord_page(&scope, method, target, &rows, 0, count as usize, None)
            }
        }
    }
    fn web_discord_page(
        &self,
        scope: &Scope,
        method: &str,
        target: Option<&Target>,
        rows: &[Value],
        offset: usize,
        count: usize,
        snapshot: Option<&str>,
    ) -> Result<Value> {
        if rows.len() > 1000 || offset > rows.len() {
            return Err("website list snapshot exceeds bounds".into());
        }
        let end = (offset + count).min(rows.len());
        let mut items = Vec::new();
        for row in &rows[offset..end] {
            let mut row = row.clone();
            let id = row["id"]
                .as_str()
                .ok_or("Discord item identity missing")?
                .into();
            let target = if row["kind"] == "guild" {
                Target::DiscordGuild { id }
            } else {
                Target::DiscordChannel { id }
            };
            target.validate()?;
            row["target_ref"] = json!(self.locators.put(scope, TARGET, &target)?);
            items.push(row);
        }
        let continuation = if end < rows.len() {
            let snapshot = match snapshot {
                Some(v) => v.to_owned(),
                None => {
                    let bytes =
                        serde_json::to_vec(rows).map_err(|_| "cannot encode website list")?;
                    self.store
                        .import(
                            scope,
                            "website-list",
                            "application/json",
                            &mut bytes.as_slice(),
                        )?
                        .id
                }
            };
            Some(self.locators.put(
                scope,
                CURSOR,
                &Cursor {
                    site: Site::Discord,
                    action: if method == "social.open" {
                        "social.chats".into()
                    } else {
                        method.into()
                    },
                    target: target.cloned(),
                    after: None,
                    snapshot: Some(snapshot),
                    offset: end,
                },
            )?)
        } else {
            None
        };
        Ok(
            json!({"status":"ok","scope":scope,"items":items,"continuation":continuation,"next_tool":"social.chats","has_more":end<rows.len(),"provider_list_may_be_capped":rows.len()>=100&&rows.iter().all(|r|r["kind"]=="guild"),"read_only":true}),
        )
    }
    fn web_reddit_post(&self, raw: &Value) -> Result<Value> {
        let mut item = platform_web::reddit_post(raw).map_err(|e| error(Site::Reddit, e))?;
        let post = item["id"].as_str().ok_or("Reddit post ID missing")?.into();
        let scope = self.scope(Platform::Reddit);
        item["target_ref"] = json!(self.locators.put(
            &scope,
            TARGET,
            &Target::RedditPost { post }
        )?);
        let mut media = Vec::new();
        for value in [raw.get("url_overridden_by_dest"), raw.get("thumbnail")]
            .into_iter()
            .flatten()
        {
            if let Some(url) = value
                .as_str()
                .filter(|u| platform_web::media_url(Site::Reddit, u))
            {
                media.push(json!({"media_ref":self.locators.put(&scope,media_kind(Site::Reddit),&Media{url:url.into()})?}));
            }
        }
        item["media"] = json!(media);
        Ok(item)
    }
    fn web_download(&mut self, site: Site, reference: &str) -> Result<Value> {
        let scope = self.locators.scope_for(media_kind(site), reference)?;
        if scope != self.scope(platform(site)) {
            return Err("website media belongs to another account".into());
        }
        let media: Media = self.locators.get(&scope, media_kind(site), reference)?;
        let saved =
            saved(&self.state, site, &self.account)?.ok_or("website authentication required")?;
        let path = self
            .state
            .join("work")
            .join(format!("web-media-{reference}"));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .map_err(|_| "cannot create website media")?;
            let mime = platform_web::Client::new(saved.session)
                .download(&media.url, 64 * 1024 * 1024, &mut file)
                .map_err(|e| error(site, e))?;
            let group = if mime.starts_with("video/") {
                "video"
            } else if mime.starts_with("audio/") {
                "audio"
            } else {
                "image"
            };
            let mime = download_mime(&path, Some(&mime), group)?;
            let resource = self.store.import(
                &scope,
                "website-media",
                &mime,
                &mut File::open(&path).map_err(|_| "cannot read website media")?,
            )?;
            Ok(json!({"resource":resource,"media_observed":false}))
        })();
        let _ = fs::remove_file(path);
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn website_tools_keep_account_platform_media_and_pagination_boundaries() {
        let state = std::env::temp_dir().join(format!("web-tool-ownership-{}", std::process::id()));
        private_dir(&state).unwrap();
        let mut rt = Runtime::open(&state, "public", None).unwrap();
        for (site, target, url) in [
            (
                Site::Reddit,
                Target::RedditPost {
                    post: "abc123".into(),
                },
                "https://i.redd.it/synthetic.png",
            ),
            (
                Site::Discord,
                Target::DiscordChannel { id: "123".into() },
                "https://cdn.discordapp.com/attachments/123/456/synthetic.png",
            ),
        ] {
            let scope = Scope {
                platform: platform(site),
                account: "alice".into(),
            };
            let reference = rt.locators.put(&scope, TARGET, &target).unwrap();
            let media = rt
                .locators
                .put(&scope, media_kind(site), &Media { url: url.into() })
                .unwrap();
            assert_eq!(rt.media_reference_scope(&media).unwrap(), scope);
            for method in ["social.open", "social.like"] {
                assert!(
                    rt.call(method, &json!({"target_ref":reference,"account":"bob"}))
                        .unwrap_err()
                        .contains("account differs")
                );
                assert!(
                    rt.call(
                        method,
                        &json!({"target_ref":reference,"platform":"bilibili"})
                    )
                    .unwrap_err()
                    .contains("platform differs")
                );
            }
            assert!(
                rt.call("media.read", &json!({"reference":media,"account":"bob"}))
                    .unwrap_err()
                    .contains("account differs")
            );
            assert_eq!(
                rt.call("social.like", &json!({"target_ref":reference}))
                    .unwrap()["status"],
                "unsupported"
            );
            let cursor = rt
                .locators
                .put(
                    &scope,
                    CURSOR,
                    &Cursor {
                        site,
                        action: "social.chats".into(),
                        target: None,
                        after: None,
                        snapshot: None,
                        offset: 0,
                    },
                )
                .unwrap();
            assert!(
                rt.call(
                    "social.chats",
                    &json!({"continuation":cursor,"account":"bob"})
                )
                .unwrap_err()
                .contains("account differs")
            );
            assert!(
                rt.call(
                    "social.chats",
                    &json!({"continuation":cursor,"platform":"bilibili"})
                )
                .unwrap_err()
                .contains("platform differs")
            );
            assert!(
                rt.call("social.messages", &json!({"continuation":cursor}))
                    .unwrap_err()
                    .contains("continuation kind/platform mismatch")
            );
        }
        assert!(
            rt.call(
                "social.open",
                &json!({"reference":"https://discord.com/channels/@me/123","platform":"reddit"})
            )
            .unwrap_err()
            .contains("platform differs")
        );
        let scope = Scope {
            platform: Platform::Discord,
            account: "alice".into(),
        };
        let rows = (1..=23)
            .map(|n| json!({"id":n.to_string(),"kind":"channel","name":format!("chat {n}")}))
            .collect::<Vec<_>>();
        let page = rt
            .web_discord_page(&scope, "social.open", None, &rows, 0, 10, None)
            .unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 10);
        assert_eq!(page["next_tool"], "social.chats");
        let cursor: Cursor = rt
            .locators
            .get(&scope, CURSOR, page["continuation"].as_str().unwrap())
            .unwrap();
        assert_eq!(cursor.action, "social.chats");
        let (_, path) = rt
            .store
            .resolve(&scope, cursor.snapshot.as_deref().unwrap())
            .unwrap();
        let cached: Vec<Value> =
            serde_json::from_slice(&read_bounded(&path, 32768).unwrap()).unwrap();
        let last = rt
            .web_discord_page(
                &scope,
                "social.chats",
                None,
                &cached,
                20,
                10,
                cursor.snapshot.as_deref(),
            )
            .unwrap();
        assert_eq!(last["items"].as_array().unwrap().len(), 3);
        assert_eq!(last["has_more"], false);
        assert!(!state.join("outbox").exists());
        drop(rt);
        fs::remove_dir_all(state).unwrap();
    }
    #[test]
    fn website_links_are_exact_objects_not_arbitrary_endpoints() {
        assert!(matches!(
            parse_reference("https://www.reddit.com/r/rust/comments/abc123/title/").unwrap(),
            Target::RedditPost { .. }
        ));
        assert!(matches!(
            parse_reference("https://discord.com/channels/@me/123").unwrap(),
            Target::DiscordChannel { .. }
        ));
        for url in [
            "https://discord.com.evil/channels/123/456",
            "https://discord.com/api/v10/users/@me",
            "https://www.reddit.com/api/me.json",
            "https://www.reddit.com/r/rust/comments/abc?evil=/api/write",
        ] {
            if url.contains("comments/abc?") {
                assert!(parse_reference(url).is_ok())
            } else {
                assert!(parse_reference(url).is_err())
            }
        }
    }
}
