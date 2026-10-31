// SPDX-License-Identifier: MIT
//! Exact-reference sends reuse the existing durable submit/readback pipeline.
//! Preparing a task never selects the account's newest publication.
use super::*;
use super::{sending::CommentRequest, tasks::TaskTarget};
use crate::outbox::{Operation, Outbox, Status};

fn serialized<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn task_request(target: &TaskTarget, params: &Value) -> Result<CommentRequest> {
    let (name, root, parent) = match target {
        TaskTarget::Video { .. } => ("bilibili", 0, 0),
        TaskTarget::BiliComment { root, parent, .. } => ("bilibili", *root, *parent),
        TaskTarget::Thread { .. } => ("tieba", 0, 0),
        TaskTarget::Creator { .. } | TaskTarget::TiebaPost { .. } => {
            return Err("unsupported send target".into());
        }
    };
    let mut request = json!({"root":root,"parent":parent});
    if let Some(image) = params.get("image_resource") {
        request["image_resource"] = image.clone();
    }
    CommentRequest::from_params(name, &request)
}

fn frozen_request(request: &CommentRequest, target: &TaskTarget) -> Result<Value> {
    let mut value = serialized(request)?;
    // The old submit pipeline reads root/parent/image_resource and deliberately
    // ignores this extension. Freeze the original semantic target as well, so
    // an operation cannot silently become another task on the same content.
    value["task_target"] = serialized(target)?;
    Ok(value)
}

fn image_source(params: &Value) -> Result<Option<&str>> {
    if params.get("image_ref").is_some() && params.get("image_resource").is_some() {
        return Err("image_ref and image_resource are mutually exclusive".into());
    }
    match params.get("image_ref") {
        None => Ok(None),
        Some(value) => {
            let id = value
                .as_str()
                .filter(|id| {
                    id.len() == 32
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
                .ok_or("image_ref must be an opaque image reference")?;
            Ok(Some(id))
        }
    }
}

fn replay_request(
    record: &Operation,
    target: &TaskTarget,
    params: &Value,
    source: Option<&str>,
) -> Result<CommentRequest> {
    let mut request = params.clone();
    if let Some(source) = source {
        let frozen = record
            .request
            .as_ref()
            .ok_or("operation has no saved image source")?;
        if frozen.get("image_ref").and_then(Value::as_str) != Some(source) {
            return Err("operation_id already belongs to another original image reference".into());
        }
        request["image_resource"] = frozen
            .get("image_resource")
            .filter(|value| value.as_str().is_some())
            .ok_or("operation has no resolved image resource")?
            .clone();
    }
    task_request(target, &request)
}

fn check_cached_request(
    record: &Operation,
    requested: &TaskTarget,
    message: &str,
    request: &CommentRequest,
    image_ref: Option<&str>,
) -> Result<()> {
    if record.message != message || CommentRequest::from_record(record)? != *request {
        return Err(
            "operation_id already belongs to another message, image or reply target".into(),
        );
    }
    if let Some(payload) = &record.request {
        let object = payload.as_object().ok_or("invalid saved comment request")?;
        // Do not reinterpret another action's ledger (for example a video like)
        // as a comment merely because some target fields overlap.
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "root" | "parent" | "image_resource" | "image_ref" | "task_target"
            )
        }) {
            return Err("operation_id belongs to a different action".into());
        }
        match (object.get("image_ref"), image_ref) {
            (None, None) => {}
            (Some(Value::String(saved)), Some(requested)) if saved == requested => {}
            _ => {
                return Err(
                    "operation_id already belongs to another original image reference".into(),
                );
            }
        }
        if let Some(target) = object.get("task_target") {
            if target != &serialized(requested)? {
                return Err("operation_id already belongs to another exact target".into());
            }
        }
    }
    let same_target = match requested {
        TaskTarget::Video { video } | TaskTarget::BiliComment { video, .. } => {
            let expected: platform_bilibili::CommentTarget =
                serde_json::from_value(record.target.clone())
                    .map_err(|_| "operation_id is not a saved Bilibili comment")?;
            let video: VideoId = video
                .parse()
                .map_err(|e: platform_bilibili::Error| e.to_string())?;
            expected.actor_mid().to_string() == record.actor_id
                && match video {
                    VideoId::Bv(bvid) => expected.bvid == bvid,
                    VideoId::Av(aid) => expected.aid == aid,
                }
        }
        TaskTarget::Thread { thread } => {
            let expected: platform_tieba::ReplyTarget =
                serde_json::from_value(record.target.clone())
                    .map_err(|_| "operation_id is not a saved Tieba reply")?;
            let thread: platform_tieba::ThreadId = thread
                .parse::<platform_tieba::ThreadId>()
                .map_err(|e| e.to_string())?;
            expected.owner_uid.to_string() == record.actor_id && expected.thread_id == thread.0
        }
        TaskTarget::Creator { .. } | TaskTarget::TiebaPost { .. } => false,
    };
    if !same_target {
        return Err("operation_id belongs to another saved content target or actor".into());
    }
    Ok(())
}

fn reply_semantics(target: &TaskTarget) -> Value {
    match target {
        TaskTarget::Video { .. } => json!({"kind":"video_root_comment"}),
        TaskTarget::BiliComment { root, parent, .. } => {
            json!({"kind":"video_comment_reply","root":root,"parent":parent})
        }
        TaskTarget::Thread { .. } => json!({"kind":"ordinary_thread_reply","floor_reply":false}),
        TaskTarget::TiebaPost { post, .. } => {
            json!({"kind":"floor_reply","supported":false,"requested_post":post})
        }
        TaskTarget::Creator { .. } => json!({"kind":"unsupported"}),
    }
}

fn unsupported(
    scope: &Scope,
    target: &TaskTarget,
    reference: &str,
    id: &str,
    detail: &str,
) -> Value {
    json!({"status":"unsupported","scope":scope,"target_ref":reference,"operation_id":id,
        "request_sent":false,"verified":false,"reply_semantics":reply_semantics(target),"detail":detail})
}

fn outcome(record: Value, target: &TaskTarget, reference: &str) -> Value {
    let attempted = record
        .get("attempt_started_at")
        .is_some_and(|v| !v.is_null());
    let mut value = super::sending::compact(record);
    value["target_ref"] = json!(reference);
    value["reply_semantics"] = reply_semantics(target);
    value["attempt_recorded"] = json!(attempted);
    value["request_sent"] = match value["status"].as_str() {
        Some("sent" | "verified") => json!(true),
        Some("prepared") => json!(false),
        _ => Value::Null,
    };
    value["verified"] = json!(value["status"] == "verified");
    value["unknown"] = json!(matches!(
        value["status"].as_str(),
        Some("unknown" | "attempting")
    ));
    value
}

impl Runtime {
    pub(super) fn send_task_target(
        &mut self,
        target: &TaskTarget,
        reference: &str,
        params: &Value,
    ) -> Result<Value> {
        // Keep this helper safe even when called outside task_send: the object
        // and active account must be exactly the object saved by the Runtime.
        let (saved_scope, saved_target) = self.load_task_target(reference)?;
        let scope = self.scope(target.platform());
        if saved_scope != scope || saved_target != *target {
            return Err("send target differs from its saved reference or active account".into());
        }
        super::tasks::check_account(params, &scope)?;
        if params
            .get("target_ref")
            .is_some_and(|v| v.as_str() != Some(reference))
        {
            return Err("target_ref differs from the resolved send target".into());
        }
        let message = required_str(params, "message")?;
        let id = required_str(params, "operation_id")?;
        if message.is_empty() || message.chars().count() > 1000 || message.contains('\0') {
            return Err("comment must contain 1..1000 characters".into());
        }
        if id.is_empty()
            || id.len() > 96
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err("operation ID must contain 1..96 ASCII letters, digits, '-' or '_'".into());
        }
        let source_image = image_source(params)?;
        if scope.account == "public" {
            return Ok(unsupported(
                &scope,
                target,
                reference,
                id,
                "Sending requires a target opened under a saved personal account",
            ));
        }
        if matches!(target, TaskTarget::Creator { .. }) {
            return Ok(unsupported(
                &scope,
                target,
                reference,
                id,
                "Creator profiles are not comment or direct-message targets",
            ));
        }
        if let TaskTarget::TiebaPost { thread, .. } = target {
            let mut result = unsupported(
                &scope,
                target,
                reference,
                id,
                "Replies beneath a Tieba floor are unsupported. No ordinary topic reply was substituted or sent.",
            );
            result["parent_thread_ref"] = json!(self.save_task_target(&TaskTarget::Thread {
                thread: thread.clone()
            })?);
            return Ok(result);
        }
        if scope.platform == Platform::Tieba
            && (params.get("image_resource").is_some() || source_image.is_some())
        {
            return Ok(unsupported(
                &scope,
                target,
                reference,
                id,
                "Tieba sending currently supports ordinary text topic replies only; image upload is unavailable",
            ));
        }
        if (params.get("image_resource").is_some() || source_image.is_some()) && id.len() > 90 {
            return Err("image comment operation_id must be at most 90 characters".into());
        }
        // Validate legacy attachment input before any ownership API reads;
        // image_ref is resolved only for a genuinely new prepared operation.
        task_request(target, params)?;
        let name = if scope.platform == Platform::Bilibili {
            "bilibili"
        } else {
            "tieba"
        };
        let outbox = Outbox::open(&self.state)?;
        let record = if outbox.exists(id)? {
            let existing = outbox.load(id, &scope)?;
            let request = replay_request(&existing, target, params, source_image)?;
            check_cached_request(&existing, target, message, &request, source_image)?;
            existing
        } else {
            // All ownership/capability checks below are reads. Unsupported
            // requests never reserve a Prepared operation or upload an image.
            if let Some(image) = params.get("image_resource").and_then(Value::as_str) {
                self.comment_image_bytes(image)?;
            }
            let mut record = match target {
                TaskTarget::Video { video } | TaskTarget::BiliComment { video, .. } => {
                    let video: VideoId = video
                        .parse()
                        .map_err(|e: platform_bilibili::Error| e.to_string())?;
                    let prepared = match target {
                        TaskTarget::BiliComment { root, parent, .. } => self
                            .bili
                            .prepare_video_reply(&video, *root, *parent, message, Vec::new()),
                        _ => self.bili.prepare_video_comment_with_pictures(
                            &video,
                            message,
                            Vec::new(),
                        ),
                    };
                    let prepared = match prepared {
                        Ok(prepared) => prepared,
                        Err(platform_bilibili::Error::NotOwnVideo) => {
                            return Ok(unsupported(
                                &scope,
                                target,
                                reference,
                                id,
                                "Only this account's own video and own root/parent comments are supported send targets",
                            ));
                        }
                        Err(error) => return Err(error.to_string()),
                    };
                    Operation::new(
                        id,
                        scope.clone(),
                        prepared.target().actor_mid().to_string(),
                        serialized(prepared.target())?,
                        message,
                    )?
                }
                TaskTarget::Thread { thread } => {
                    let thread: platform_tieba::ThreadId = thread
                        .parse::<platform_tieba::ThreadId>()
                        .map_err(|e| e.to_string())?;
                    let (path, mut session) = self.tieba_comment_session()?;
                    let api = platform_tieba::AuthClient::new().map_err(|e| e.to_string())?;
                    let prepared = api.prepare_own_reply(&mut session, thread, message);
                    session.save(&path).map_err(|e| e.to_string())?;
                    let prepared = match prepared {
                        Ok(prepared) => prepared,
                        Err(platform_tieba::Error::Schema(
                            "reply target is not owned by current account"
                            | "reply target identity or forum",
                        )) => {
                            return Ok(unsupported(
                                &scope,
                                target,
                                reference,
                                id,
                                "Only ordinary replies to this account's own topics are supported; topic ownership or required forum identity could not be established",
                            ));
                        }
                        Err(error) => return Err(error.to_string()),
                    };
                    Operation::new(
                        id,
                        scope.clone(),
                        prepared.target().owner_uid.to_string(),
                        serialized(prepared.target())?,
                        message,
                    )?
                }
                TaskTarget::Creator { .. } | TaskTarget::TiebaPost { .. } => unreachable!(),
            };
            let mut resolved = params.clone();
            if let Some(image) = source_image {
                resolved["image_resource"] = json!(self.resolve_send_image(image)?);
            }
            let request = task_request(target, &resolved)?;
            let mut frozen = frozen_request(&request, target)?;
            if let Some(image) = source_image {
                frozen["image_ref"] = json!(image);
            }
            record.request = Some(frozen);
            outbox.insert(&record)?;
            record
        };
        let mut value = serialized(&record)?;
        if record.status == Status::Prepared {
            match self.submit_comment(name, id) {
                Ok(result) => value = result,
                Err(error) => {
                    // A persistence/readback failure after dispatch must retain
                    // the durable attempted state in the task response.
                    let stored = outbox.load(id, &scope)?;
                    if stored.status == Status::Prepared {
                        return Err(error);
                    }
                    value = serialized(&stored)?;
                    value["execution_error"] = json!(error);
                }
            }
        }
        let execution_error = value.get("execution_error").cloned();
        if matches!(value["status"].as_str(), Some("sent" | "attempting"))
            || (name == "tieba" && value["status"] == "unknown")
        {
            match self.verify_comment(name, id) {
                Ok(result) => value = result,
                Err(error) => {
                    value = serialized(&outbox.load(id, &scope)?)?;
                    value["verification_error"] = json!(error);
                }
            }
        }
        let verification_error = value.get("verification_error").cloned();
        let mut result = outcome(value, target, reference);
        if let Some(error) = execution_error {
            result["execution_error"] = error;
        }
        if let Some(error) = verification_error {
            result["verification_error"] = error;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "task-send-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn video() -> TaskTarget {
        TaskTarget::Video {
            video: "BV17x411w7KC".into(),
        }
    }
    fn bili_target() -> Value {
        json!({"aid":170001,"bvid":"BV17x411w7KC","title":"saved own video","owner_mid":42})
    }
    fn seed(
        runtime: &Runtime,
        target: Value,
        request: Option<Value>,
        status: Status,
        id: &str,
    ) -> Operation {
        let platform = if target.get("bvid").is_some() {
            Platform::Bilibili
        } else {
            Platform::Tieba
        };
        let outbox = Outbox::open(&runtime.state).unwrap();
        let mut record = Operation::new(
            id,
            runtime.scope(platform),
            "42".into(),
            target,
            "晚安喵（test）",
        )
        .unwrap();
        record.request = request;
        outbox.insert(&record).unwrap();
        outbox.begin_attempt(&mut record).unwrap();
        if status != Status::Attempting {
            record.status = if status == Status::Verified {
                Status::Sent
            } else {
                status.clone()
            };
            outbox.finish(&record).unwrap();
            if status == Status::Verified {
                record.status = Status::Verified;
                record.verification = Some(json!({"author":42,"message":record.message}));
                outbox.finish(&record).unwrap();
            }
        }
        record
    }

    #[test]
    fn legacy_unknown_survives_restart_without_network_and_cannot_retarget() {
        let directory = directory();
        let runtime = Runtime::open(&directory, "a", None).unwrap();
        let target = video();
        let reference = runtime.save_task_target(&target).unwrap();
        let record = seed(
            &runtime,
            bili_target(),
            None,
            Status::Unknown,
            "legacy-unknown",
        );
        drop(runtime);
        let mut runtime = Runtime::open(&directory, "a", None).unwrap();
        let params = json!({"message":"晚安喵（test）","operation_id":record.id});
        let result = runtime
            .send_task_target(&target, &reference, &params)
            .unwrap();
        assert_eq!(result["status"], "unknown");
        assert_eq!(result["target_ref"], reference);
        assert_eq!(result["unknown"], true);
        assert_eq!(result["verified"], false);
        assert_eq!(result["attempt_recorded"], true);
        assert_eq!(result["request_sent"], Value::Null);
        let stored = Outbox::open(&runtime.state)
            .unwrap()
            .load(&record.id, &record.scope)
            .unwrap();
        assert_eq!(stored.attempt_started_at, record.attempt_started_at);
        let another = TaskTarget::Video {
            video: "av170002".into(),
        };
        let another_ref = runtime.save_task_target(&another).unwrap();
        assert!(
            runtime
                .send_task_target(&another, &another_ref, &params)
                .is_err()
        );
        assert!(
            runtime
                .send_task_target(
                    &target,
                    &reference,
                    &json!({"message":"changed","operation_id":record.id})
                )
                .is_err()
        );
        drop(runtime);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn interrupted_attempt_becomes_unknown_without_repeating_dispatch() {
        let directory = directory();
        let mut runtime = Runtime::open(&directory, "a", None).unwrap();
        let target = video();
        let reference = runtime.save_task_target(&target).unwrap();
        let record = seed(
            &runtime,
            bili_target(),
            None,
            Status::Attempting,
            "interrupted",
        );
        let params = json!({"message":record.message,"operation_id":record.id});
        let result = runtime
            .send_task_target(&target, &reference, &params)
            .unwrap();
        assert_eq!(result["status"], "unknown");
        let result = runtime
            .send_task_target(&target, &reference, &params)
            .unwrap();
        assert_eq!(result["status"], "unknown");
        assert_eq!(
            Outbox::open(&runtime.state)
                .unwrap()
                .load(&record.id, &record.scope)
                .unwrap()
                .attempt_started_at,
            record.attempt_started_at
        );
        drop(runtime);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cached_comment_requires_exact_reply_and_image_even_when_image_is_evicted() {
        let directory = directory();
        let mut runtime = Runtime::open(&directory, "a", None).unwrap();
        let target = TaskTarget::BiliComment {
            video: "BV17x411w7KC".into(),
            root: 7,
            parent: 8,
        };
        let reference = runtime.save_task_target(&target).unwrap();
        let image = "a".repeat(32);
        let params =
            json!({"message":"晚安喵（test）","operation_id":"exact-image","image_resource":image});
        let request = task_request(&target, &params).unwrap();
        seed(
            &runtime,
            bili_target(),
            Some(frozen_request(&request, &target).unwrap()),
            Status::Verified,
            "exact-image",
        );
        let result = runtime
            .send_task_target(&target, &reference, &params)
            .unwrap();
        assert_eq!(result["status"], "verified");
        assert_eq!(result["reply_semantics"]["parent"], 8);
        let mut changed = params.clone();
        changed["image_resource"] = json!("b".repeat(32));
        assert!(
            runtime
                .send_task_target(&target, &reference, &changed)
                .is_err()
        );
        let another = TaskTarget::BiliComment {
            video: "BV17x411w7KC".into(),
            root: 7,
            parent: 9,
        };
        let another_ref = runtime.save_task_target(&another).unwrap();
        assert!(
            runtime
                .send_task_target(&another, &another_ref, &params)
                .is_err()
        );
        drop(runtime);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn tieba_post_reference_never_downgrades_to_ordinary_topic_reply() {
        let directory = directory();
        let mut runtime = Runtime::open(&directory, "a", None).unwrap();
        let target = TaskTarget::TiebaPost {
            thread: "700".into(),
            post: 12,
        };
        let reference = runtime.save_task_target(&target).unwrap();
        let params = json!({"message":"晚安喵（test）","operation_id":"topic-reply"});
        let result = runtime
            .send_task_target(&target, &reference, &params)
            .unwrap();
        assert_eq!(result["status"], "unsupported");
        assert_eq!(result["request_sent"], false);
        assert_eq!(result["reply_semantics"]["kind"], "floor_reply");
        assert_eq!(result["reply_semantics"]["supported"], false);
        assert_eq!(result["reply_semantics"]["requested_post"], 12);
        assert!(
            !Outbox::open(&runtime.state)
                .unwrap()
                .exists("topic-reply")
                .unwrap()
        );
        let (_, parent) = runtime
            .load_task_target(result["parent_thread_ref"].as_str().unwrap())
            .unwrap();
        assert!(
            parent
                == TaskTarget::Thread {
                    thread: "700".into()
                }
        );
        drop(runtime);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unsupported_capabilities_never_prepare_and_saved_reference_cannot_change() {
        let directory = directory();
        let mut runtime = Runtime::open(&directory, "a", None).unwrap();
        let target = TaskTarget::Creator { mid: 42 };
        let reference = runtime.save_task_target(&target).unwrap();
        let params = json!({"message":"晚安喵（test）","operation_id":"unsupported"});
        let result = runtime
            .send_task_target(&target, &reference, &params)
            .unwrap();
        assert_eq!(result["status"], "unsupported");
        assert!(
            !Outbox::open(&runtime.state)
                .unwrap()
                .exists("unsupported")
                .unwrap()
        );
        assert!(
            runtime
                .send_task_target(&video(), &reference, &params)
                .is_err()
        );
        let topic = TaskTarget::Thread {
            thread: "700".into(),
        };
        let topic_ref = runtime.save_task_target(&topic).unwrap();
        let result = runtime.send_task_target(&topic, &topic_ref, &json!({"message":"晚安喵（test）","operation_id":"unsupported-image","image_resource":"a".repeat(32)})).unwrap();
        assert_eq!(result["status"], "unsupported");
        assert!(
            !Outbox::open(&runtime.state)
                .unwrap()
                .exists("unsupported-image")
                .unwrap()
        );
        drop(runtime);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn original_image_ref_is_frozen_and_cached_replay_survives_media_eviction() {
        for (index, status) in [Status::Verified, Status::Unknown].into_iter().enumerate() {
            let directory = directory();
            fs::create_dir_all(&directory).unwrap();
            let fixture = directory.join("source.png");
            image::DynamicImage::new_rgb8(8, 8)
                .save_with_format(&fixture, image::ImageFormat::Png)
                .unwrap();
            let mut runtime = Runtime::open(&directory, "a", None).unwrap();
            let target = video();
            let reference = runtime.save_task_target(&target).unwrap();
            let source = runtime
                .import_file(&fixture, "application/octet-stream")
                .unwrap();
            let resolved = runtime.resolve_send_image(&source.id).unwrap();
            let id = format!("frozen-source-{index}");
            let params =
                json!({"message":"晚安喵（test）","operation_id":id,"image_ref":source.id});
            let mut private = params.clone();
            private["image_resource"] = json!(resolved);
            let request = task_request(&target, &private).unwrap();
            let mut frozen = frozen_request(&request, &target).unwrap();
            frozen["image_ref"] = json!(source.id);
            seed(&runtime, bili_target(), Some(frozen), status.clone(), &id);
            runtime
                .store
                .remove(&runtime.scope(Platform::Local), &source.id)
                .unwrap();
            runtime
                .store
                .remove(&runtime.scope(Platform::Local), &resolved)
                .unwrap();
            let result = runtime
                .send_task_target(&target, &reference, &params)
                .unwrap();
            assert_eq!(result["status"], serialized(&status).unwrap());
            assert_eq!(result["request"]["image_ref"], source.id);
            assert_eq!(result["request"]["image_resource"], resolved);
            let mut changed = params.clone();
            changed["image_ref"] = json!("f".repeat(32));
            assert!(
                runtime
                    .send_task_target(&target, &reference, &changed)
                    .is_err()
            );
            let mut legacy = params.clone();
            legacy.as_object_mut().unwrap().remove("image_ref");
            legacy["image_resource"] = json!(resolved);
            assert!(
                runtime
                    .send_task_target(&target, &reference, &legacy)
                    .is_err()
            );
            let mut both = params.clone();
            both["image_resource"] = json!(resolved);
            assert!(
                runtime
                    .send_task_target(&target, &reference, &both)
                    .is_err()
            );
            drop(runtime);
            fs::remove_dir_all(directory).unwrap();
        }
    }
}
