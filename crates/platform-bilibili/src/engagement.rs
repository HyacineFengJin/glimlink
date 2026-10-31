// SPDX-License-Identifier: MIT
//! Positive like/follow operations. The embedding host journals each consumed
//! capability before POST; unknown outcomes are reconciled with reads only.
//! Sources: pskdje/bilibili-API-collect docs/video/action.md and user/relation.md.
use crate::{Client, Error, Result, VideoId, api_data};
use crate::{comments::csrf_from_cookie, http::EngagementPost};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngagementAction {
    LikeVideo,
    FollowUploader,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngagementTarget {
    pub aid: u64,
    pub bvid: String,
    pub title: String,
    pub owner_mid: u64,
    pub owner_name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngagementState {
    /// archive/has/like reports recent state; false is not lifetime proof.
    pub liked: Option<bool>,
    pub following: Option<bool>,
    pub blocked: Option<bool>,
    pub relation_attribute: Option<u64>,
    pub like_state_scope: Option<String>,
}
impl EngagementState {
    fn satisfied(&self, action: EngagementAction) -> bool {
        match action {
            EngagementAction::LikeVideo => self.liked == Some(true),
            EngagementAction::FollowUploader => {
                self.following == Some(true) && self.blocked == Some(false)
            }
        }
    }
}

/// Contains account-bound Cookie and CSRF material. Not Clone or Serialize.
pub struct PreparedEngagement {
    actor_mid: u64,
    target: EngagementTarget,
    action: EngagementAction,
    state_before: EngagementState,
    csrf: String,
    cookie_binding: String,
    prepared_at: u64,
}
impl fmt::Debug for PreparedEngagement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedEngagement")
            .field("actor_mid", &self.actor_mid)
            .field("target", &self.target)
            .field("action", &self.action)
            .field("state_before", &self.state_before)
            .field("private_payload", &"[redacted]")
            .finish()
    }
}
impl PreparedEngagement {
    pub fn actor_mid(&self) -> u64 {
        self.actor_mid
    }
    pub fn target(&self) -> &EngagementTarget {
        &self.target
    }
    pub fn action(&self) -> EngagementAction {
        self.action
    }
    pub fn state_before(&self) -> &EngagementState {
        &self.state_before
    }
    pub fn needed(&self) -> bool {
        !self.state_before.satisfied(self.action)
    }
}

/// Server acceptance only. This is not proof the desired state is visible.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngagementReceipt {
    pub actor_mid: u64,
    pub target: EngagementTarget,
    pub action: EngagementAction,
    pub accepted_at: u64,
    pub result_code: i64,
}

/// Genuine current GET projection, also usable without any success receipt.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngagementVerification {
    pub actor_mid: u64,
    pub target: EngagementTarget,
    pub action: EngagementAction,
    pub state: EngagementState,
    pub verified: bool,
}

fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| Error::InvalidConfig)
}
fn id(id: u64) -> Result<()> {
    if id == 0 || id > i64::MAX as u64 {
        Err(Error::InvalidId)
    } else {
        Ok(())
    }
}
fn number(raw: &Value, name: &str) -> Result<u64> {
    raw.get(name)
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .ok_or(Error::Schema("engagement number"))
}
fn like_state(data: Value) -> Result<EngagementState> {
    let value = data
        .as_u64()
        .or_else(|| data.as_str()?.parse().ok())
        .ok_or(Error::Schema("recent like flag"))?;
    let liked = match value {
        0 => false,
        1 => true,
        _ => return Err(Error::Schema("recent like flag")),
    };
    Ok(EngagementState {
        liked: Some(liked),
        like_state_scope: Some("recent_only".into()),
        ..Default::default()
    })
}
fn relation_state(data: Value, owner_mid: u64) -> Result<EngagementState> {
    id(owner_mid)?;
    let relation_mid = number(&data, "mid")?;
    let attribute = number(&data, "attribute")?;
    // The actual relation endpoint uses this exact empty-relation sentinel.
    // The action target stays bound to video_info.owner, never this zero MID.
    let empty_relation = relation_mid == 0 && attribute == 0;
    if relation_mid != owner_mid && !empty_relation {
        return Err(Error::Schema("uploader relation target mismatch"));
    }
    let (following, blocked) = match attribute {
        0 => (false, false),
        1 | 2 | 6 => (true, false),
        n if n & 128 != 0 => (false, true),
        _ => {
            return Err(Error::Schema(
                "unrecognized uploader relation; no automatic change",
            ));
        }
    };
    Ok(EngagementState {
        following: Some(following),
        blocked: Some(blocked),
        relation_attribute: Some(attribute),
        ..Default::default()
    })
}
fn validate_target(target: &EngagementTarget) -> Result<()> {
    id(target.aid)?;
    id(target.owner_mid)?;
    if !matches!(target.bvid.parse::<VideoId>()?, VideoId::Bv(_)) {
        return Err(Error::InvalidId);
    }
    Ok(())
}
fn ensure_follow_allowed(
    actor_mid: u64,
    target: &EngagementTarget,
    state: &EngagementState,
) -> Result<()> {
    if actor_mid == target.owner_mid {
        return Err(Error::Schema("cannot follow current account itself"));
    }
    if state.blocked == Some(true) {
        return Err(Error::Schema(
            "uploader is blocked; refusing implicit unblock",
        ));
    }
    if state.following.is_none() || state.blocked.is_none() {
        return Err(Error::Schema(
            "uploader relation unavailable; no follow sent",
        ));
    }
    Ok(())
}
fn form(prepared: &PreparedEngagement) -> Result<Vec<u8>> {
    validate_target(&prepared.target)?;
    if prepared.csrf.len() != 32 || !prepared.csrf.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::LoginState);
    }
    let body = match prepared.action {
        EngagementAction::LikeVideo => {
            format!("aid={}&like=1&csrf={}", prepared.target.aid, prepared.csrf)
        }
        EngagementAction::FollowUploader => {
            ensure_follow_allowed(prepared.actor_mid, &prepared.target, &prepared.state_before)?;
            // re_src=14 is the documented video-page source. No act=2/3/5/6 etc.
            format!(
                "fid={}&act=1&re_src=14&csrf={}",
                prepared.target.owner_mid, prepared.csrf
            )
        }
    };
    Ok(body.into_bytes())
}
fn parse_receipt(raw: Value, prepared: &PreparedEngagement) -> Result<EngagementReceipt> {
    let code = raw
        .get("code")
        .and_then(Value::as_i64)
        .ok_or(Error::WriteOutcomeUnknown("engagement_result_code"))?;
    if code != 0 {
        return Err(match code {
            -101 => Error::LoginRequired,
            -352 | -412 | -401 | -509 => Error::RiskControl(code),
            _ => Error::Platform(code),
        });
    }
    if raw.get("v_voucher").is_some()
        || raw.pointer("/data/v_voucher").is_some()
        || raw.get("need_captcha").and_then(Value::as_bool) == Some(true)
        || raw.pointer("/data/need_captcha").and_then(Value::as_bool) == Some(true)
    {
        return Err(Error::WriteOutcomeUnknown("engagement_challenge"));
    }
    Ok(EngagementReceipt {
        actor_mid: prepared.actor_mid,
        target: prepared.target.clone(),
        action: prepared.action,
        accepted_at: now()?,
        result_code: code,
    })
}

impl Client {
    fn engagement_context(&self, video: &VideoId) -> Result<(u64, EngagementTarget)> {
        self.transport.cookie_header().ok_or(Error::LoginRequired)?;
        let actor = self.current_user()?;
        let view = self.video_info(video)?;
        let target = EngagementTarget {
            aid: view.aid,
            bvid: view.bvid,
            title: view.title,
            owner_mid: view.owner.id,
            owner_name: view.owner.name,
        };
        validate_target(&target)?;
        id(actor.mid)?;
        match video {
            VideoId::Av(aid) if *aid != target.aid => {
                return Err(Error::Schema("engagement video mismatch"));
            }
            VideoId::Bv(bvid) if *bvid != target.bvid => {
                return Err(Error::Schema("engagement video mismatch"));
            }
            _ => {}
        }
        Ok((actor.mid, target))
    }
    fn read_engagement_state(
        &self,
        target: &EngagementTarget,
        action: EngagementAction,
    ) -> Result<EngagementState> {
        validate_target(target)?;
        match action {
            EngagementAction::LikeVideo => like_state(api_data(
                self.transport.json(&format!(
                    "https://api.bilibili.com/x/web-interface/archive/has/like?aid={}",
                    target.aid
                ))?,
                false,
            )?),
            EngagementAction::FollowUploader => relation_state(
                api_data(
                    self.transport.json(&format!(
                        "https://api.bilibili.com/x/relation?fid={}",
                        target.owner_mid
                    ))?,
                    false,
                )?,
                target.owner_mid,
            ),
        }
    }
    pub fn engagement_state(
        &self,
        video: &VideoId,
        action: EngagementAction,
    ) -> Result<EngagementVerification> {
        let (actor_mid, target) = self.engagement_context(video)?;
        let state = self.read_engagement_state(&target, action)?;
        Ok(EngagementVerification {
            actor_mid,
            target,
            action,
            verified: state.satisfied(action),
            state,
        })
    }
    pub fn prepare_video_like(&self, video: &VideoId) -> Result<PreparedEngagement> {
        self.prepare_engagement(video, EngagementAction::LikeVideo)
    }
    pub fn prepare_uploader_follow(&self, video: &VideoId) -> Result<PreparedEngagement> {
        self.prepare_engagement(video, EngagementAction::FollowUploader)
    }
    fn prepare_engagement(
        &self,
        video: &VideoId,
        action: EngagementAction,
    ) -> Result<PreparedEngagement> {
        let cookie = self.transport.cookie_header().ok_or(Error::LoginRequired)?;
        let csrf = csrf_from_cookie(cookie)?;
        let current = self.engagement_state(video, action)?;
        if action == EngagementAction::FollowUploader {
            ensure_follow_allowed(current.actor_mid, &current.target, &current.state)?;
        }
        Ok(PreparedEngagement {
            actor_mid: current.actor_mid,
            target: current.target,
            action,
            state_before: current.state,
            csrf,
            cookie_binding: cookie.into(),
            prepared_at: now()?,
        })
    }
    /// Local validation and exactly one positive POST; never reads, redirects or retries.
    pub fn submit_prepared_engagement(
        &self,
        prepared: PreparedEngagement,
    ) -> Result<EngagementReceipt> {
        if self.transport.cookie_header() != Some(prepared.cookie_binding.as_str()) {
            return Err(Error::CommentPreparationChanged);
        }
        if !prepared.needed() {
            return Err(Error::Schema(
                "engagement already satisfied; no write needed",
            ));
        }
        if now()?.saturating_sub(prepared.prepared_at) > 300 {
            return Err(Error::CommentPreparationChanged);
        }
        let body = form(&prepared)?;
        let action = match prepared.action {
            EngagementAction::LikeVideo => EngagementPost::LikeVideo,
            EngagementAction::FollowUploader => EngagementPost::FollowUploader,
        };
        let raw = self
            .transport
            .post_engagement(action, &body, &prepared.target.bvid)?;
        parse_receipt(raw, &prepared)
    }
    pub fn verify_engagement(&self, receipt: &EngagementReceipt) -> Result<EngagementVerification> {
        validate_target(&receipt.target)?;
        let video: VideoId = receipt.target.bvid.parse()?;
        let current = self.engagement_state(&video, receipt.action)?;
        if current.actor_mid != receipt.actor_mid
            || current.target.aid != receipt.target.aid
            || current.target.bvid != receipt.target.bvid
            || current.target.owner_mid != receipt.target.owner_mid
        {
            return Err(Error::Schema(
                "engagement verification account or target mismatch",
            ));
        }
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn target() -> EngagementTarget {
        EngagementTarget {
            aid: 42,
            bvid: "BV17x411w7KC".into(),
            title: "chosen".into(),
            owner_mid: 88,
            owner_name: "uploader".into(),
        }
    }
    fn prepared(action: EngagementAction) -> PreparedEngagement {
        let state = match action {
            EngagementAction::LikeVideo => like_state(json!(0)).unwrap(),
            EngagementAction::FollowUploader => {
                relation_state(json!({"mid":88,"attribute":0}), 88).unwrap()
            }
        };
        PreparedEngagement {
            actor_mid: 7,
            target: target(),
            action,
            state_before: state,
            csrf: "0123456789abcdef0123456789abcdef".into(),
            cookie_binding: "private-cookie".into(),
            prepared_at: now().unwrap(),
        }
    }
    #[test]
    fn positive_forms_never_toggle_unlike_unfollow_or_arbitrary_uid() {
        let like = prepared(EngagementAction::LikeVideo);
        assert_eq!(
            String::from_utf8(form(&like).unwrap()).unwrap(),
            "aid=42&like=1&csrf=0123456789abcdef0123456789abcdef"
        );
        let follow = prepared(EngagementAction::FollowUploader);
        assert_eq!(
            String::from_utf8(form(&follow).unwrap()).unwrap(),
            "fid=88&act=1&re_src=14&csrf=0123456789abcdef0123456789abcdef"
        );
        assert!(!format!("{follow:?}").contains("private-cookie"));
        assert!(!format!("{follow:?}").contains("0123456789abcdef"));
    }
    #[test]
    fn positive_current_states_become_noop_without_lifetime_like_claims() {
        let liked = like_state(json!(1)).unwrap();
        assert!(liked.satisfied(EngagementAction::LikeVideo));
        let unconfirmed = like_state(json!(0)).unwrap();
        assert_eq!(unconfirmed.like_state_scope.as_deref(), Some("recent_only"));
        assert!(like_state(json!(2)).is_err());
        for attribute in [1, 2, 6] {
            let state = relation_state(json!({"mid":88,"attribute":attribute}), 88).unwrap();
            assert!(state.satisfied(EngagementAction::FollowUploader));
        }
        let mut p = prepared(EngagementAction::LikeVideo);
        p.state_before = liked;
        assert!(!p.needed());
    }
    #[test]
    fn blocked_unknown_and_self_relations_never_become_implicit_follow() {
        let blocked = relation_state(json!({"mid":88,"attribute":128}), 88).unwrap();
        assert_eq!(blocked.blocked, Some(true));
        assert!(ensure_follow_allowed(7, &target(), &blocked).is_err());
        assert!(relation_state(json!({"mid":88,"attribute":4}), 88).is_err());
        assert!(relation_state(json!({"mid":99,"attribute":0}), 88).is_err());
        assert!(
            ensure_follow_allowed(
                88,
                &target(),
                &EngagementState {
                    following: Some(false),
                    blocked: Some(false),
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    #[test]
    fn only_the_exact_empty_relation_sentinel_is_accepted_without_a_target_mid() {
        let data = json!({"mid":0,"attribute":0,"mtime":0,"tag":null,"special":0});
        let state = relation_state(data.clone(), 88).unwrap();
        assert_eq!(state.following, Some(false));
        assert_eq!(state.blocked, Some(false));
        assert_eq!(state.relation_attribute, Some(0));
        let mut prepared = prepared(EngagementAction::FollowUploader);
        prepared.state_before = state;
        assert!(prepared.needed());
        assert_eq!(prepared.target().owner_mid, 88);
        assert!(
            String::from_utf8(form(&prepared).unwrap())
                .unwrap()
                .starts_with("fid=88&")
        );
        for attribute in [1, 2, 6, 128] {
            assert!(relation_state(json!({"mid":0,"attribute":attribute}), 88).is_err());
        }
        assert!(relation_state(json!({"attribute":0}), 88).is_err());
        assert!(relation_state(json!({"mid":99,"attribute":0}), 88).is_err());
        assert!(relation_state(data, 0).is_err());
    }
    #[test]
    fn acceptance_is_not_get_verification_and_unknown_receipts_stay_unknown() {
        let p = prepared(EngagementAction::LikeVideo);
        let receipt = parse_receipt(json!({"code":0,"SESSDATA":"private"}), &p).unwrap();
        assert_eq!(receipt.result_code, 0);
        assert_eq!(receipt.actor_mid, 7);
        assert!(!serde_json::to_string(&receipt).unwrap().contains("private"));
        assert!(matches!(
            parse_receipt(json!({"data":{}}), &p),
            Err(Error::WriteOutcomeUnknown(_))
        ));
        assert!(matches!(
            parse_receipt(json!({"code":65006}), &p),
            Err(Error::Platform(65006))
        ));
        assert!(matches!(
            parse_receipt(json!({"code":-352}), &p),
            Err(Error::RiskControl(-352))
        ));
        assert!(matches!(
            parse_receipt(json!({"code":0,"data":{"need_captcha":true}}), &p),
            Err(Error::WriteOutcomeUnknown(_))
        ));
    }
    #[test]
    fn prepared_capability_cannot_run_with_another_clients_session() {
        let client = Client::new(crate::Config::default()).unwrap();
        assert!(matches!(
            client.submit_prepared_engagement(prepared(EngagementAction::LikeVideo)),
            Err(Error::CommentPreparationChanged)
        ));
        assert!(matches!(
            client.engagement_state(
                &"BV17x411w7KC".parse().unwrap(),
                EngagementAction::LikeVideo
            ),
            Err(Error::LoginRequired)
        ));
    }
}
