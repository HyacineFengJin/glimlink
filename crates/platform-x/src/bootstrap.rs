// SPDX-License-Identifier: MIT
// Reads only allowlisted public JS data. Never evaluates downloaded code.
use crate::{
    Error, Result,
    transaction::{Transaction, regex},
    transport::{Transport, script_url},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OperationKind {
    UserName,
    UserId,
    Tweet,
    Timeline,
}
#[derive(Clone)]
pub(crate) struct Operation {
    pub id: String,
    pub name: String,
    pub features: Value,
    pub modern: bool,
    providers: BTreeMap<String, bool>,
    unresolved_features: bool,
}
impl Operation {
    pub fn provided_variables(&self, vars: &mut Value) {
        for (k, v) in &self.providers {
            vars[k] = json!(v);
        }
    }
}
pub(crate) struct WebState {
    pub transaction: Transaction,
    operations: BTreeMap<OperationKind, Operation>,
    assets: BTreeSet<String>,
    fetched: BTreeSet<String>,
    flags: BTreeMap<String, bool>,
    bytes: usize,
    modern: bool,
}
impl WebState {
    pub fn load(transport: &Transport) -> Result<Self> {
        let html = transport.page()?;
        if !html.contains("twitter-site-verification") {
            return Err(
                if html.contains("/account/access")
                    || html.to_ascii_lowercase().contains("verify you are human")
                {
                    Error::VerificationRequired
                } else {
                    Error::ProtocolChanged
                },
            );
        }
        let flags = page_flags(&html)?;
        let modern = html.contains("/x-web/x-web/entry-client");
        let mut assets = BTreeSet::new();
        collect_assets(&html, None, &mut assets)?;
        let mut scripts = Vec::new();
        let mut fetched = BTreeSet::new();
        let mut bytes = html.len();
        let entry = assets
            .iter()
            .find(|url| {
                if modern {
                    url.contains("/entry-client")
                } else {
                    url.contains("/main.")
                }
            })
            .cloned()
            .ok_or(Error::ProtocolChanged)?;
        let script = transport.script(&entry)?;
        bytes += script.len();
        collect_assets(&script, Some(&entry), &mut assets)?;
        fetched.insert(entry.clone());
        scripts.push(script);
        if modern {
            let startup: Vec<_> = assets
                .iter()
                .filter(|s| {
                    s.rsplit('/').next().is_some_and(|n| {
                        n.starts_with("startup-") && !n.starts_with("startup-vendor")
                    })
                })
                .cloned()
                .collect();
            if startup.is_empty() || startup.len() > 4 {
                return Err(Error::ProtocolChanged);
            }
            for url in startup {
                let script = transport.script(&url)?;
                bytes += script.len();
                collect_assets(&script, Some(&url), &mut assets)?;
                fetched.insert(url);
                scripts.push(script);
            }
        }
        let sign_url = if modern {
            assets
                .iter()
                .find(|u| {
                    u.rsplit('/')
                        .next()
                        .is_some_and(|s| s.starts_with("sign.o-"))
                })
                .cloned()
        } else {
            legacy_sign(&html)?
        };
        let sign_url = sign_url.ok_or(Error::ProtocolChanged)?;
        let signature = transport.script(&sign_url)?;
        bytes += signature.len();
        fetched.insert(sign_url);
        if bytes > 16 * 1024 * 1024 {
            return Err(Error::Limit);
        }
        let transaction = Transaction::from_pages(&html, &signature)?;
        let mut out = Self {
            transaction,
            operations: BTreeMap::new(),
            assets,
            fetched,
            flags,
            bytes,
            modern,
        };
        for script in scripts {
            out.scan_operations(&script)?;
        }
        Ok(out)
    }
    pub fn operation(&mut self, transport: &Transport, kind: OperationKind) -> Result<Operation> {
        if let Some(op) = self.operations.get(&kind) {
            return if op.unresolved_features {
                Err(Error::ProtocolChanged)
            } else {
                Ok(op.clone())
            };
        }
        if self.modern {
            let prefixes: &[&str] = match kind {
                OperationKind::UserName => &["UserByScreenName.graphql-"],
                OperationKind::UserId => &[],
                OperationKind::Tweet => &["retweet-"],
                OperationKind::Timeline => &["user-posts-"],
            };
            self.fetch_prefixes(transport, prefixes)?;
            if kind == OperationKind::Timeline && !self.operations.contains_key(&kind) {
                self.fetch_prefixes(transport, &["_profile-"])?;
                self.fetch_prefixes(transport, prefixes)?;
            }
        }
        self.operations
            .get(&kind)
            .cloned()
            .filter(|op| !op.unresolved_features)
            .ok_or(Error::ProtocolChanged)
    }
    fn fetch_prefixes(&mut self, transport: &Transport, prefixes: &[&str]) -> Result<()> {
        for _ in 0..3 {
            let urls: Vec<_> = self
                .assets
                .iter()
                .filter(|url| {
                    !self.fetched.contains(*url)
                        && url
                            .rsplit('/')
                            .next()
                            .is_some_and(|name| prefixes.iter().any(|p| name.starts_with(p)))
                })
                .cloned()
                .collect();
            if urls.is_empty() {
                break;
            }
            if urls.len() > 8 {
                return Err(Error::Limit);
            }
            for url in urls {
                if self.fetched.len() >= 20 {
                    return Err(Error::Limit);
                }
                let script = transport.script(&url)?;
                self.bytes = self.bytes.checked_add(script.len()).ok_or(Error::Limit)?;
                if self.bytes > 16 * 1024 * 1024 {
                    return Err(Error::Limit);
                }
                collect_assets(&script, Some(&url), &mut self.assets)?;
                self.scan_operations(&script)?;
                self.fetched.insert(url);
            }
        }
        Ok(())
    }
    fn scan_operations(&mut self, script: &str) -> Result<()> {
        if self.modern {
            for c in regex(r#"params:\{id:["'`]([A-Za-z0-9_-]{8,128})["'`],metadata:\{[^}]{0,512}\},name:["'`]([A-Za-z0-9_]+)["'`],operationKind:["'`]query["'`]"#)?.captures_iter(script) {
                let kind=match &c[2]{"UserByScreenName"=>OperationKind::UserName,"userIdRedirectQuery"=>OperationKind::UserId,"TweetResultByRestId"=>OperationKind::Tweet,"UserOriginalsTimeline"=>OperationKind::Timeline,_=>continue};
                let end=c.get(0).ok_or(Error::ProtocolChanged)?.start();
                let start=script[..end].rfind("=(function(){").unwrap_or(end);
                let own=&script[start..end];let mut providers=BTreeMap::new();
                for capture in regex(r#"kind:["'`]LocalArgument["'`],name:["'`](__relay_internal__pv__[A-Za-z0-9_]+)["'`]"#)?.captures_iter(own) {
                    let key=&capture[1];providers.insert(key.to_owned(),provider_value(key,&self.flags)?);
                }
                insert_op(&mut self.operations,kind,Operation{id:c[1].to_owned(),name:c[2].to_owned(),features:json!({}),modern:true,providers,unresolved_features:false})?;
            }
        } else {
            for c in regex(
                r#"queryId:["']([A-Za-z0-9_-]{8,128})["'],operationName:["']([A-Za-z0-9_]+)["']"#,
            )?
            .captures_iter(script)
            {
                let kind = match &c[2] {
                    "UserByScreenName" => OperationKind::UserName,
                    "UserByRestId" => OperationKind::UserId,
                    "TweetDetail" => OperationKind::Tweet,
                    "UserTweets" => OperationKind::Timeline,
                    _ => continue,
                };
                let end = c.get(0).ok_or(Error::ProtocolChanged)?.end();
                let tail = &script[end..script.len().min(end + 8192)];
                let mut features = serde_json::Map::new();
                let mut unresolved_features = false;
                if let Some(m) = regex(r"featureSwitches:\[([^\]]{0,4096})\]")?.captures(tail) {
                    for name in regex(r#"["']([a-z0-9_]+)["']"#)?.captures_iter(&m[1]) {
                        let value = self
                            .flags
                            .get(&name[1])
                            .copied()
                            .or_else(|| legacy_flag(&name[1]));
                        let Some(value) = value else {
                            unresolved_features = true;
                            continue;
                        };
                        features.insert(name[1].to_owned(), json!(value));
                    }
                }
                insert_op(
                    &mut self.operations,
                    kind,
                    Operation {
                        id: c[1].to_owned(),
                        name: c[2].to_owned(),
                        features: Value::Object(features),
                        modern: false,
                        providers: BTreeMap::new(),
                        unresolved_features,
                    },
                )?;
            }
        }
        Ok(())
    }
}
fn insert_op(
    map: &mut BTreeMap<OperationKind, Operation>,
    kind: OperationKind,
    op: Operation,
) -> Result<()> {
    if map.get(&kind).is_some_and(|old| old.id != op.id) {
        return Err(Error::ProtocolChanged);
    }
    map.insert(kind, op);
    Ok(())
}
fn collect_assets(text: &str, origin: Option<&str>, out: &mut BTreeSet<String>) -> Result<()> {
    let re = regex(
        r#"https://abs\.twimg\.com/[A-Za-z0-9_./-]+\.js|(?:\./|assets/)[A-Za-z0-9_.-]+\.js"#,
    )?;
    for m in re.find_iter(text) {
        let value = m.as_str();
        let url = if value.starts_with("https://") {
            value.to_owned()
        } else if let Some(origin) = origin {
            if value.starts_with("assets/") {
                format!("https://abs.twimg.com/x-web/x-web/{value}")
            } else {
                let base = origin.rsplit_once('/').ok_or(Error::ProtocolChanged)?.0;
                format!("{base}/{}", value.trim_start_matches("./"))
            }
        } else {
            continue;
        };
        if script_url(&url) {
            out.insert(url);
        }
        if out.len() > 2048 {
            return Err(Error::Limit);
        }
    }
    Ok(())
}
fn page_flags(html: &str) -> Result<BTreeMap<String, bool>> {
    let mut flags = BTreeMap::new();
    if let Some(i) = html.find("window.__INITIAL_DATA__=") {
        let tail = &html[i + "window.__INITIAL_DATA__=".len()..];
        if let Some(Ok(v)) = serde_json::Deserializer::from_str(tail)
            .into_iter::<Value>()
            .next()
        {
            if let Some(values) = v
                .pointer("/featureSwitchPayload/features")
                .and_then(Value::as_object)
            {
                for (k, v) in values {
                    if let Some(b) = v.as_bool() {
                        flags.insert(k.clone(), b);
                    }
                }
            }
        }
    }
    for c in
        regex(r#""([a-z0-9_]{3,160})"\s*:\s*\{\s*"value"\s*:\s*(true|false)"#)?.captures_iter(html)
    {
        flags.insert(c[1].to_owned(), &c[2] == "true");
    }
    // Apply explicit legacy layers in precedence order; both flat booleans and
    // {value: bool} entries are data, never JavaScript expressions.
    if let Some(i) = html.find("window.__INITIAL_STATE__=") {
        let tail = &html[i + "window.__INITIAL_STATE__=".len()..];
        if let Some(Ok(v)) = serde_json::Deserializer::from_str(tail)
            .into_iter::<Value>()
            .next()
        {
            for path in [
                "/featureSwitch/defaultConfig",
                "/featureSwitch/defaultConfig/config",
                "/featureSwitch/user/config",
                "/featureSwitch/user/features",
            ] {
                if let Some(layer) = v.pointer(path).and_then(Value::as_object) {
                    for (k, v) in layer {
                        if let Some(b) = v
                            .as_bool()
                            .or_else(|| v.get("value").and_then(Value::as_bool))
                        {
                            flags.insert(k.clone(), b);
                        }
                    }
                }
            }
        }
    }
    Ok(flags)
}
fn provider_value(key: &str, flags: &BTreeMap<String, bool>) -> Result<bool> {
    let (flag, default) = match key {
        "__relay_internal__pv__xclientsfeaturesappviewerisloggedinrelayprovider" => {
            return Ok(true);
        }
        "__relay_internal__pv__xclientsfeaturestranslationgroktranslatedpostenabledrelayprovider" => {
            ("responsive_web_grok_show_grok_translated_post", true)
        }
        "__relay_internal__pv__xclientsfeaturestweetcomponentsgrokanalyzepostrelayprovider" => {
            ("grok_xweb_analyze_post_enabled", false)
        }
        "__relay_internal__pv__xclientsfeaturestranslationnoteautotranslationenabledrelayprovider" => {
            (
                "responsive_web_grok_community_note_auto_translation_is_enabled",
                false,
            )
        }
        "__relay_internal__pv__liveeventpostcontextenabledrelayprovider" => {
            ("x_elections_post_context_enabled", false)
        }
        "__relay_internal__pv__sportspostcontextenabledrelayprovider" => {
            ("rweb_sports_post_context_enabled", false)
        }
        _ => return Err(Error::ProtocolChanged),
    };
    Ok(flags.get(flag).copied().unwrap_or(default))
}
fn legacy_flag(name: &str) -> Option<bool> {
    // Explicit SDK projection: do not request reply-downvote UI or Made-on-X
    // labels. These are client choices, NOT inferred account/server defaults.
    if matches!(
        name,
        "rweb_conversational_replies_downvote_enabled" | "responsive_web_made_on_x_label_enabled"
    ) {
        return Some(false);
    }
    if matches!(
        name,
        "tweet_awards_web_tipping_enabled"
            | "verified_phone_label_enabled"
            | "responsive_web_media_download_video_enabled"
            | "responsive_web_graphql_skip_user_profile_image_extensions_enabled"
            | "responsive_web_enhance_cards_enabled"
            | "responsive_web_twitter_article_notes_tab_enabled"
    ) {
        return Some(false);
    }
    if matches!(
        name,
        "creator_subscriptions_tweet_preview_api_enabled"
            | "c9s_tweet_anatomy_moderator_badge_enabled"
            | "tweetypie_unmention_optimization_enabled"
            | "responsive_web_edit_tweet_api_enabled"
            | "graphql_is_translatable_rweb_tweet_is_translatable_enabled"
            | "view_counts_everywhere_api_enabled"
            | "longform_notetweets_consumption_enabled"
            | "responsive_web_twitter_article_tweet_consumption_enabled"
            | "longform_notetweets_rich_text_read_enabled"
            | "longform_notetweets_inline_media_enabled"
            | "rweb_video_timestamps_enabled"
            | "responsive_web_graphql_exclude_directive_enabled"
            | "freedom_of_speech_not_reach_fetch_enabled"
            | "standardized_nudges_misinfo"
            | "tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled"
            | "responsive_web_graphql_timeline_navigation_enabled"
            | "hidden_profile_likes_enabled"
            | "hidden_profile_subscriptions_enabled"
            | "subscriptions_verification_info_is_identity_verified_enabled"
            | "subscriptions_verification_info_verified_since_enabled"
            | "highlights_tweets_tab_ui_enabled"
    ) {
        return Some(true);
    }
    None
}
fn legacy_sign(html: &str) -> Result<Option<String>> {
    if let Some(c) = regex(r#"["']ondemand\.s["']:\s*["']([A-Za-z0-9_]+)["']"#)?.captures(html) {
        return Ok(Some(format!(
            "https://abs.twimg.com/responsive-web/client-web/ondemand.s.{}a.js",
            &c[1]
        )));
    }
    if let Some(c) = regex(r#",(\d+):["']ondemand\.s["']"#)?.captures(html) {
        let pattern = format!(r#",{}:["']([a-f0-9]+)["']"#, &c[1]);
        if let Some(hash) = regex(&pattern)?.captures(html) {
            return Ok(Some(format!(
                "https://abs.twimg.com/responsive-web/client-web/ondemand.s.{}a.js",
                &hash[1]
            )));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "explicit private candidate diagnostic; prints fixed stages and schema only"]
    fn private_candidate_stage_probe() -> Result<()> {
        if std::env::var("PRIVATE_X_CANDIDATE").ok().as_deref() != Some("1") {
            return Err(Error::InvalidInput);
        }
        let path = std::path::Path::new("/state/accounts/x/eC1tYWlu/login-candidate.json");
        let metadata = std::fs::metadata(path).map_err(|_| Error::Io)?;
        if metadata.len() > 32768 {
            return Err(Error::Limit);
        }
        let age = metadata
            .modified()
            .ok()
            .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
            .ok_or(Error::Io)?;
        if age.as_secs() > 15 * 60 {
            eprintln!("PRIVATE_X stage=candidate expired=true");
            return Err(Error::LoginRequired);
        }
        let bytes = std::fs::read(path).map_err(|_| Error::Io)?;
        let session: crate::Session =
            serde_json::from_slice(&bytes).map_err(|_| Error::InvalidInput)?;
        let transport = Transport::new(crate::Config::default(), session)?;
        eprintln!("PRIVATE_X stage=candidate ok=true");
        let html = match transport.page() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("PRIVATE_X stage=page error={}", e.code());
                return Err(e);
            }
        };
        eprintln!(
            "PRIVATE_X stage=page ok=true bytes={} has_verification={} modern={} has_form={} has_migrate={}",
            html.len(),
            html.contains("twitter-site-verification"),
            html.contains("/x-web/x-web/entry-client"),
            html.contains("<form"),
            html.contains("migrate")
        );
        if !html.contains("twitter-site-verification") {
            return Err(Error::ProtocolChanged);
        }
        let flags = page_flags(&html)?;
        let modern = html.contains("/x-web/x-web/entry-client");
        let mut assets = BTreeSet::new();
        collect_assets(&html, None, &mut assets)?;
        eprintln!(
            "PRIVATE_X stage=asset_discovery ok=true count={}",
            assets.len()
        );
        let entry = assets
            .iter()
            .find(|u| {
                if modern {
                    u.contains("/entry-client")
                } else {
                    u.contains("/main.")
                }
            })
            .cloned()
            .ok_or(Error::ProtocolChanged)?;
        let script = match transport.script(&entry) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("PRIVATE_X stage=entry error={}", e.code());
                return Err(e);
            }
        };
        eprintln!("PRIVATE_X stage=entry ok=true bytes={}", script.len());
        let mut total = html.len() + script.len();
        let mut fetched = BTreeSet::from([entry.clone()]);
        let mut scripts = vec![script];
        collect_assets(&scripts[0], Some(&entry), &mut assets)?;
        if modern {
            let urls: Vec<_> = assets
                .iter()
                .filter(|u| {
                    u.rsplit('/').next().is_some_and(|n| {
                        n.starts_with("startup-") && !n.starts_with("startup-vendor")
                    })
                })
                .cloned()
                .collect();
            eprintln!(
                "PRIVATE_X stage=startup_discovery ok=true count={}",
                urls.len()
            );
            if urls.is_empty() || urls.len() > 4 {
                return Err(Error::ProtocolChanged);
            }
            for url in urls {
                let script = match transport.script(&url) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("PRIVATE_X stage=startup_script error={}", e.code());
                        return Err(e);
                    }
                };
                total += script.len();
                eprintln!(
                    "PRIVATE_X stage=startup_script ok=true bytes={}",
                    script.len()
                );
                collect_assets(&script, Some(&url), &mut assets)?;
                fetched.insert(url);
                scripts.push(script);
            }
        }
        let sign = if modern {
            assets
                .iter()
                .find(|u| {
                    u.rsplit('/')
                        .next()
                        .is_some_and(|s| s.starts_with("sign.o-"))
                })
                .cloned()
        } else {
            legacy_sign(&html)?
        }
        .ok_or(Error::ProtocolChanged)?;
        let script = match transport.script(&sign) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("PRIVATE_X stage=sign_script error={}", e.code());
                return Err(e);
            }
        };
        total += script.len();
        fetched.insert(sign);
        eprintln!("PRIVATE_X stage=sign_script ok=true bytes={}", script.len());
        if total > 16 * 1024 * 1024 {
            return Err(Error::Limit);
        }
        let transaction = match Transaction::from_pages(&html, &script) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("PRIVATE_X stage=transaction_parse error={}", e.code());
                return Err(e);
            }
        };
        eprintln!("PRIVATE_X stage=transaction_parse ok=true");
        let mut web = WebState {
            transaction,
            operations: BTreeMap::new(),
            assets,
            fetched,
            flags,
            bytes: total,
            modern,
        };
        for script in scripts {
            if !modern {
                eprintln!(
                    "PRIVATE_X stage=legacy_flags ok=true count={}",
                    web.flags.len()
                );
                for c in regex(r#"queryId:["']([A-Za-z0-9_-]{8,128})["'],operationName:["']([A-Za-z0-9_]+)["']"#)?.captures_iter(&script){
                    if !matches!(&c[2],"UserByScreenName"|"UserByRestId"|"TweetDetail"|"UserTweets"){continue;}
                    let end=c.get(0).ok_or(Error::ProtocolChanged)?.end();let tail=&script[end..script.len().min(end+8192)];let mut missing=Vec::new();let mut count=0;
                    if let Some(m)=regex(r"featureSwitches:\[([^\]]{0,4096})\]")?.captures(tail){for name in regex(r#"["']([a-z0-9_]+)["']"#)?.captures_iter(&m[1]){count+=1;if !web.flags.contains_key(&name[1])&&legacy_flag(&name[1]).is_none(){missing.push(name[1].to_owned());}}}
                    eprintln!("PRIVATE_X stage=legacy_feature_schema operation={} feature_count={} missing_keys={}",&c[2],count,serde_json::to_string(&missing).map_err(|_|Error::ProtocolChanged)?);
                }
            }
            if let Err(e) = web.scan_operations(&script) {
                eprintln!("PRIVATE_X stage=operation_scan error={}", e.code());
                return Err(e);
            }
        }
        eprintln!(
            "PRIVATE_X stage=operation_scan ok=true count={}",
            web.operations.len()
        );
        let mut client = crate::Client {
            transport,
            web: Some(web),
        };
        let settings = match client.get_json("/i/api/1.1/account/settings.json", &[]) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("PRIVATE_X stage=settings error={}", e.code());
                return Err(e);
            }
        };
        let keys: Vec<_> = settings
            .as_object()
            .into_iter()
            .flat_map(|o| o.keys())
            .filter(|k| k.len() <= 64 && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
            .take(64)
            .cloned()
            .collect();
        eprintln!(
            "PRIVATE_X stage=settings ok=true keys={}",
            serde_json::to_string(&keys).map_err(|_| Error::ProtocolChanged)?
        );
        let name = settings
            .get("screen_name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                eprintln!(
                    "PRIVATE_X stage=settings_identity error=protocol_changed has_screen_name=false"
                );
                Error::ProtocolChanged
            })?;
        let op = client.operation(OperationKind::UserName).map_err(|e| {
            eprintln!("PRIVATE_X stage=viewer_operation error={}", e.code());
            e
        })?;
        let mut vars = crate::user_name_variables(op.modern, name);
        op.provided_variables(&mut vars);
        let raw = client.graphql(&op, vars).map_err(|e| {
            eprintln!("PRIVATE_X stage=viewer_graphql error={}", e.code());
            e
        })?;
        for (path, stage) in [
            ("/data", "viewer_data_schema"),
            ("/data/user/result", "viewer_legacy_schema"),
            (
                "/data/user_result_by_screen_name/result",
                "viewer_modern_schema",
            ),
        ] {
            let keys: Vec<_> = raw
                .pointer(path)
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|o| o.keys())
                .filter(|k| {
                    k.len() <= 64 && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
                .take(64)
                .cloned()
                .collect();
            eprintln!(
                "PRIVATE_X stage={} ok=true keys={}",
                stage,
                serde_json::to_string(&keys).map_err(|_| Error::ProtocolChanged)?
            );
        }
        match crate::model::user_response(&raw) {
            Ok(u) if u.screen_name.eq_ignore_ascii_case(name) => {
                eprintln!("PRIVATE_X stage=viewer_user ok=true");
                Ok(())
            }
            Ok(_) => Err(Error::ProtocolChanged),
            Err(e) => {
                eprintln!("PRIVATE_X stage=viewer_user error={}", e.code());
                Err(e)
            }
        }
    }
    #[test]
    fn assets_are_data_not_arbitrary_network_targets() {
        let mut out = BTreeSet::new();
        collect_assets(
            "https://evil.invalid/x.js ./ok-hash.js assets/retweet-abc.js",
            Some("https://abs.twimg.com/x-web/x-web/assets/startup-abc.js"),
            &mut out,
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|s| s.starts_with("https://abs.twimg.com/")));
    }
    #[test]
    fn flags_respect_reference_provider_default() {
        let flags=page_flags("window.__INITIAL_DATA__={\"featureSwitchPayload\":{\"features\":{\"grok_xweb_analyze_post_enabled\":true}}};").unwrap();
        assert!(
            provider_value(
                "__relay_internal__pv__xclientsfeaturestweetcomponentsgrokanalyzepostrelayprovider",
                &flags
            )
            .unwrap()
        );
        assert!(provider_value("__relay_internal__pv__xclientsfeaturestranslationgroktranslatedpostenabledrelayprovider",&flags).unwrap());
        assert!(provider_value("__relay_internal__pv__unknownrelayprovider", &flags).is_err());
    }
    #[test]
    fn legacy_explicit_user_layer_overrides_default_and_flat_values_work() {
        let flags=page_flags(r#"window.__INITIAL_STATE__={"featureSwitch":{"defaultConfig":{"fixture_flag":{"value":false}},"user":{"config":{"fixture_flag":{"value":true},"fixture_flat":false}}}};"#).unwrap();
        assert_eq!(flags.get("fixture_flag"), Some(&true));
        assert_eq!(flags.get("fixture_flat"), Some(&false));
        assert_eq!(legacy_flag("unrecognized_future_projection"), None);
    }
    #[test]
    fn unresolved_unused_operation_does_not_block_confirmed_user_operation() {
        use base64::Engine;
        let key = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        let row = "C 50,200 237,161 221,130 h 68 s 47,29 101,209 ";
        let html = format!(
            "<meta name='twitter-site-verification' content='{key}'><svg id='loading-x-anim-0'><g><path d='unused'/><path d='M 10,30 {}'/></g></svg>",
            row.repeat(16)
        );
        let transaction = Transaction::from_pages(&html, "(a[0],16)(a[1],16)").unwrap();
        let mut state = WebState {
            transaction,
            operations: BTreeMap::new(),
            assets: BTreeSet::new(),
            fetched: BTreeSet::new(),
            flags: BTreeMap::new(),
            bytes: 0,
            modern: false,
        };
        state.scan_operations(r#"queryId:"AAAAAAAAAAAAAAAAAAAAAA",operationName:"TweetDetail",metadata:{featureSwitches:["unrecognized_future_projection"]};queryId:"BBBBBBBBBBBBBBBBBBBBBB",operationName:"UserByScreenName",metadata:{featureSwitches:["responsive_web_graphql_exclude_directive_enabled"]}"#).unwrap();
        let transport = Transport::new(
            crate::Config::default(),
            crate::Session::new("fixture-auth", "fixture-csrf").unwrap(),
        )
        .unwrap();
        assert!(state.operation(&transport, OperationKind::UserName).is_ok());
        assert!(matches!(
            state.operation(&transport, OperationKind::Tweet),
            Err(Error::ProtocolChanged)
        ));
    }
    #[test]
    #[ignore = "optional offline public-document corpus, never requests an account"]
    fn public_source_corpus_has_all_four_read_operations() {
        let root = std::path::PathBuf::from(
            std::env::var_os("X_PUBLIC_CORPUS").expect("explicit public corpus path"),
        );
        let html = std::fs::read_to_string(root.join("home.html")).unwrap();
        let sign = std::fs::read_to_string(root.join("sign.js")).unwrap();
        let transaction = Transaction::from_pages(&html, &sign).unwrap();
        let mut state = WebState {
            transaction,
            operations: BTreeMap::new(),
            assets: BTreeSet::new(),
            fetched: BTreeSet::new(),
            flags: page_flags(&html).unwrap(),
            bytes: 0,
            modern: true,
        };
        collect_assets(&html, None, &mut state.assets).unwrap();
        for entry in std::fs::read_dir(&root).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) != Some("js") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            state.scan_operations(&source).unwrap();
            let origin = format!(
                "https://abs.twimg.com/x-web/x-web/assets/{}",
                path.file_name().unwrap().to_string_lossy()
            );
            collect_assets(&source, Some(&origin), &mut state.assets).unwrap();
        }
        for kind in [
            OperationKind::UserName,
            OperationKind::UserId,
            OperationKind::Tweet,
            OperationKind::Timeline,
        ] {
            assert!(state.operations.contains_key(&kind));
        }
        assert!(state.assets.iter().any(|s| s.contains("entry-client")));
        let op = state.operations.get(&OperationKind::Tweet).unwrap();
        let mut vars = json!({});
        op.provided_variables(&mut vars);
        assert_eq!(
            vars["__relay_internal__pv__xclientsfeaturesappviewerisloggedinrelayprovider"],
            true
        );
        assert_eq!(
            vars["__relay_internal__pv__xclientsfeaturestranslationgroktranslatedpostenabledrelayprovider"],
            true
        );
    }
}
