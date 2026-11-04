// SPDX-License-Identifier: MIT
//! Personalized Discord Highlights notifications, not Server Discovery or a
//! list of joined guilds. Fixed read-only routes; no acknowledgements or Gateway.
//! Protocol reference: discord-userdoccers/discord-userdoccers,
//! pages/resources/notification-center.mdx at f0f8a1fe3824ce31fee1848465c0d408d9980d30.
use super::{Client, Error, Result, Site, User, id, media_url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub url: String,
    pub mime_type: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub channel_id: String,
    pub guild_id: Option<String>,
    pub text: String,
    pub text_truncated: bool,
    pub author: Option<User>,
    pub attachments: Vec<Attachment>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recommendation {
    pub id: String,
    pub notification_type: String,
    pub deprecated_type: bool,
    pub body: String,
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub message_id: Option<String>,
    pub message_excerpt: Option<String>,
    pub message: Option<Message>,
    pub attachment_count: Option<u64>,
    pub embed_count: Option<u64>,
}
#[derive(Clone, Debug)]
pub struct RecommendationPage {
    pub items: Vec<Recommendation>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub filtered_items: usize,
}
fn checked_text(value: &Value, max_chars: usize) -> Result<String> {
    let value = value.as_str().ok_or(Error::ProtocolChanged)?;
    if value.len() > 128 * 1024
        || value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(Error::Limit);
    }
    Ok(value.chars().take(max_chars).collect())
}
fn optional_id(value: Option<&Value>) -> Result<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let value = value.as_str().ok_or(Error::ProtocolChanged)?;
            id(Site::Discord, value).map_err(|_| Error::ProtocolChanged)?;
            Ok(Some(value.into()))
        }
    }
}
fn agree(a: Option<String>, b: Option<String>) -> Result<Option<String>> {
    if a.is_some() && b.is_some() && a != b {
        return Err(Error::ProtocolChanged);
    }
    Ok(a.or(b))
}
fn message(
    raw: &Value,
    expected_channel: Option<&str>,
    expected_id: Option<&str>,
) -> Result<Message> {
    let message_id = optional_id(raw.get("id"))?.ok_or(Error::ProtocolChanged)?;
    let channel = optional_id(raw.get("channel_id"))?.ok_or(Error::ProtocolChanged)?;
    if expected_id.is_some_and(|v| v != message_id)
        || expected_channel.is_some_and(|v| v != channel)
    {
        return Err(Error::ProtocolChanged);
    }
    let content = raw.get("content").ok_or(Error::ProtocolChanged)?;
    let text = checked_text(content, 6400)?;
    let author = match raw.get("author") {
        None | Some(Value::Null) => None,
        Some(author) => {
            let user = User {
                id: optional_id(author.get("id"))?.ok_or(Error::ProtocolChanged)?,
                name: checked_text(
                    author
                        .get("global_name")
                        .filter(|v| v.is_string())
                        .or_else(|| author.get("username"))
                        .ok_or(Error::ProtocolChanged)?,
                    128,
                )?,
            };
            user.validate(Site::Discord)?;
            Some(user)
        }
    };
    let mut attachments = Vec::new();
    if let Some(rows) = raw.get("attachments") {
        let rows = rows
            .as_array()
            .filter(|v| v.len() <= 100)
            .ok_or(Error::ProtocolChanged)?;
        for row in rows.iter().take(10) {
            let url = row["url"].as_str().ok_or(Error::ProtocolChanged)?;
            if !media_url(Site::Discord, url) {
                continue;
            }
            attachments.push(Attachment {
                name: checked_text(&row["filename"], 256)?,
                url: url.into(),
                mime_type: row["content_type"]
                    .as_str()
                    .map(|v| v.chars().take(128).collect()),
            });
        }
    }
    Ok(Message {
        id: message_id,
        channel_id: channel,
        guild_id: optional_id(raw.get("guild_id"))?,
        text,
        text_truncated: content.as_str().is_some_and(|v| v.chars().count() > 6400),
        author,
        attachments,
    })
}
pub fn parse_recommendations(
    raw: &Value,
    previous: Option<&str>,
    requested: u64,
) -> Result<RecommendationPage> {
    if !(1..=20).contains(&requested) {
        return Err(Error::InvalidInput);
    }
    let rows = raw["items"].as_array().ok_or(Error::ProtocolChanged)?;
    if raw["limit"].as_u64() != Some(requested) || rows.len() > requested as usize {
        return Err(Error::ProtocolChanged);
    }
    let has_more = raw["has_more"].as_bool().ok_or(Error::ProtocolChanged)?;
    if raw.get("cursor").is_none() {
        return Err(Error::ProtocolChanged);
    }
    let next_cursor = optional_id(raw.get("cursor"))?;
    if has_more && (next_cursor.is_none() || next_cursor.as_deref() == previous) {
        return Err(Error::ProtocolChanged);
    }
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    let mut filtered_items = 0;
    for row in rows {
        let notification_id = optional_id(row.get("id"))?.ok_or(Error::ProtocolChanged)?;
        let kind = row["type"]
            .as_str()
            .filter(|v| v.len() <= 128)
            .ok_or(Error::ProtocolChanged)?;
        // A notification center also contains mentions, requests, tutorials and
        // unknown trending types. Those are not evidence of recommendations.
        if !matches!(kind, "missed_messages" | "top_messages") {
            filtered_items += 1;
            continue;
        }
        if !seen.insert(notification_id.clone()) {
            continue;
        }
        let body = checked_text(&row["body"], 1600)?;
        if body.is_empty() {
            return Err(Error::ProtocolChanged);
        }
        let selected = row.get("message").filter(|v| !v.is_null());
        let channel = agree(
            optional_id(row.get("message_channel_id"))?,
            optional_id(selected.and_then(|m| m.get("channel_id")))?,
        )?;
        let message_id = agree(
            optional_id(row.get("message_id"))?,
            optional_id(selected.and_then(|m| m.get("id")))?,
        )?;
        let guild_id = agree(
            optional_id(row.get("guild_id"))?,
            optional_id(selected.and_then(|m| m.get("guild_id")))?,
        )?;
        let selected = selected
            .map(|m| message(m, channel.as_deref(), message_id.as_deref()))
            .transpose()?;
        let message_excerpt = row
            .get("message_content")
            .filter(|v| !v.is_null())
            .map(|v| checked_text(v, 1600))
            .transpose()?
            .filter(|value| !value.is_empty());
        items.push(Recommendation {
            id: notification_id,
            notification_type: kind.into(),
            deprecated_type: kind == "top_messages",
            body,
            guild_id,
            channel_id: channel,
            message_id,
            message_excerpt,
            message: selected,
            attachment_count: row["message_attachment_count"].as_u64(),
            embed_count: row["message_embed_count"].as_u64(),
        });
    }
    Ok(RecommendationPage {
        items,
        next_cursor: if has_more { next_cursor } else { None },
        has_more,
        filtered_items,
    })
}
impl Client {
    pub fn discord_recommendations(
        &self,
        after: Option<&str>,
        count: u64,
    ) -> Result<RecommendationPage> {
        if self.session.site != Site::Discord || !(1..=20).contains(&count) {
            return Err(Error::InvalidInput);
        }
        let after = match after {
            Some(value) => {
                id(Site::Discord, value)?;
                format!("&after={value}")
            }
            None => String::new(),
        };
        let raw=self.get(&format!("/users/@me/notification-center/items?limit={count}&with_mentions=false&roles_filter=true&everyone_filter=true{after}"))?;
        parse_recommendations(&raw, after.strip_prefix("&after="), count)
    }
    pub fn discord_recommended_message(&self, channel: &str, selected: &str) -> Result<Message> {
        if self.session.site != Site::Discord {
            return Err(Error::InvalidInput);
        }
        id(Site::Discord, channel)?;
        id(Site::Discord, selected)?;
        message(
            &self.get(&format!("/channels/{channel}/messages/{selected}"))?,
            Some(channel),
            Some(selected),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn account_notifications_keep_only_documented_personalized_types() {
        let row = |id: &str, kind: &str| json!({"id":id,"type":kind,"body":"Synthetic recommended conversation","message_channel_id":"42","message_id":"43","message_content":"excerpt","unknown_private":"not copied"});
        let raw = json!({"limit":10,"items":[row("10","missed_messages"),row("11","top_messages"),row("12","recent_mention"),row("13","trending_content"),row("14","friend_request_pending")],"cursor":"99","has_more":true});
        let page = parse_recommendations(&raw, None, 10).unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.filtered_items, 3);
        assert!(page.items[1].deprecated_type);
        assert_eq!(page.next_cursor.as_deref(), Some("99"));
        assert!(
            !serde_json::to_string(&page.items)
                .unwrap()
                .contains("unknown_private")
        );
        assert!(parse_recommendations(&raw, Some("99"), 10).is_err());
        for bad in [
            json!({}),
            json!({"message":"unauthorized"}),
            json!({"limit":10,"items":[],"cursor":null,"has_more":true}),
        ] {
            assert!(parse_recommendations(&bad, None, 10).is_err())
        }
    }
    #[test]
    fn conflicting_message_identity_and_foreign_media_cannot_become_targets() {
        let mut raw = json!({"limit":1,"items":[{"id":"10","type":"missed_messages","body":"Synthetic","message_channel_id":"42","message_id":"43","message":{"id":"44","channel_id":"42","content":"body"}}],"cursor":null,"has_more":false});
        assert!(parse_recommendations(&raw, None, 1).is_err());
        raw["items"][0]["message"]["id"] = json!("43");
        raw["items"][0]["message"]["attachments"] = json!([{"filename":"bad","url":"https://example.invalid/secret"},{"filename":"photo.png","url":"https://cdn.discordapp.com/attachments/42/43/photo.png","content_type":"image/png"}]);
        let page = parse_recommendations(&raw, None, 1).unwrap();
        assert_eq!(page.items[0].message.as_ref().unwrap().attachments.len(), 1);
    }
}
