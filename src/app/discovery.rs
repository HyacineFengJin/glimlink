// SPDX-License-Identifier: MIT
//! Account-bound entry points for roaming. Catalog reads never contact a site;
//! the selected adapter verifies the saved session when a feed is opened.
use super::*;
use serde::{Deserialize, Serialize};

const FEED: &str = "task-social-feed";
const PROVIDERS: &[(&str, &str)] = &[
    ("bilibili", "Bilibili homepage recommendations"),
    ("tieba", "Tieba personalized homepage"),
    ("x", "X For You"),
    ("pixiv", "Pixiv homepage recommendations"),
    ("reddit", "Reddit Best home feed"),
    ("discord", "Discord personalized highlights"),
    ("github", "GitHub following activity"),
];
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Feed {
    source: String,
}
fn platform(name: &str) -> Option<Platform> {
    match name {
        "bilibili" => Some(Platform::Bilibili),
        "tieba" => Some(Platform::Tieba),
        "x" => Some(Platform::X),
        "pixiv" => Some(Platform::Pixiv),
        "reddit" => Some(Platform::Reddit),
        "discord" => Some(Platform::Discord),
        "github" => Some(Platform::Github),
        _ => None,
    }
}
pub(super) fn unavailable(name: &str, source: &str) -> Option<&'static str> {
    match name {
        "discord" if source != "recommended" => Some(
            "Discord highlights are personalized notifications, not a followed-authors feed; joined servers and chats are separate capabilities.",
        ),
        "github" if source != "following" => Some(
            "A personalized GitHub dashboard recommendation reader is unavailable; use the explicitly labeled following activity source.",
        ),
        "qq" => Some("The QQ adapter provides chats, not personalized recommendation feeds."),
        "weixin" => Some("Weixin personal-account access is paused."),
        _ => None,
    }
}
impl Runtime {
    pub(super) fn discover_sources(&mut self, params: &Value) -> Result<Value> {
        let selected = params
            .get("platform")
            .map(|v| v.as_str().ok_or("platform must be a string"))
            .transpose()?;
        let source = params
            .get("source")
            .map(|v| v.as_str().ok_or("source must be a string"))
            .transpose()?
            .unwrap_or("recommended");
        if !matches!(source, "recommended" | "following") {
            return Err("feed catalog source must be recommended or following".into());
        }
        if selected.is_some_and(|p| platform(p).is_none() && unavailable(p, source).is_none()) {
            return Err("unknown recommendation platform".into());
        }
        let account = params
            .get("account")
            .map(|v| v.as_str().ok_or("account must be an alias string"))
            .transpose()?;
        if let Some(alias) = account {
            Scope {
                platform: Platform::Local,
                account: alias.into(),
            }
            .validate()?;
        }
        let limit = number(params, "limit", 20)?;
        if !(1..=32).contains(&limit) {
            return Err("feed catalog limit must be 1..32".into());
        }
        let accounts = self.saved_accounts()?;
        let mut feeds = Vec::new();
        let mut missing = Vec::new();
        let mut truncated = false;
        for (name, title) in PROVIDERS {
            if selected.is_some_and(|p| p != *name) {
                continue;
            }
            if let Some(reason) = unavailable(name, source) {
                missing.push(
                    json!({"platform":name,"status":"unsupported","source":source,"detail":reason}),
                );
                continue;
            }
            let aliases = accounts[*name]
                .as_array()
                .ok_or("invalid saved account catalog")?;
            let mut matched = false;
            for alias in aliases
                .iter()
                .filter_map(Value::as_str)
                .filter(|a| account.is_none_or(|wanted| wanted == *a))
            {
                matched = true;
                if feeds.len() >= limit as usize {
                    truncated = true;
                    continue;
                }
                let scope = Scope {
                    platform: platform(name).unwrap(),
                    account: alias.into(),
                };
                let reference = self.locators.put_reusable(
                    &scope,
                    FEED,
                    &Feed {
                        source: source.into(),
                    },
                )?;
                let kind = if source == "following" {
                    match *name {
                        "reddit" => "subscriptions",
                        "tieba" => "followed_forums",
                        _ => "following_activity",
                    }
                } else if *name == "discord" {
                    "personalized_highlights"
                } else {
                    "personalized_home"
                };
                let title = if source == "following" {
                    if *name == "tieba" {
                        "Tieba · Followed forums".into()
                    } else {
                        format!("{name} · Following")
                    }
                } else {
                    (*title).into()
                };
                feeds.push(json!({"feed_ref":reference,"scope":scope,"title":title,"source":source,
                    "recommendation_kind":kind,"authentication":"saved_unchecked","online_checked":false,
                    "next":{"tool":"social_discover","arguments":{"feed_ref":reference}}}));
            }
            if !matched {
                missing.push(json!({"platform":name,"source":source,"status":"needs_auth"}));
            }
        }
        for name in ["qq", "weixin"] {
            if selected.is_none_or(|p| p == name) {
                missing.push(json!({"platform":name,"source":source,"status":"unsupported","detail":unavailable(name, source)}));
            }
        }
        Ok(
            json!({"status":"ok","source":source,"feeds":feeds,"unavailable":missing,"truncated":truncated,
            "network_requested":false,"media_downloaded":false,"selection":"personalized recommendations and following are separate; no automatic fallback"}),
        )
    }
    pub(super) fn discover_feed_ref(&mut self, params: &Value) -> Result<Value> {
        let reference = required_str(params, "feed_ref")?;
        let scope = self.locators.scope_for(FEED, reference)?;
        tasks::check_account(params, &scope)?;
        let saved: Feed = self.locators.get(&scope, FEED, reference)?;
        if !matches!(saved.source.as_str(), "recommended" | "following") {
            return Err("invalid saved feed source".into());
        }
        let name = PROVIDERS
            .iter()
            .find(|(name, _)| platform(name).as_ref() == Some(&scope.platform))
            .map(|(name, _)| *name)
            .ok_or("invalid feed platform")?;
        if params.get("platform").is_some_and(|v| v != name)
            || params
                .get("source")
                .is_some_and(|v| v.as_str() != Some(saved.source.as_str()))
        {
            return Err("feed reference platform and source cannot be changed".into());
        }
        let mut request = params.clone();
        request.as_object_mut().unwrap().remove("feed_ref");
        request["platform"] = json!(name);
        request["source"] = json!(saved.source);
        request["account"] = json!(scope.account);
        self.with_scope_context(&scope, |rt| rt.call_task("social.discover", &request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_is_local_account_bound_and_never_projects_credentials() {
        let state =
            std::env::temp_dir().join(format!("personal-feed-catalog-{}", std::process::id()));
        private_dir(&state).unwrap();
        for name in ["bilibili", "pixiv"] {
            let dir = crate::private_state::account_directory(&state, name, "reader").unwrap();
            save_private_new(
                &dir.join("session.json"),
                &json!({"credential":"synthetic-private-must-not-project"}),
            )
            .unwrap();
        }
        let mut rt = Runtime::open(&state, "public", None).unwrap();
        let first = rt.call("social.discover", &json!({"action":"sources"})).unwrap();
        assert_eq!(first["feeds"].as_array().unwrap().len(), 2);
        assert_eq!(first["network_requested"], false);
        assert!(!first.to_string().contains("synthetic-private"));
        let second = rt
            .call("social.feeds", &json!({"platform":"pixiv"}))
            .unwrap();
        let feed = &second["feeds"][0];
        assert_eq!(feed["scope"]["account"], "reader");
        assert_eq!(feed["feed_ref"], first["feeds"][1]["feed_ref"]);
        for overrides in [
            json!({"account":"someone-else"}),
            json!({"platform":"bilibili"}),
            json!({"source":"popular"}),
        ] {
            let mut p = overrides;
            p["feed_ref"] = feed["feed_ref"].clone();
            assert!(rt.call("social.discover", &p).is_err());
        }
        let following = rt
            .call(
                "social.feeds",
                &json!({"platform":"pixiv","source":"following"}),
            )
            .unwrap();
        assert_ne!(following["feeds"][0]["feed_ref"], feed["feed_ref"]);
        assert_eq!(
            rt.call("social.discover", &json!({"platform":"github"}))
                .unwrap()["status"],
            "unsupported"
        );
        assert!(rt.call("social.feeds", &json!({"limit":33})).is_err());
        drop(rt);
        fs::remove_dir_all(state).unwrap();
    }
}
