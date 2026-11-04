// SPDX-License-Identifier: MIT
//! Current account's explicitly followed forums. The concern/userlike stream
//! mixes relationship/recommendation cards and is not evidence of forum follows.
//! Request/field facts: aiotieba get_self_follow_forums at 3922a9f.
use crate::{Client, Error, Forum, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FollowedForums {
    pub forums: Vec<Forum>,
    /// This nonpaginated provider endpoint returns at most 200 followed forums.
    pub provider_list_may_be_capped: bool,
}
fn positive(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|v| *v > 0 && *v <= i64::MAX as u64)
}
fn parse(raw: &[u8]) -> Result<FollowedForums> {
    let value: Value =
        serde_json::from_slice(raw).map_err(|_| Error::Schema("followed forums JSON"))?;
    let code = value
        .get("error_code")
        .ok_or(Error::Schema("followed forums status"))?;
    let code = code
        .as_i64()
        .or_else(|| code.as_str()?.parse().ok())
        .ok_or(Error::Schema("followed forums status"))?;
    if code != 0 {
        return Err(Error::Platform(code));
    }
    // Never use recommend_forum or other cards as a substitute for like_forum.
    let rows = value["like_forum"]
        .as_array()
        .ok_or(Error::Schema("followed forum list"))?;
    if rows.len() > 200 {
        return Err(Error::Limit);
    }
    let mut seen = BTreeSet::new();
    let mut forums = Vec::new();
    for row in rows {
        let id = positive(&row["forum_id"]).ok_or(Error::Schema("followed forum identity"))?;
        let name = row["forum_name"]
            .as_str()
            .filter(|v| !v.trim().is_empty() && v.len() <= 256 && !v.chars().any(char::is_control))
            .ok_or(Error::Schema("followed forum name"))?;
        if seen.insert(id) {
            forums.push(Forum {
                id,
                name: name.into(),
            });
        }
    }
    Ok(FollowedForums {
        forums,
        provider_list_may_be_capped: rows.len() == 200,
    })
}
impl Client {
    /// Only this account's own like_forum list, never a user-following tab or
    /// suggested-forum discovery stream. Does not sign in or follow anything.
    pub fn followed_forums(&self) -> Result<FollowedForums> {
        if self.transport.bduss().is_none() {
            return Err(Error::LoginRequired);
        }
        parse(&self.transport.followed_forums()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn followed_forum_membership_comes_only_from_the_own_like_list() {
        let raw = json!({"error_code":"0","like_forum":[{"forum_id":"42","forum_name":"Synthetic followed forum","BDUSS":"private-cookie"},{"forum_id":42,"forum_name":"Synthetic followed forum"}],"recommend_forum":[{"forum_id":99,"forum_name":"Not followed"}],"author":{"has_concerned":1}});
        let page = parse(&serde_json::to_vec(&raw).unwrap()).unwrap();
        assert_eq!(page.forums.len(), 1);
        assert_eq!(page.forums[0].id, 42);
        assert!(!page.provider_list_may_be_capped);
        let encoded = serde_json::to_string(&page).unwrap();
        assert!(!encoded.contains("private-cookie"));
        assert!(!encoded.contains("Not followed"));
        assert!(parse(br#"{"error_code":0,"recommend_forum":[]}"#).is_err());
        assert!(parse(br#"{"error_code":2150040,"like_forum":[]}"#).is_err());
        assert!(
            parse(br#"{"error_code":0,"like_forum":[{"forum_id":0,"forum_name":"bad"}]}"#).is_err()
        );
        let empty = parse(br#"{"error_code":0,"like_forum":[]}"#).unwrap();
        assert!(empty.forums.is_empty());
    }
    #[test]
    fn followed_forums_have_an_explicit_provider_cap_and_require_login() {
        let raw = json!({"error_code":0,"like_forum":(1..=200).map(|id|json!({"forum_id":id,"forum_name":format!("forum {id}")})).collect::<Vec<_>>()});
        assert!(
            parse(&serde_json::to_vec(&raw).unwrap())
                .unwrap()
                .provider_list_may_be_capped
        );
        let client = Client::new(crate::Config::default()).unwrap();
        assert_eq!(client.followed_forums().unwrap_err(), Error::LoginRequired);
    }
}
