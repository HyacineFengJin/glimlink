// SPDX-License-Identifier: MIT
use super::*;
use crate::private_state::LocatorBatch;
use platform_tieba::{
    Client as TiebaClient, Comment, Config as TiebaConfig, Fragment, MediaKind, MediaResource,
    Post, ThreadId,
};

pub(super) struct TiebaState {
    config: TiebaConfig,
    client: Option<TiebaClient>,
}
impl TiebaState {
    pub fn new(config: TiebaConfig) -> Self {
        Self {
            config,
            client: None,
        }
    }
}

fn fragments(
    input: &[Fragment],
    scope: &Scope,
    locators: &mut LocatorBatch<'_>,
) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    for fragment in input {
        result.push(match fragment{
            Fragment::Media{resource,label}=>{
                // Public MediaResource serialization deliberately drops the URL;
                // only this private record is allowed to persist it.
                let private=json!({"kind":resource.kind,"url":resource.url,"mime":resource.mime,"width":resource.width,"height":resource.height,"duration_seconds":resource.duration_seconds});
                let asset=locators.put(scope,"tieba-media",&private)?;
                json!({"type":"media","kind":resource.kind,"asset":asset,"label":label,"width":resource.width,"height":resource.height,"duration_seconds":resource.duration_seconds,"fetched":false})
            },
            other=>serde_json::to_value(other).map_err(|e|e.to_string())?,
        });
    }
    Ok(result)
}
fn comment(
    input: &Comment,
    scope: &Scope,
    locators: &mut LocatorBatch<'_>,
    thread: u64,
) -> Result<Value> {
    let reply = locators.put(
        scope,
        "task-tieba-reply",
        &json!({"thread":thread,"parent":input.parent_post_id,"child":input.id}),
    )?;
    Ok(
        json!({"id":input.id,"parent_post_id":input.parent_post_id,"author":input.author,"created_at":input.created_at,"floor":input.floor,"reply_to_user_id":input.reply_to_user_id,"fragments":fragments(&input.fragments,scope,locators)?,"reply_ref":reply,"reply_tool":"social_tieba","reply_action":"reply_floor","reply_supported":true}),
    )
}
fn post(
    input: &Post,
    scope: &Scope,
    locators: &mut LocatorBatch<'_>,
    thread: u64,
) -> Result<Value> {
    let reply = locators.put(
        scope,
        "task-tieba-reply",
        &json!({"thread":thread,"parent":input.id,"child":null}),
    )?;
    let comments = input
        .comments
        .iter()
        .map(|v| comment(v, scope, locators, thread))
        .collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"id":input.id,"floor":input.floor,"created_at":input.created_at,"author":input.author,"reply_count":input.reply_count,"fragments":fragments(&input.fragments,scope,locators)?,"comments":comments,"reply_ref":reply,"reply_tool":"social_tieba","reply_action":"reply_floor","reply_supported":true}),
    )
}

impl Runtime {
    pub(super) fn tieba_client(&mut self) -> Result<&TiebaClient> {
        if self.tieba.client.is_none() {
            self.tieba.client =
                Some(TiebaClient::new(self.tieba.config.clone()).map_err(|e| e.to_string())?);
        }
        Ok(self.tieba.client.as_ref().unwrap())
    }
    pub(super) fn call_tieba(&mut self, method: &str, params: &Value) -> Result<Value> {
        let scope = self.scope(Platform::Tieba);
        match method {
            "tieba.thread" => {
                let id: ThreadId = required_str(params, "thread")?
                    .parse()
                    .map_err(|e: platform_tieba::Error| e.to_string())?;
                let page = u32::try_from(number(params, "page", 1)?).map_err(|_| "invalid page")?;
                let fetched = self
                    .tieba_client()?
                    .thread_page(&id, page)
                    .map_err(|e| e.to_string())?;
                let mut assets = self.locators.batch();
                let mut resources = self.store.batch();
                let raw = resources.import(
                    &scope,
                    &format!("thread:{}:{page}:source", id.0),
                    fetched.source_mime,
                    &mut fetched.raw_source.as_slice(),
                )?;
                let posts = fetched
                    .data
                    .posts
                    .iter()
                    .map(|v| post(v, &scope, &mut assets, id.0))
                    .collect::<Result<Vec<_>>>()?;
                let page_data = json!({"thread_id":id.0,"title":fetched.data.title,"forum":fetched.data.forum,"author":fetched.data.author,"reply_count":fetched.data.reply_count,"page":fetched.data.page,"posts":posts,"comments_included":fetched.data.comments_included,"notes":fetched.data.notes});
                let document = Document::new(
                    scope.clone(),
                    format!("tieba:{}:page:{page}", id.0),
                    fetched.data.title,
                    vec![
                        ContentBlock::Json {
                            value: page_data.clone(),
                        },
                        ContentBlock::Source {
                            resource: raw.clone(),
                            label: "credential-stripped protobuf source".into(),
                        },
                    ],
                );
                let result = json!({"document":document,"page":page_data,"source":raw,"source_redacted":fetched.source_redacted,"response_bytes":fetched.response_bytes});
                let manifest = resources.import(
                    &scope,
                    "tieba-thread-manifest",
                    "application/json",
                    &mut serde_json::to_vec(&result)
                        .map_err(|e| e.to_string())?
                        .as_slice(),
                )?;
                resources.commit();
                assets.commit();
                Ok(json!({"manifest":manifest,"content":result}))
            }
            "tieba.comments" | "tieba.comment_context" => {
                let id: ThreadId = required_str(params, "thread")?
                    .parse()
                    .map_err(|e: platform_tieba::Error| e.to_string())?;
                let post_id = number(params, "post", 0)?;
                let page = u32::try_from(number(params, "page", 1)?).map_err(|_| "invalid page")?;
                let fetched = if method == "tieba.comment_context" {
                    let selected = number(params, "comment", 0)?;
                    self.tieba_client()?.comment_context(
                        &id,
                        selected,
                        (post_id > 0).then_some(post_id),
                    )
                } else {
                    self.tieba_client()?.comments(&id, post_id, page)
                }
                .map_err(|e| e.to_string())?;
                let mut assets = self.locators.batch();
                let mut resources = self.store.batch();
                let raw = resources.import(
                    &scope,
                    &format!("thread:{}:post:{post_id}:comments:{page}", id.0),
                    fetched.source_mime,
                    &mut fetched.raw_source.as_slice(),
                )?;
                let comments = fetched
                    .data
                    .comments
                    .iter()
                    .map(|v| comment(v, &scope, &mut assets, id.0))
                    .collect::<Result<Vec<_>>>()?;
                let parent_post = fetched
                    .data
                    .parent_post
                    .as_ref()
                    .map(|parent| post(parent, &scope, &mut assets, id.0))
                    .transpose()?;
                let result = json!({"thread_id":id.0,"parent_post_id":fetched.data.parent_post_id,"parent_post":parent_post,
                    "selected_comment_id":fetched.data.selected_comment_id,"selected_comment_found":fetched.data.selected_comment_found,
                    "title":fetched.data.title,"forum":fetched.data.forum,"page":fetched.data.page,"comments":comments,"source":raw,"source_redacted":fetched.source_redacted});
                let manifest = resources.import(
                    &scope,
                    "tieba-comments-manifest",
                    "application/json",
                    &mut serde_json::to_vec(&result)
                        .map_err(|e| e.to_string())?
                        .as_slice(),
                )?;
                resources.commit();
                assets.commit();
                Ok(json!({"manifest":manifest,"content":result}))
            }
            "tieba.media" => {
                let asset = required_str(params, "asset")?;
                let resource: MediaResource = self.locators.get(&scope, "tieba-media", asset)?;
                let work = WorkDir::new(&self.state)?;
                let path = work.path.join("download");
                let receipt = self
                    .tieba_client()?
                    .download_to_path(&resource, &path, Budget::default().max_resource_bytes)
                    .map_err(|e| e.to_string())?;
                let group = match resource.kind {
                    MediaKind::Image | MediaKind::Emoji => "image",
                    MediaKind::Audio => "audio",
                    MediaKind::Video => "video",
                };
                let mime = super::download_mime(&path, receipt.mime.as_deref(), group)?;
                let stored = self.import_output(&scope, "tieba-media", &path, &mime)?;
                Ok(
                    json!({"asset":asset,"resource":stored,"kind":resource.kind,"fetched":true,"model_observed":false}),
                )
            }
            _ => Err("unknown or unavailable Tieba tool".into()),
        }
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    #[test]
    fn native_child_projection_returns_exact_account_bound_reply_tool_input() {
        let root = std::env::temp_dir().join(format!(
            "tieba-child-projection-{}-{}",
            std::process::id(),
            crate::outbox::now().unwrap()
        ));
        let store = crate::private_state::PrivateLocators::open(&root).unwrap();
        let scope = Scope {
            platform: Platform::Tieba,
            account: "owner".into(),
        };
        let input = Comment {
            id: 789,
            parent_post_id: 456,
            author: platform_tieba::Author::default(),
            created_at: 100,
            floor: 2,
            reply_to_user_id: Some(77),
            fragments: vec![Fragment::Text {
                text: "body".into(),
            }],
        };
        let mut batch = store.batch();
        let projected = comment(&input, &scope, &mut batch, 123).unwrap();
        batch.commit();
        let reference = projected["reply_ref"].as_str().unwrap();
        let saved: Value = store.get(&scope, "task-tieba-reply", reference).unwrap();
        assert_eq!(saved, json!({"thread":123,"parent":456,"child":789}));
        assert_eq!(projected["reply_tool"], "social_tieba");
        assert_eq!(projected["reply_action"], "reply_floor");
        assert_eq!(projected["reply_supported"], true);
        assert!(
            store
                .get::<Value>(
                    &Scope {
                        platform: Platform::Tieba,
                        account: "other".into()
                    },
                    "task-tieba-reply",
                    reference
                )
                .is_err()
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
