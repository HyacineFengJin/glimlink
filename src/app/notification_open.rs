// SPDX-License-Identifier: MIT
//! Open an account-bound notification without asking the model to interpret
//! subject/root/source IDs or search all pages for the referenced reply.
use super::tasks::{TaskTarget, check_account};
use super::*;
use serde::{Deserialize, Serialize};

const NOTIFICATION: &str = "task-notification";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    kind: String,
    item: Value,
}

impl Runtime {
    pub(super) fn notification_reference(
        &self,
        platform: &Platform,
        kind: &str,
        item: &Value,
    ) -> Result<String> {
        self.locators.put_reusable(
            &self.scope(platform.clone()),
            NOTIFICATION,
            &Snapshot {
                schema: 1,
                kind: kind.into(),
                item: item.clone(),
            },
        )
    }

    pub(super) fn open_notification_reference(&mut self, params: &Value) -> Result<Option<Value>> {
        let reference = required_str(params, "reference")?;
        let Ok(scope) = self.locators.scope_for(NOTIFICATION, reference) else {
            return Ok(None);
        };
        check_account(params, &scope)?;
        if params
            .get("platform")
            .is_some_and(|v| Some(v) != serde_json::to_value(&scope.platform).ok().as_ref())
        {
            return Err("platform differs from the saved notification".into());
        }
        let saved: Snapshot = self.locators.get(&scope, NOTIFICATION, reference)?;
        if saved.schema != 1 || !matches!(saved.kind.as_str(), "reply" | "at" | "like") {
            return Err("unsupported notification snapshot".into());
        }
        self.with_task_scope(&scope, |rt| match scope.platform {
            Platform::Bilibili => rt.open_bili_notification(reference, saved.item),
            Platform::Tieba => rt.open_tieba_notification(reference, saved.item),
            _ => Err("notification provider is unavailable or paused".into()),
        })
        .map(Some)
    }

    fn open_bili_notification(&mut self, reference: &str, item: Value) -> Result<Value> {
        let notification: platform_bilibili::Notification =
            serde_json::from_value(item).map_err(|_| "invalid saved Bilibili notification")?;
        let mut preview = serde_json::to_value(&notification).map_err(|e| e.to_string())?;
        self.normalize_notification_media(&mut preview)?;
        let context = match self.bili.notification_context(&notification, 20) {
            Ok(context) => context,
            Err(platform_bilibili::Error::Platform(62002)) => {
                return Ok(
                    json!({"status":"partial","scope":self.scope(Platform::Bilibili),
                "kind":"notification","notification_ref":reference,"notification_kind":notification.kind,
                "notification":preview,
                "exact_target_found":false,"content":{"status":"unavailable","location":platform_bilibili::notification_location(&notification),
                "reason":"The platform refused this notification's video read (62002); no comment or video content was substituted."},
                "media_downloaded":false,"content_writes":false}),
                );
            }
            Err(error) => return Err(error.to_string()),
        };
        let has_selected = context.selected_comment.is_some();
        let selected = context.selected_comment.as_ref().map(|comment| {
            (
                comment.rpid,
                comment.root.filter(|r| *r > 0).unwrap_or(comment.rpid),
            )
        });
        let video = context.video.as_ref().map(|video| video.bvid.clone());
        let content = serde_json::to_value(&context).map_err(|e| e.to_string())?;
        let cached = self.cache_bili_comments("notification_context", content)?;
        let mut content = cached["page"]["data"].clone();
        self.annotate_task_content(&mut content, &Platform::Bilibili, video.as_deref(), None)?;
        let scope = self.scope(Platform::Bilibili);
        let mut result = json!({"status":if has_selected || context.status==platform_bilibili::NotificationContextStatus::VideoOnly{"ok"}else{"partial"},
            "scope":scope,"kind":"notification","notification_ref":reference,"notification_kind":notification.kind,
            "notification":preview,
            "content":content,"media_downloaded":false,"content_writes":false,"exact_target_found":has_selected});
        if let Some(video) = video {
            result["context_ref"] = json!(self.save_task_target(&TaskTarget::Video {
                video: video.clone()
            })?);
            if let Some((parent, root)) = selected {
                result["target_ref"] = json!(self.save_task_target(&TaskTarget::BiliComment {
                    video,
                    root,
                    parent
                })?);
            }
        }
        Ok(result)
    }

    fn open_tieba_notification(&mut self, reference: &str, item: Value) -> Result<Value> {
        let notification: platform_tieba::Notification =
            serde_json::from_value(item).map_err(|_| "invalid saved Tieba notification")?;
        let thread = notification.target.thread_id;
        if thread == 0 {
            return Err("notification thread identity missing".into());
        }
        let parent = notification.target.parent_post_id.filter(|id| *id > 0);
        let selected = notification.target.post_id.filter(|id| *id > 0);
        let scope = self.scope(Platform::Tieba);
        let context_ref = self.save_task_target(&TaskTarget::Thread {
            thread: thread.to_string(),
        })?;
        let mut params = json!({"thread":thread.to_string()});
        let route = if notification.is_comment == Some(true) {
            let Some(selected) = selected else {
                return Ok(
                    json!({"status":"partial","scope":scope,"notification_ref":reference,
                "kind":"notification","context_ref":context_ref,"exact_target_found":false,"reason":"notification has no selected comment identity"}),
                );
            };
            // A feed's quote/parent ID can itself be a subpost. Locate the
            // selected source directly; only readback establishes its root.
            params["comment"] = json!(selected);
            "tieba.comment_context"
        } else if let Some(post) = selected.or(parent) {
            params["post"] = json!(post);
            "tieba.comments"
        } else {
            return Ok(
                json!({"status":"partial","scope":scope,"kind":"notification","notification_ref":reference,
                "context_ref":context_ref,"exact_target_found":false,"notification":notification,
                "reason":"notification identifies the topic but not a particular floor","media_downloaded":false,"content_writes":false}),
            );
        };
        let read = self.call(route, &params)?;
        let mut content = read["content"].clone();
        let actual_parent = content["parent_post_id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or("notification parent identity missing")?;
        let found = if notification.is_comment == Some(true) {
            content["comments"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["id"].as_u64() == selected))
        } else {
            content["parent_post"]["id"].as_u64() == selected.or(parent)
        };
        if notification.is_comment == Some(true) {
            content["selected_comment_id"] = json!(selected);
            content["selected_comment_found"] = json!(found);
        }
        let mut focus = if notification.is_comment == Some(true) {
            content["comments"]
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .find(|comment| comment["id"].as_u64() == selected)
                })
                .cloned()
        } else {
            Some(content["parent_post"].clone())
        };
        let related = notification.is_comment != Some(true) || found;
        if related {
            self.annotate_task_content(
                &mut content,
                &Platform::Tieba,
                None,
                Some(&thread.to_string()),
            )?;
            if let Some(focus) = focus.as_mut() {
                self.annotate_task_content(
                    focus,
                    &Platform::Tieba,
                    None,
                    Some(&thread.to_string()),
                )?;
            }
        } else {
            content.as_object_mut().unwrap().remove("parent_post");
            content["comments"] = json!([]);
            content["unrelated_context_omitted"] = json!(true);
        }
        let mut result = json!({"status":if found{"ok"}else{"partial"},"scope":scope,"kind":"notification","notification_ref":reference,
            "notification":notification,
            "context_ref":context_ref,"content":content,"focus":focus,"exact_target_found":found,
            "reply_supported":false,"media_downloaded":false,"content_writes":false});
        if related {
            result["discussion_ref"] = json!(self.save_task_target(&TaskTarget::TiebaPost {
                thread: thread.to_string(),
                post: actual_parent
            })?);
        }
        if !found {
            result["reason"] = json!(
                "selected notification content was not present in the returned native context; no other reply was substituted"
            );
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notification_refs_keep_provider_account_and_kind_across_restart_and_refuse_retargeting() {
        let root = std::env::temp_dir().join(format!(
            "notification-ref-{}-{}",
            std::process::id(),
            crate::outbox::now().unwrap()
        ));
        let rt = Runtime::open(&root, "alice", None).unwrap();
        let item = json!({"target":{"thread_id":7,"post_id":70},"content":"notification"});
        let reference = rt
            .notification_reference(&Platform::Tieba, "reply", &item)
            .unwrap();
        assert_eq!(
            rt.notification_reference(&Platform::Tieba, "reply", &item)
                .unwrap(),
            reference
        );
        drop(rt);
        let mut rt = Runtime::open(&root, "public", None).unwrap();
        assert!(
            rt.open_notification_reference(&json!({"reference":reference,"account":"bob"}))
                .unwrap_err()
                .contains("account differs")
        );
        assert!(
            rt.open_notification_reference(&json!({"reference":reference,"platform":"bilibili"}))
                .unwrap_err()
                .contains("platform differs")
        );
        let scope = rt.locators.scope_for(NOTIFICATION, &reference).unwrap();
        assert_eq!(scope.account, "alice");
        let saved: Snapshot = rt.locators.get(&scope, NOTIFICATION, &reference).unwrap();
        assert_eq!(saved.item, item);
        drop(rt);
        fs::remove_dir_all(root).unwrap();
    }
}
