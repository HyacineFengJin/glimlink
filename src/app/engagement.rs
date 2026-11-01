// SPDX-License-Identifier: MIT
//! Positive-only, durable video likes and follows bound to a video's uploader.
use super::*;
use crate::outbox::{Operation, Outbox, Status};
use platform_bilibili::{EngagementAction, EngagementTarget, PreparedEngagement};

fn action(method: &str) -> Result<EngagementAction> {
    match method {
        "bilibili.like" => Ok(EngagementAction::LikeVideo),
        "bilibili.follow_uploader" => Ok(EngagementAction::FollowUploader),
        _ => Err("unsupported engagement action".into()),
    }
}
fn json<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}
fn result(record: &Operation) -> Result<Value> {
    let mut value = json!({"operation_id":record.id,"scope":record.scope,"status":record.status,
        "target":record.target,"action":record.message,"write_attempted":record.attempt_started_at.is_some(),
        "created_at":record.created_at,"attempt_started_at":record.attempt_started_at,
        "receipt":record.receipt,"verification":record.verification,"error":record.error});
    // archive/has/like only projects recent likes; an older like can therefore
    // return false before POST reports 65006 (duplicate). Keep the actual ledger
    // rejection and do not invent a new success receipt or a GET verification.
    // Protocol observation: bilibili-plugins/bilibili-api-collect/docs/video/action.md.
    if record.message == "like_video"
        && record.status == Status::Rejected
        && record.error.as_deref() == Some("Bilibili API rejected request (65006)")
    {
        value["outcome"] = json!({"kind":"already_liked","server_code":65006,
            "server_reported_existing_like":true,"new_like_created":false,"readback_verified":false,
            "retry_write":false,"detail":"Server rejected a duplicate like. The preliminary GET only covers recent likes; do not use another operation ID to repeat this action."});
    }
    Ok(value)
}
fn prepare(
    runtime: &Runtime,
    video: &VideoId,
    action: EngagementAction,
) -> Result<PreparedEngagement> {
    match action {
        EngagementAction::LikeVideo => runtime.bili.prepare_video_like(video),
        EngagementAction::FollowUploader => runtime.bili.prepare_uploader_follow(video),
    }
    .map_err(|e| e.to_string())
}

impl Runtime {
    pub(super) fn call_bilibili_engagement(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<Value> {
        let video: VideoId = required_str(params, "video")?
            .parse()
            .map_err(|e: platform_bilibili::Error| e.to_string())?;
        if method == "bilibili.engagement" {
            return self.with_comment_account("bilibili", params, |runtime| {
                let liked = runtime
                    .bili
                    .engagement_state(&video, EngagementAction::LikeVideo)
                    .map_err(|e| e.to_string())?;
                bind_actor(runtime, liked.actor_mid)?;
                let followed = runtime
                    .bili
                    .engagement_state(&video, EngagementAction::FollowUploader)
                    .map_err(|e| e.to_string())?;
                if liked.actor_mid != followed.actor_mid
                    || json(&liked.target)? != json(&followed.target)?
                {
                    return Err("engagement status account or target changed".into());
                }
                Ok(
                    json!({"scope":runtime.scope(Platform::Bilibili),"actor_mid":liked.actor_mid,
                    "target":liked.target,"like_state":liked.state,"follow_state":followed.state,
                    "content_writes":false}),
                )
            });
        }
        let action = action(method)?;
        let id = required_str(params, "operation_id")?;
        let request = json!({"kind":method,"video":video.to_string()});
        self.with_comment_account("bilibili",params,|runtime|{
            let scope=runtime.scope(Platform::Bilibili);
            let outbox=Outbox::open(&runtime.state)?;
            let mut prepared=None;
            let mut record=if outbox.exists(id)?{
                let existing=outbox.load(id,&scope)?;
                if existing.request.as_ref()!=Some(&request){return Err("operation ID belongs to another action or video".into());}
                existing
            }else{
                let next=prepare(runtime,&video,action)?;
                bind_actor(runtime,next.actor_mid())?;
                let mut record=Operation::new(id,scope,next.actor_mid().to_string(),json(next.target())?,
                    match action {EngagementAction::LikeVideo=>"like_video",EngagementAction::FollowUploader=>"follow_uploader"})?;
                record.request=Some(request);
                record.attempt_context=Some(json!({"state_before":next.state_before()}));
                outbox.insert(&record)?;
                prepared=Some(next);
                record
            };
            if matches!(record.status,Status::Verified|Status::AlreadySatisfied|Status::Rejected){return result(&record);}
            if record.status==Status::Attempting {
                record.status=Status::Unknown;
                record.error=Some("prior process interrupted; checking state without repeating the write".into());
                outbox.finish(&record)?;
            }
            if record.status==Status::Prepared {
                let prepared=match prepared {Some(value)=>value,None=>prepare(runtime,&video,action)?};
                bind_actor(runtime,prepared.actor_mid())?;
                if record.actor_id!=prepared.actor_mid().to_string() || record.target!=json(prepared.target())? || prepared.action()!=action {
                    return Err("prepared engagement identity or target changed".into());
                }
                if !prepared.needed(){
                    record.status=Status::AlreadySatisfied;
                    record.verification=Some(json!({"actor_mid":prepared.actor_mid(),"target":prepared.target(),
                        "action":action,"state":prepared.state_before(),"verified":true,"request_sent":false,
                        "checked_at":crate::outbox::now()?}));
                    record.error=None;
                    outbox.finish(&record)?;
                    return result(&record);
                }
                outbox.begin_attempt(&mut record)?;
                match runtime.bili.submit_prepared_engagement(prepared){
                    Ok(receipt)=>{record.status=Status::Sent;record.receipt=Some(json(&receipt)?);record.error=None;}
                    Err(error)=>{
                        record.status=match error{platform_bilibili::Error::Platform(_)|platform_bilibili::Error::RiskControl(_)|platform_bilibili::Error::LoginRequired=>Status::Rejected,_=>Status::Unknown};
                        record.error=Some(error.to_string());
                    }
                }
                outbox.finish(&record)?;
            }
            if matches!(record.status,Status::Sent|Status::Unknown){
                runtime.reconcile_engagement(&outbox,&mut record,&video,action)?;
            }
            result(&record)
        })
    }

    fn reconcile_engagement(
        &self,
        outbox: &Outbox,
        record: &mut Operation,
        video: &VideoId,
        action: EngagementAction,
    ) -> Result<()> {
        let expected: EngagementTarget = serde_json::from_value(record.target.clone())
            .map_err(|_| "invalid engagement target")?;
        match self.bili.engagement_state(video, action) {
            Ok(verification)
                if verification.actor_mid.to_string() == record.actor_id
                    && json(&verification.target)? == json(&expected)? =>
            {
                record.verification = Some(json(&verification)?);
                if verification.verified {
                    record.status = Status::Verified;
                    record.error = None;
                } else {
                    record.error=Some("desired positive state not confirmed by the status endpoint; no write repeated".into());
                }
            }
            Ok(_) => {
                record.error =
                    Some("engagement readback account or target changed; no write repeated".into())
            }
            Err(error) => record.error = Some(error.to_string()),
        }
        outbox.finish(record)
    }
}

fn bind_actor(runtime: &Runtime, actor_mid: u64) -> Result<()> {
    let directory =
        crate::private_state::account_directory(&runtime.state, "bilibili", &runtime.account)?;
    super::auth::bind_identity(&directory, &actor_mid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_like_explains_existing_state_without_inventing_verification() {
        let mut record = Operation::new(
            "duplicate-like",
            Scope {
                platform: Platform::Bilibili,
                account: "test".into(),
            },
            "42".into(),
            json!({"bvid":"BV17x411w7KC"}),
            "like_video",
        )
        .unwrap();
        record.status = Status::Rejected;
        record.attempt_started_at = Some(record.created_at);
        record.error = Some("Bilibili API rejected request (65006)".into());
        let value = result(&record).unwrap();
        assert_eq!(value["status"], "rejected");
        assert_eq!(value["outcome"]["kind"], "already_liked");
        assert_eq!(value["outcome"]["retry_write"], false);
        assert_eq!(value["outcome"]["readback_verified"], false);
        assert!(value["receipt"].is_null());
        assert!(value["verification"].is_null());
        record.message = "follow_uploader".into();
        assert!(result(&record).unwrap().get("outcome").is_none());
        record.message = "like_video".into();
        record.error = Some("Bilibili API rejected request (-403)".into());
        assert!(result(&record).unwrap().get("outcome").is_none());
    }
    #[test]
    fn terminal_positive_operations_are_cached_without_network_or_replay() {
        use std::os::unix::fs::OpenOptionsExt;
        let state = std::env::temp_dir().join(format!(
            "engagement-cache-{}-{}",
            std::process::id(),
            crate::outbox::now().unwrap()
        ));
        let directory =
            crate::private_state::account_directory(&state, "bilibili", "test").unwrap();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("session.json"))
            .unwrap();
        serde_json::to_writer(file,&json!({"schema":1,"mid":42,"cookies":{"DedeUserID":"42",
            "DedeUserID__ckMd5":"synthetic-hash","SESSDATA":"synthetic-private-session",
            "bili_jct":"0123456789abcdef0123456789abcdef"},"refresh_token":"","authenticated_at":0})).unwrap();
        let outbox = Outbox::open(&state).unwrap();
        let scope = Scope {
            platform: Platform::Bilibili,
            account: "test".into(),
        };
        for (id, method, noop) in [
            ("already-followed", "bilibili.follow_uploader", true),
            ("verified-like", "bilibili.like", false),
        ] {
            let mut record=Operation::new(id,scope.clone(),"42".into(),
                json!({"aid":7,"bvid":"BV17x411w7KC","title":"target","owner_mid":88,"owner_name":"uploader"}),
                if noop{"follow_uploader"}else{"like_video"}).unwrap();
            record.request = Some(json!({"kind":method,"video":"BV17x411w7KC"}));
            outbox.insert(&record).unwrap();
            if noop {
                record.status = Status::AlreadySatisfied;
                outbox.finish(&record).unwrap();
            } else {
                outbox.begin_attempt(&mut record).unwrap();
                record.status = Status::Sent;
                outbox.finish(&record).unwrap();
                record.status = Status::Verified;
                outbox.finish(&record).unwrap();
            }
            assert!(outbox.begin_attempt(&mut record).is_err());
            let mut runtime = Runtime::open(&state, "public", None).unwrap();
            let params = json!({"video":"BV17x411w7KC","operation_id":id});
            for _ in 0..2 {
                let result = runtime.call(method, &params).unwrap();
                assert_eq!(result["write_attempted"], !noop);
                assert_eq!(
                    result["status"],
                    if noop {
                        "already_satisfied"
                    } else {
                        "verified"
                    }
                );
                assert!(!result.to_string().contains("synthetic-private"));
                assert_eq!(runtime.scope(Platform::Bilibili).account, "public");
            }
            assert!(
                runtime
                    .call(method, &json!({"video":"av8","operation_id":id}))
                    .is_err()
            );
            assert_eq!(runtime.scope(Platform::Bilibili).account, "public");
        }
        fs::remove_dir_all(state).unwrap();
    }
}
