// SPDX-License-Identifier: MIT
use super::*;

fn notification_kind(params: &Value) -> Result<&str> {
    match required_str(params, "kind")? {
        kind @ ("reply" | "at" | "like") => Ok(kind),
        _ => Err("notification kind must be reply, at or like".into()),
    }
}

impl Runtime {
    fn call_tieba_notifications(&mut self, params: &Value) -> Result<Value> {
        let kind = notification_kind(params)?;
        let typed = match kind {
            "reply" => platform_tieba::NotificationKind::Reply,
            "at" => platform_tieba::NotificationKind::At,
            "like" => platform_tieba::NotificationKind::Like,
            _ => unreachable!(),
        };
        let page = u32::try_from(number(params, "page", 1)?).map_err(|_| "invalid page")?;
        let limit = usize::try_from(number(params, "limit", 20)?)
            .map_err(|_| "invalid notification limit")?;
        if page == 0 || page > 100000 || limit == 0 || limit > 20 {
            return Err("notification page must be 1..100000 and limit 1..20".into());
        }
        let path = super::auth::saved_session(&self.state, "tieba", &self.account)?
            .ok_or("No saved Tieba login for this alias")?;
        let mut session = platform_tieba::Session::load(&path).map_err(|e| e.to_string())?;
        super::auth::bind_identity(
            path.parent().ok_or("missing account directory")?,
            &session.stable_identity(),
        )?;
        let data = platform_tieba::AuthClient::new()
            .map_err(|e| e.to_string())?
            .notifications(&mut session, typed, page, limit)
            .map_err(|e| e.to_string())?;
        session.save(&path).map_err(|e| e.to_string())?;
        self.cache_account_page(
            Platform::Tieba,
            kind,
            serde_json::to_value(data).map_err(|e| e.to_string())?,
        )
    }

    pub(super) fn cache_account_page(
        &self,
        platform: Platform,
        kind: &str,
        data: Value,
    ) -> Result<Value> {
        let scope = self.scope(platform);
        let page = json!({"scope":scope,"kind":kind,"data":data});
        let encoded = serde_json::to_vec(&page).map_err(|e| e.to_string())?;
        if encoded.len() > 2 * 1024 * 1024 {
            return Err("account page projection exceeds 2 MiB".into());
        }
        let manifest = self.store.import(
            &scope,
            &format!("account-{kind}-page"),
            "application/json",
            &mut encoded.as_slice(),
        )?;
        Ok(json!({"page":page,"manifest":manifest}))
    }

    pub(super) fn call_notifications(&mut self, method: &str, params: &Value) -> Result<Value> {
        if self.account == "public" {
            return Err(
                "Notifications need a saved personal account and explicit --account alias".into(),
            );
        }
        match method {
            "bilibili.unread" => {
                self.call_bilibili_account("bilibili.me")?;
                let counts = self.bili.notification_unread().map_err(|e| e.to_string())?;
                self.cache_account_page(
                    Platform::Bilibili,
                    "unread",
                    serde_json::to_value(counts).map_err(|e| e.to_string())?,
                )
            }
            "bilibili.notifications" => {
                let kind = notification_kind(params)?;
                let typed = match kind {
                    "reply" => platform_bilibili::NotificationKind::Reply,
                    "at" => platform_bilibili::NotificationKind::At,
                    "like" => platform_bilibili::NotificationKind::Like,
                    _ => unreachable!(),
                };
                let limit = u32::try_from(number(params, "limit", 20)?)
                    .map_err(|_| "invalid notification limit")?;
                let cursor = match (params.get("cursor_id"), params.get("cursor_time")) {
                    (None, None) => None,
                    (Some(_), Some(_)) => Some(platform_bilibili::NotificationCursor {
                        id: number(params, "cursor_id", 0)?,
                        time: number(params, "cursor_time", 0)?,
                    }),
                    _ => return Err("cursor_id and cursor_time must be supplied together".into()),
                };
                if limit == 0 || limit > 20 {
                    return Err("notification limit must be 1..20".into());
                }
                self.call_bilibili_account("bilibili.me")?;
                let page = self
                    .bili
                    .notifications(typed, cursor, limit)
                    .map_err(|e| e.to_string())?;
                self.cache_account_page(
                    Platform::Bilibili,
                    kind,
                    serde_json::to_value(page).map_err(|e| e.to_string())?,
                )
            }
            "tieba.notifications" => self.call_tieba_notifications(params),
            _ => Err("unknown notification method".into()),
        }
    }
}
