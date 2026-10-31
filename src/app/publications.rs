// SPDX-License-Identifier: MIT
use super::*;

impl Runtime {
    pub(super) fn call_publications(&mut self, method: &str, params: &Value) -> Result<Value> {
        if self.account == "public" {
            return Err(
                "Own publications need a saved account and explicit --account alias".into(),
            );
        }
        let page = u32::try_from(number(params, "page", 1)?).map_err(|_| "invalid page")?;
        let limit = u32::try_from(number(params, "limit", 20)?).map_err(|_| "invalid limit")?;
        if page == 0 || page > 100000 || limit == 0 || limit > 20 {
            return Err("publication page must be 1..100000 and limit 1..20".into());
        }
        match method {
            "bilibili.publications" => {
                self.call_bilibili_account("bilibili.me")?;
                let data = self
                    .bili
                    .my_publications(page, limit)
                    .map_err(|e| e.to_string())?;
                let scope = self.scope(Platform::Bilibili);
                let mut assets = self.locators.batch();
                let mut projected = serde_json::to_value(&data).map_err(|e| e.to_string())?;
                for (original, item) in data.items.iter().zip(
                    projected["items"]
                        .as_array_mut()
                        .ok_or("invalid publication projection")?,
                ) {
                    if let Some(cover) = &original.cover {
                        let asset = assets.put(&scope, "bilibili-media", cover)?;
                        item["cover"] = json!({"asset":asset,"kind":"image","fetched":false});
                    }
                }
                let result =
                    self.cache_account_page(Platform::Bilibili, "published_videos", projected)?;
                assets.commit();
                Ok(result)
            }
            "tieba.publications" => {
                let requested_kind = match params.get("kind") {
                    None => "thread",
                    Some(Value::String(kind)) => kind,
                    _ => return Err("Tieba publication kind must be a string".into()),
                };
                let kind = match requested_kind {
                    "thread" => platform_tieba::PublicationKind::Threads,
                    "reply" => platform_tieba::PublicationKind::Replies,
                    _ => return Err("Tieba publication kind must be thread or reply".into()),
                };
                let path = super::auth::saved_session(&self.state, "tieba", &self.account)?
                    .ok_or("No saved Tieba login for this alias")?;
                let mut session =
                    platform_tieba::Session::load(&path).map_err(|e| e.to_string())?;
                super::auth::bind_identity(
                    path.parent().ok_or("missing account directory")?,
                    &session.stable_identity(),
                )?;
                let data = platform_tieba::AuthClient::new()
                    .map_err(|e| e.to_string())?
                    .self_publications(&mut session, kind, page, limit as usize)
                    .map_err(|e| e.to_string())?;
                session.save(&path).map_err(|e| e.to_string())?;
                self.cache_account_page(
                    Platform::Tieba,
                    "own_posts",
                    serde_json::to_value(data).map_err(|e| e.to_string())?,
                )
            }
            _ => Err("unknown publication read".into()),
        }
    }

    pub(super) fn bilibili_asset(&mut self, params: &Value) -> Result<Value> {
        let asset = required_str(params, "asset")?;
        let scope = self.scope(Platform::Bilibili);
        let resource: platform_bilibili::MediaResource =
            self.locators.get(&scope, "bilibili-media", asset)?;
        let work = WorkDir::new(&self.state)?;
        let path = work.path.join("cover");
        let receipt = self
            .bili
            .download_to_path(&resource, &path, 8 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mime = super::download_mime(&path, receipt.content_type.as_deref(), "image")?;
        let stored = self.import_output(&scope, "publication-cover", &path, &mime)?;
        Ok(json!({"asset":asset,"resource":stored,"fetched":true,"model_observed":false}))
    }
}
