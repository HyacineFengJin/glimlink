// SPDX-License-Identifier: MIT
//! Account-selected Discord Highlights notifications. They are labelled as
//! notifications, never advertised as the Discord homepage or server discovery.
use super::*;
use platform_web::discord_discovery::{Message, Recommendation};
use std::collections::BTreeSet;

const HIGHLIGHT_CURSOR: &str = "task-discord-highlights-continuation";
const HIGHLIGHT_TARGET: &str = "task-discord-highlight-target";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HighlightCursor {
    source: String,
    viewer: String,
    started: bool,
    after: Option<String>,
    pending: Vec<Recommendation>,
    seen: BTreeSet<String>,
    pages: u32,
    empty_pages: u32,
    provider_has_more: bool,
}
impl HighlightCursor {
    fn validate(&self) -> Result<()> {
        if self.source != "recommended"
            || self.pending.len() > 20
            || self.seen.len() > 512
            || self.pages > 32
            || self.empty_pages > 2
        {
            return Err("invalid Discord Highlights continuation".into());
        }
        platform_web::id(Site::Discord, &self.viewer)
            .map_err(|_| "invalid Discord Highlights viewer")?;
        for id in self.seen.iter().chain(self.after.iter()) {
            platform_web::id(Site::Discord, id).map_err(|_| "invalid Discord Highlights cursor")?;
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HighlightTarget {
    viewer: String,
    notification: Recommendation,
}
fn allowed(params: &Value, names: &[&str]) -> Result<()> {
    if params
        .as_object()
        .is_none_or(|v| v.keys().any(|key| !names.contains(&key.as_str())))
    {
        return Err("unsupported Discord Highlights parameter".into());
    }
    if params.get("platform").is_some_and(|v| v != "discord") {
        return Err("Discord Highlights platform mismatch".into());
    }
    Ok(())
}
impl Runtime {
    pub(in crate::app) fn discord_discover_matches(&self, method: &str, params: &Value) -> bool {
        if !method.starts_with("social.") {
            return false;
        }
        (method == "social.discover" && params["platform"] == "discord")
            || params["continuation"]
                .as_str()
                .is_some_and(|r| self.locators.scope_for(HIGHLIGHT_CURSOR, r).is_ok())
            || params["reference"]
                .as_str()
                .is_some_and(|r| self.locators.scope_for(HIGHLIGHT_TARGET, r).is_ok())
    }
    pub(in crate::app) fn call_discord_discover(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<Value> {
        match self.discord_highlights_inner(method, params) {
            Err(error) if error.starts_with("web-request:discord:") => Ok(
                json!({"status":error.trim_start_matches("web-request:discord:"),"platform":"discord","automatic_retry":false}),
            ),
            value => value,
        }
    }
    fn discord_highlights_identity(&self, viewer: Option<&str>) -> Result<Saved> {
        let saved = saved(&self.state, Site::Discord, &self.account)?
            .ok_or("web-request:discord:needs_auth")?;
        if viewer.is_some_and(|id| id != saved.user.id) {
            return Err("Discord Highlights viewer differs from the saved account".into());
        }
        Ok(saved)
    }
    fn discord_highlights_client(&self, viewer: &str) -> Result<platform_web::Client> {
        let saved = self.discord_highlights_identity(Some(viewer))?;
        let client = platform_web::Client::new(saved.session);
        let actual = client.me().map_err(|e| error(Site::Discord, e))?;
        if actual.id != viewer {
            return Err("Discord Highlights authenticated viewer mismatch".into());
        }
        Ok(client)
    }
    fn discord_highlights_inner(&mut self, method: &str, params: &Value) -> Result<Value> {
        if method == "social.open" {
            allowed(params, &["platform", "account", "reference"])?;
            let reference = required_str(params, "reference")?;
            let scope = self.locators.scope_for(HIGHLIGHT_TARGET, reference)?;
            tasks::check_account(params, &scope)?;
            if scope.platform != Platform::Discord {
                return Err("Discord Highlights target platform mismatch".into());
            }
            let target: HighlightTarget = self.locators.get(&scope, HIGHLIGHT_TARGET, reference)?;
            return self.with_task_scope(&scope, |runtime| runtime.discord_highlight_open(target));
        }
        if method != "social.discover" {
            return Ok(
                json!({"status":"unsupported","platform":"discord","detail":"This reference selects a personalized Highlights notification; use social_open to read it."}),
            );
        }
        allowed(
            params,
            &["platform", "account", "source", "continuation", "limit"],
        )?;
        if params.get("source").is_some_and(|v| v != "recommended") {
            return Ok(
                json!({"status":"unsupported","platform":"discord","available_sources":["recommended"],"recommendation_kind":"personalized_notifications","detail":"Discord Highlights are account-selected notifications; joined servers or DMs are not substituted."}),
            );
        }
        let count = number(params, "limit", 10)?;
        if !(1..=20).contains(&count) {
            return Err("Discord Highlights limit must be 1..20".into());
        }
        if let Some(reference) = params.get("continuation") {
            let reference = reference
                .as_str()
                .ok_or("Discord continuation must be a string")?;
            let scope = self.locators.scope_for(HIGHLIGHT_CURSOR, reference)?;
            tasks::check_account(params, &scope)?;
            if scope.platform != Platform::Discord {
                return Err("Discord Highlights cursor platform mismatch".into());
            }
            let cursor: HighlightCursor = self.locators.get(&scope, HIGHLIGHT_CURSOR, reference)?;
            cursor.validate()?;
            return self.with_task_scope(&scope, |runtime| {
                runtime.discord_highlights_page(cursor, count as usize)
            });
        }
        tasks::check_account(params, &self.scope(Platform::Discord))?;
        let saved = self.discord_highlights_identity(None)?;
        self.discord_highlights_page(
            HighlightCursor {
                source: "recommended".into(),
                viewer: saved.user.id,
                started: false,
                after: None,
                pending: Vec::new(),
                seen: BTreeSet::new(),
                pages: 0,
                empty_pages: 0,
                provider_has_more: false,
            },
            count as usize,
        )
    }
    fn discord_highlight_message_value(&self, message: &Message) -> Result<Value> {
        let scope = self.scope(Platform::Discord);
        let mut media = Vec::new();
        for attachment in message.attachments.iter().take(10) {
            if !platform_web::media_url(Site::Discord, &attachment.url) {
                return Err("invalid Discord Highlights media host".into());
            }
            let media_ref = self.locators.put_reusable(
                &scope,
                media_kind(Site::Discord),
                &Media {
                    url: attachment.url.clone(),
                },
            )?;
            media.push(json!({"name":attachment.name,"mime":attachment.mime_type,"media_ref":media_ref,"read_tool":"media_read","fetched":false}));
        }
        Ok(
            json!({"id":message.id,"channel_id":message.channel_id,"guild_id":message.guild_id,"author":message.author,"text":message.text,"text_truncated":message.text_truncated,"media":media,"media_downloaded":false}),
        )
    }
    fn discord_highlight_value(
        &self,
        notification: &Recommendation,
        viewer: &str,
    ) -> Result<Value> {
        platform_web::id(Site::Discord, &notification.id)
            .map_err(|_| "invalid Discord Highlights notification identity")?;
        if !matches!(
            notification.notification_type.as_str(),
            "missed_messages" | "top_messages"
        ) {
            return Err("Discord notification is not a known personalized highlight".into());
        }
        let target = self.locators.put_reusable(
            &self.scope(Platform::Discord),
            HIGHLIGHT_TARGET,
            &HighlightTarget {
                viewer: viewer.into(),
                notification: notification.clone(),
            },
        )?;
        let text = notification
            .message_excerpt
            .as_deref()
            .unwrap_or(&notification.body);
        let mut item = json!({"id":notification.id,"kind":"personalized_notification","notification_type":notification.notification_type,"deprecated_type":notification.deprecated_type,"text":text,"text_origin":if notification.message_excerpt.is_some(){"notification_message_excerpt"}else{"notification_summary"},"summary":notification.body,"guild_id":notification.guild_id,"channel_id":notification.channel_id,"message_id":notification.message_id,"attachment_count":notification.attachment_count,"embed_count":notification.embed_count,"target_ref":target,"open_tool":"social_open","message_open_required":notification.message.is_none()&&notification.channel_id.is_some()&&notification.message_id.is_some(),"message_reference_available":notification.channel_id.is_some()&&notification.message_id.is_some(),"message_detail_available":notification.message.is_some(),"media_downloaded":false});
        if let Some(message) = &notification.message {
            item["message"] = self.discord_highlight_message_value(message)?;
        }
        Ok(item)
    }
    fn discord_highlight_open(&self, mut target: HighlightTarget) -> Result<Value> {
        self.discord_highlights_identity(Some(&target.viewer))?;
        // Notification messages can be abbreviated. Opening a concrete message
        // fetches that exact message, including its lazy attachment references.
        {
            if let (Some(channel), Some(selected)) = (
                &target.notification.channel_id,
                &target.notification.message_id,
            ) {
                let client = self.discord_highlights_client(&target.viewer)?;
                let message = client
                    .discord_recommended_message(channel, selected)
                    .map_err(|e| error(Site::Discord, e))?;
                if target.notification.guild_id.is_some()
                    && message.guild_id.is_some()
                    && target.notification.guild_id != message.guild_id
                {
                    return Err("Discord recommended message guild mismatch".into());
                }
                target.notification.message = Some(message);
            }
        }
        Ok(
            json!({"status":"ok","scope":self.scope(Platform::Discord),"source":"discord_personalized_highlights_notifications","content":self.discord_highlight_value(&target.notification,&target.viewer)?,"home_timeline":false,"recommendation_kind":"personalized_notifications","media_downloaded":false}),
        )
    }
    fn discord_highlights_page(
        &mut self,
        mut cursor: HighlightCursor,
        count: usize,
    ) -> Result<Value> {
        cursor.validate()?;
        self.discord_highlights_identity(Some(&cursor.viewer))?;
        let mut filtered_items = 0;
        if cursor.pending.is_empty() && (!cursor.started || cursor.after.is_some()) {
            if cursor.pages >= 32 || cursor.seen.len() >= 512 || cursor.empty_pages >= 2 {
                return Err("Discord Highlights continuation budget exhausted".into());
            }
            let client = self.discord_highlights_client(&cursor.viewer)?;
            let page = client
                .discord_recommendations(cursor.after.as_deref(), 20)
                .map_err(|e| error(Site::Discord, e))?;
            cursor.started = true;
            cursor.pages += 1;
            cursor.provider_has_more = page.has_more;
            cursor.after = page.next_cursor;
            filtered_items = page.filtered_items;
            cursor.pending = page.items;
        }
        let mut consumed = 0;
        let mut items = Vec::new();
        for notification in &cursor.pending {
            if items.len() >= count || cursor.seen.len() >= 512 {
                break;
            }
            consumed += 1;
            if cursor.seen.insert(notification.id.clone()) {
                items.push(self.discord_highlight_value(notification, &cursor.viewer)?)
            }
        }
        cursor.pending.drain(..consumed);
        if items.is_empty() {
            cursor.empty_pages += 1
        } else {
            cursor.empty_pages = 0
        }
        let more = (!cursor.pending.is_empty() || (cursor.after.is_some() && cursor.pages < 32))
            && cursor.seen.len() < 512
            && cursor.empty_pages < 2;
        let continuation = if more {
            Some(
                self.locators
                    .put(&self.scope(Platform::Discord), HIGHLIGHT_CURSOR, &cursor)?,
            )
        } else {
            None
        };
        Ok(
            json!({"status":"ok","scope":self.scope(Platform::Discord),"source":"discord_personalized_highlights_notifications","requested_source":"recommended","recommendation_kind":"personalized_notifications","home_timeline":false,"signed_in_request":true,"viewer_verified":true,"personalization_verified":false,"context":{"source":"discord_personalized_highlights_notifications","signed_in_request":true,"personalization_verified":false,"personalization_basis":"provider_selected_notifications_from_frequently_read_channels_or_guilds"},"items":items,"continuation":continuation,"has_more":more,"provider_has_more":cursor.provider_has_more,"filtered_non_recommendation_notifications":filtered_items,"media_downloaded":false,"marks_read":false,"stop_reason":if more{"page_complete"}else if cursor.empty_pages>=2{"no_matching_highlights"}else if cursor.pages>=32||cursor.seen.len()>=512{"session_budget"}else{"provider_end"}}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lab() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "discord-highlights-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        private_dir(&root).unwrap();
        root
    }
    fn notification(id: &str) -> Recommendation {
        Recommendation {
            id: id.into(),
            notification_type: "missed_messages".into(),
            deprecated_type: false,
            body: "Synthetic selected conversation".into(),
            guild_id: None,
            channel_id: None,
            message_id: None,
            message_excerpt: None,
            message: None,
            attachment_count: Some(0),
            embed_count: Some(0),
        }
    }
    #[test]
    fn cached_highlights_keep_viewer_source_and_target_scopes_without_network_or_ack() {
        let root = lab();
        let mut runtime = Runtime::open(&root, "owner", None).unwrap();
        let record = Saved {
            schema: 1,
            user: User {
                id: "42".into(),
                name: "Fixture".into(),
            },
            session: platform_web::Session::from_private_input(
                Site::Discord,
                r#"{"token":"synthetic-private"}"#,
            )
            .unwrap(),
        };
        let dir = account_directory(&root, "discord", "owner").unwrap();
        save_private_new(
            &dir.join("session.json"),
            &serde_json::to_value(record).unwrap(),
        )
        .unwrap();
        let cursor = HighlightCursor {
            source: "recommended".into(),
            viewer: "42".into(),
            started: true,
            after: None,
            pending: vec![notification("111"), notification("112")],
            seen: BTreeSet::new(),
            pages: 1,
            empty_pages: 0,
            provider_has_more: false,
        };
        let first = runtime.discord_highlights_page(cursor, 1).unwrap();
        assert_eq!(first["items"].as_array().unwrap().len(), 1);
        assert_eq!(first["home_timeline"], false);
        assert_eq!(first["marks_read"], false);
        let next = first["continuation"].as_str().unwrap();
        assert!(
            runtime
                .call_discord_discover(
                    "social.discover",
                    &json!({"continuation":next,"account":"other"})
                )
                .unwrap_err()
                .contains("account differs")
        );
        assert_eq!(
            runtime
                .call_discord_discover(
                    "social.discover",
                    &json!({"continuation":next,"source":"following"})
                )
                .unwrap()["status"],
            "unsupported"
        );
        let second = runtime
            .call_discord_discover("social.discover", &json!({"continuation":next}))
            .unwrap();
        assert_eq!(second["items"][0]["id"], "112");
        assert_eq!(second["has_more"], false);
        let target = first["items"][0]["target_ref"].as_str().unwrap();
        assert!(runtime.discord_discover_matches("social.open", &json!({"reference":target})));
        let opened = runtime
            .call_discord_discover("social.open", &json!({"reference":target}))
            .unwrap();
        assert_eq!(opened["content"]["kind"], "personalized_notification");
        assert_eq!(opened["content"]["message_open_required"], false);
        assert_eq!(opened["content"]["message_reference_available"], false);
        assert!(
            !serde_json::to_string(&opened)
                .unwrap()
                .contains("synthetic-private")
        );
        assert!(!root.join("outbox").exists());
        let mut changed: Saved = saved(&root, Site::Discord, "owner").unwrap().unwrap();
        changed.user.id = "99".into();
        fs::remove_file(dir.join("session.json")).unwrap();
        save_private_new(
            &dir.join("session.json"),
            &serde_json::to_value(changed).unwrap(),
        )
        .unwrap();
        assert!(
            runtime
                .call_discord_discover("social.discover", &json!({"continuation":next}))
                .unwrap_err()
                .contains("viewer differs")
        );
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }
}
