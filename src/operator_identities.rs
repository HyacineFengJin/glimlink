// SPDX-License-Identifier: MIT
//! Human-only account labels. Never credentials, model context, or login proof.
use crate::feature_state::Store;
use runtime_core::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
#[derive(Default, Serialize, Deserialize)]
struct Labels {
    items: Vec<Label>,
}
#[derive(Serialize, Deserialize)]
struct Label {
    platform: String,
    account: String,
    name: Option<String>,
    status: String,
    checked_at: u64,
}
fn store(state: &Path) -> Result<Store> {
    Store::open(state, "operator-identities", 128 * 1024)
}
fn name(v: &Value) -> Option<String> {
    let user = &v["user"];
    [
        v.get("display_name"),
        user.get("display_name"),
        user.get("name"),
        user.get("uname"),
        user.get("nickname"),
        user.get("user_name"),
        user.get("login"),
        user.get("screen_name"),
        v.pointer("/identity/label"),
    ]
    .into_iter()
    .flatten()
    .find_map(|v| {
        v.as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            .map(str::to_owned)
    })
}
pub fn remember(state: &Path, platform: &str, account: &str, v: &Value) -> Result<()> {
    let status = v["status"].as_str().unwrap_or("");
    if !matches!(
        status,
        "online" | "authenticated" | "saved_session" | "saved_token" | "signed_out"
    ) {
        return Ok(());
    }
    let display = name(v);
    let now = crate::agent_host::now()?;
    store(state)?.update(|labels: &mut Labels| {
        if let Some(l) = labels
            .items
            .iter_mut()
            .find(|l| l.platform == platform && l.account == account)
        {
            if status == "signed_out" {
                l.name = None
            } else if display.is_some() {
                l.name = display;
            }
            l.status = status.into();
            l.checked_at = now;
        } else if labels.items.len() < 128 {
            labels.items.push(Label {
                platform: platform.into(),
                account: account.into(),
                name: display,
                status: status.into(),
                checked_at: now,
            });
        }
        Ok(())
    })
}
pub fn list(state: &Path) -> Result<Value> {
    let s: Labels = store(state)?.load()?;
    if s.items.len() > 128 {
        return Err("identity_label_limit".into());
    }
    Ok(
        json!({"identities":s.items,"cached_display_only":true,"network_requests":0,"login_started":false}),
    )
}
