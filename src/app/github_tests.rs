// SPDX-License-Identifier: MIT
//! Entirely synthetic GitHub workflows. No test loads host credentials or makes HTTP.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SHA_HEAD: &str = "1111111111111111111111111111111111111111";
const SHA_BASE: &str = "2222222222222222222222222222222222222222";
struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "github-task-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
    fn runtime(&self) -> Runtime {
        Runtime::open(&self.0, "public", None).unwrap()
    }
    fn scope() -> Scope {
        Scope {
            platform: Platform::Github,
            account: "fixture-account".into(),
        }
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn repository() -> Target {
    Target::Repository {
        owner: "fixture-owner".into(),
        repo: "fixture-repo".into(),
    }
}
fn issue(number: u64) -> Target {
    Target::Issue {
        owner: "fixture-owner".into(),
        repo: "fixture-repo".into(),
        number,
        pull: false,
    }
}
fn request() -> Value {
    json!({"action":"github_issue_comment","owner":"fixture-owner","repo":"fixture-repo","issue":7,"actor_id":42,"actor_login":"fixture-user","reply_semantics":"new_issue_comment_no_nested_thread"})
}
fn seed_rate_limit(rt: &mut Runtime, scope: &Scope, reset_at: u64) -> Value {
    rt.with_task_scope(scope, |rt| {
        let error = rt.github_sdk_error(platform_github::Error::RateLimited {
            retry_after_seconds: None,
            reset_at: Some(reset_at),
        });
        Ok(decode_task_error(&error).unwrap())
    })
    .unwrap()
}
fn record(lab: &Lab, rt: &Runtime, status: Status) -> String {
    let reference = rt.locators.put(&Lab::scope(), TARGET, &issue(7)).unwrap();
    let outbox = Outbox::open(&lab.0).unwrap();
    let mut record = Operation::new(
        "fixture-comment",
        Lab::scope(),
        "42".into(),
        serde_json::to_value(issue(7)).unwrap(),
        "one exact comment",
    )
    .unwrap();
    record.request = Some(request());
    outbox.insert(&record).unwrap();
    outbox.begin_attempt(&mut record).unwrap();
    if status != Status::Attempting {
        record.status = status;
        record.receipt = Some(
            json!({"id":99,"html_url":"https://github.com/fixture-owner/fixture-repo/issues/7#issuecomment-99"}),
        );
        outbox.finish(&record).unwrap();
    }
    reference
}

#[test]
fn ordinary_repository_issue_and_frozen_file_links_preserve_exact_semantics() {
    assert!(
        matches!(parse_reference("https://github.com/Owner/Repo").unwrap(), Parsed::Target(Target::Repository{owner,repo}) if owner=="owner" && repo=="repo")
    );
    assert!(matches!(
        parse_reference("https://github.com/o/r/pull/7#issuecomment-19").unwrap(),
        Parsed::Target(Target::Comment {
            number: 7,
            pull: true,
            comment: 19,
            ..
        })
    ));
    let link = format!("https://github.com/o/r/blob/{SHA_HEAD}/src/a%20b.rs");
    assert!(
        matches!(parse_reference(&link).unwrap(),Parsed::Target(Target::File{path,revision,directory:false,..}) if path=="src/a b.rs" && revision==SHA_HEAD)
    );
    for url in [
        "http://github.com/o/r",
        "https://github.com.evil.invalid/o/r",
        "https://github.com@evil.invalid/o/r",
        "https://api.github.com/repos/o/r",
        "https://github.com:444/o/r",
        "https://github.com/o/r/issues/0",
        "https://github.com/o/r/issues/7#discussion_r9",
    ] {
        assert!(parse_reference(url).is_err(), "{url}");
    }
    assert!(
        parse_reference(&format!(
            "https://github.com/o/r/blob/{SHA_HEAD}/%2e%2e/private"
        ))
        .is_err()
    );
}

#[test]
fn ambiguous_slash_branch_link_returns_a_repository_recovery_target_without_guessing() {
    let lab = Lab::new();
    let mut rt = lab.runtime();
    let data = rt.call_github_task("github.open", &json!({"reference":"https://github.com/fixture-owner/fixture-repo/blob/feature/with/slashes/src/lib.rs","account":"public"})).unwrap();
    assert_eq!(data["status"], "unsupported");
    assert_eq!(data["content_observed"], false);
    let reference = data["target_ref"].as_str().unwrap();
    let (scope, target) = rt
        .load_github_target(reference, &json!({"account":"public"}))
        .unwrap();
    assert_eq!(scope.account, "public");
    assert_eq!(target, repository());
    assert!(data["detail"].as_str().unwrap().contains("did not guess"));
}

#[test]
fn cached_list_cursor_survives_restart_and_limit_changes_without_sessions_or_repagination() {
    let lab = Lab::new();
    let mut rt = lab.runtime();
    let scope = Lab::scope();
    let rate = seed_rate_limit(&mut rt, &scope, crate::outbox::now().unwrap() + 3600);
    assert_eq!(rate["status"], "rate_limited");
    let mut items = Vec::new();
    for number in 1..=3 {
        items.push(
            json!({"id":number,"number":number,"title":format!("issue {number}"),"is_pull_request":false}),
        );
    }
    let encoded = serde_json::to_vec(&items).unwrap();
    let resource = rt
        .store
        .import(
            &scope,
            "github-pending-fixture",
            "application/json",
            &mut encoded.as_slice(),
        )
        .unwrap();
    let cursor = Cursor {
        kind: ListKind::Issues,
        target: Some(repository()),
        page: 1,
        seen: VecDeque::new(),
        pending: Some(Pending {
            resource: resource.id.clone(),
            offset: 0,
            next_page: None,
            context: json!({"server_has_more":false}),
        }),
        changes: None,
    };
    let reference = rt.locators.put(&scope, CONTINUATION, &cursor).unwrap();
    assert!(
        rt.call_github_task(
            "github.list",
            &json!({"continuation":reference,"account":"somebody-else"})
        )
        .is_err()
    );
    let first = rt
        .call_github_task("github.list", &json!({"continuation":reference,"limit":1}))
        .unwrap();
    assert_eq!(first["items"][0]["number"], 1);
    assert_eq!(first["has_more"], true);
    assert_eq!(rt.account, "public");
    let next = first["continuation"].as_str().unwrap().to_owned();
    let saved: Cursor = rt.locators.get(&scope, CONTINUATION, &next).unwrap();
    assert_eq!(
        saved.pending.unwrap().resource,
        resource.id,
        "cached pagination should reuse the same body resource"
    );
    drop(rt);
    let mut rt = lab.runtime();
    let rest = rt
        .call_github_task("github.list", &json!({"continuation":next,"limit":20}))
        .unwrap();
    assert_eq!(
        rest["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["number"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(rest["has_more"], false);
    assert_eq!(rest["collection_complete"], true);
    assert!(
        !lab.0.join("accounts/github").exists(),
        "cached list reads must not discover/create authentication state"
    );
}

#[test]
fn cached_send_replay_needs_neither_a_parseable_session_nor_a_network_client() {
    let lab = Lab::new();
    let mut rt = lab.runtime();
    let reference = record(&lab, &rt, Status::Sent);
    seed_rate_limit(&mut rt, &Lab::scope(), crate::outbox::now().unwrap() + 3600);
    let directory = account_directory(&lab.0, "github", &Lab::scope().account).unwrap();
    save_private_new(
        &directory.join("session.json"),
        &json!({"deliberately_invalid":"must not be loaded during replay"}),
    )
    .unwrap();
    let args = json!({"target_ref":reference,"operation_id":"fixture-comment","message":"one exact comment"});
    let data = rt.call_github_task("github.comment", &args).unwrap();
    assert_eq!(data["status"], "sent");
    assert_eq!(data["reused_operation"], true);
    assert_eq!(data["readback_verified"], false);
    let mut changed = args.clone();
    changed["message"] = json!("changed text");
    assert!(rt.call_github_task("github.comment", &changed).is_err());
    let mut changed = args.clone();
    changed["account"] = json!("other");
    assert!(rt.call_github_task("github.comment", &changed).is_err());
    let other = rt.locators.put(&Lab::scope(), TARGET, &issue(8)).unwrap();
    let mut changed = args;
    changed["target_ref"] = json!(other);
    assert!(rt.call_github_task("github.comment", &changed).is_err());
    assert_eq!(
        Outbox::open(&lab.0)
            .unwrap()
            .load("fixture-comment", &Lab::scope())
            .unwrap()
            .status,
        Status::Sent
    );
}

#[test]
fn interrupted_comment_attempt_becomes_unknown_and_cannot_be_retried() {
    let lab = Lab::new();
    let mut rt = lab.runtime();
    let reference = record(&lab, &rt, Status::Attempting);
    let args = json!({"target_ref":reference,"operation_id":"fixture-comment","message":"one exact comment"});
    for _ in 0..2 {
        let data = rt.call_github_task("github.comment", &args).unwrap();
        assert_eq!(data["status"], "unknown");
        assert_eq!(data["unknown"], true);
        assert_eq!(data["request_sent"], Value::Null);
        assert_eq!(data["readback_verified"], false);
    }
    assert_eq!(
        Outbox::open(&lab.0)
            .unwrap()
            .load("fixture-comment", &Lab::scope())
            .unwrap()
            .status,
        Status::Unknown
    );
}

#[test]
fn exact_comment_readback_requires_id_actor_body_and_repository_issue_relationship() {
    let data = json!({"id":99,"body":"one exact comment","user":{"id":42},"issue_url":"https://api.github.com/repos/fixture-owner/fixture-repo/issues/7"});
    assert!(exact_readback(
        &data,
        99,
        42,
        "fixture-owner",
        "fixture-repo",
        7,
        "one exact comment"
    ));
    for (pointer, value) in [
        ("/id", json!(100)),
        ("/user/id", json!(43)),
        ("/body", json!("one exact comment\n")),
        (
            "/issue_url",
            json!("https://api.github.com/repos/fixture-owner/other-repo/issues/7"),
        ),
        (
            "/issue_url",
            json!("https://api.github.com/repos/fixture-owner/fixture-repo/issues/8"),
        ),
    ] {
        let mut wrong = data.clone();
        *wrong.pointer_mut(pointer).unwrap() = value;
        assert!(
            !exact_readback(
                &wrong,
                99,
                42,
                "fixture-owner",
                "fixture-repo",
                7,
                "one exact comment"
            ),
            "{pointer}"
        );
    }
}

#[test]
fn frozen_file_read_reports_binary_and_size_gaps_and_caches_only_actual_text() {
    let lab = Lab::new();
    let rt = lab.runtime();
    let target = Target::File {
        owner: "fixture-owner".into(),
        repo: "fixture-repo".into(),
        path: "src/readme.txt".into(),
        revision: SHA_HEAD.into(),
        directory: false,
    };
    let reference = rt.save_github_target(&target).unwrap();
    let text = "猫".repeat(PREVIEW_CHARS + 10);
    let data = json!({"type":"file","encoding":"base64","content":STANDARD.encode(text.as_bytes()),"size":text.len(),"sha":"blob-sha"});
    let result = rt
        .github_file_content(&target, &reference, &data, false)
        .unwrap();
    assert_eq!(result["status"], "ok");
    assert_eq!(result["content_truncated"], true);
    assert_eq!(result["revision"], SHA_HEAD);
    let resource = result["content_resource"].as_str().unwrap();
    let bytes = rt
        .store
        .read_range(&rt.scope(Platform::Github), resource, 0, MAX_BODY_BYTES)
        .unwrap();
    assert_eq!(bytes, text.as_bytes());
    let binary =
        json!({"type":"file","encoding":"base64","content":STANDARD.encode([0xff,0x00]),"size":2});
    let result = rt
        .github_file_content(&target, &reference, &binary, false)
        .unwrap();
    assert_eq!(result["status"], "unsupported");
    assert_eq!(result["content_observed"], false);
    assert!(result.get("content_resource").is_none());
    let large = json!({"type":"file","encoding":"base64","content":"","size":MAX_BODY_BYTES+1});
    let result = rt
        .github_file_content(&target, &reference, &large, false)
        .unwrap();
    assert_eq!(result["status"], "unsupported");
    assert_eq!(result["content_observed"], false);
}

#[test]
fn changed_files_use_the_exact_head_fork_and_never_substitute_base_tip_for_removed_files() {
    let lab = Lab::new();
    let rt = lab.runtime();
    let snapshot = ChangeSnapshot {
        head_sha: SHA_HEAD.into(),
        base_sha: SHA_BASE.into(),
        head_repository: Some(Repository {
            owner: "fixture-fork".into(),
            repo: "fixture-repo".into(),
        }),
        base_repository: Some(Repository {
            owner: "fixture-owner".into(),
            repo: "fixture-repo".into(),
        }),
    };
    let cursor = Cursor {
        kind: ListKind::Changes,
        target: Some(Target::Issue {
            owner: "fixture-owner".into(),
            repo: "fixture-repo".into(),
            number: 7,
            pull: true,
        }),
        page: 1,
        seen: VecDeque::new(),
        pending: None,
        changes: Some(snapshot),
    };
    for status in ["modified", "removed"] {
        let item=rt.github_change_item(&cursor,&json!({"filename":"src/binary.dat","status":status,"patch_available":false,"sha":"blob"})).unwrap();
        assert_eq!(item["patch_available"], false);
        assert!(item["patch_gap"].is_string());
        assert!(item.get("patch").is_none());
        if status == "removed" {
            assert!(item.get("file_ref").is_none());
            assert_eq!(item["old_file_snapshot_supported"], false);
            assert!(item["file_gap"].as_str().unwrap().contains("merge-base"));
        } else {
            let (_, target) = rt
                .load_github_target(item["file_ref"].as_str().unwrap(), &json!({}))
                .unwrap();
            assert!(
                matches!(target,Target::File{owner,revision,..} if owner=="fixture-fork" && revision==SHA_HEAD)
            );
        }
    }
}

#[test]
fn expired_or_foreign_references_never_fall_back_to_another_account() {
    let lab = Lab::new();
    let mut rt = lab.runtime();
    let reference = "e".repeat(32);
    save_private_new(&lab.0.join("locators").join(format!("{reference}.json")),&json!({"scope":Lab::scope(),"kind":TARGET,"value":issue(7),"created_at":crate::outbox::now().unwrap()-30*24*60*60})).unwrap();
    let error = rt
        .call_github_task("github.open", &json!({"reference":reference}))
        .unwrap_err();
    assert!(error.contains("expired"));
    let reference = rt
        .locators
        .put(
            &Scope {
                platform: Platform::Tieba,
                account: "fixture-account".into(),
            },
            TARGET,
            &issue(7),
        )
        .unwrap();
    assert!(
        rt.call_github_task("github.open", &json!({"reference":reference}))
            .unwrap_err()
            .contains("another platform")
    );
    assert_eq!(rt.account, "public");
}

#[test]
fn notifications_expose_only_exact_supported_subject_targets_and_status_never_claims_online() {
    let data = json!({"repository":{"full_name":"Fixture-Owner/Fixture-Repo"},"subject":{"url":"https://api.github.com/repos/fixture-owner/fixture-repo/pulls/7"}});
    assert!(matches!(
        notification_target(&data).unwrap(),
        Some(Target::Issue {
            number: 7,
            pull: true,
            ..
        })
    ));
    for url in [
        "https://evil.invalid/repos/Fixture-Owner/Fixture-Repo/issues/7",
        "https://api.github.com/repos/Fixture-Owner/Other/issues/7",
        "https://api.github.com/repos/Fixture-Owner/Fixture-Repo/issues/7?token=x",
    ] {
        let mut wrong = data.clone();
        wrong["subject"]["url"] = json!(url);
        assert!(notification_target(&wrong).unwrap().is_none());
    }
    let lab = Lab::new();
    let mut rt = lab.runtime();
    let status = rt.call_github_task("github.status", &json!({})).unwrap();
    assert_eq!(status["status"], "signed_out");
    assert_eq!(status["online_checked"], false);
    let directory = account_directory(&lab.0, "github", &Lab::scope().account).unwrap();
    save_private_new(
        &directory.join("session.json"),
        &json!({"schema":1,"id":42,"login":"fixture-user","token":"not-a-real-token"}),
    )
    .unwrap();
    let status = rt
        .call_github_task("github.status", &json!({"account":"fixture-account"}))
        .unwrap();
    assert_eq!(status["status"], "saved_token");
    assert_eq!(status["online_checked"], false);
    assert!(!status.to_string().contains("not-a-real-token"));
}

#[test]
fn thousand_entry_directory_creates_only_visible_refs_under_locator_pressure() {
    let lab = Lab::new();
    let mut rt = lab.runtime();
    let scope = Lab::scope();
    // Synthetic low-value caches put the locator store just below GC pressure.
    // The response must still contain live refs even if GC runs while assembling it.
    let current = crate::outbox::now().unwrap();
    for n in 1..=1780_u64 {
        save_private_new(&lab.0.join("locators").join(format!("{n:032x}.json")), &json!({"scope":scope,"kind":"task-list","value":{"fixture":n},"created_at":current-100})).unwrap();
    }
    let raw:Vec<Value> = (0..1000).map(|n| json!({"name":format!("file-{n}.rs"),"path":format!("src/file-{n}.rs"),"type":"file","sha":format!("{n:040x}"),"size":1})).collect();
    let bytes = serde_json::to_vec(&raw).unwrap();
    let resource = rt
        .store
        .import(
            &scope,
            "raw-directory-fixture",
            "application/json",
            &mut bytes.as_slice(),
        )
        .unwrap();
    let cursor = Cursor {
        kind: ListKind::Files,
        target: Some(Target::File {
            owner: "fixture-owner".into(),
            repo: "fixture-repo".into(),
            path: "src".into(),
            revision: SHA_HEAD.into(),
            directory: true,
        }),
        page: 1,
        seen: VecDeque::new(),
        pending: Some(Pending {
            resource: resource.id.clone(),
            offset: 0,
            next_page: None,
            context: json!({"possibly_truncated":true,"directory_limit":1000}),
        }),
        changes: None,
    };
    let reference = rt.locators.put(&scope, CONTINUATION, &cursor).unwrap();
    let first = rt
        .call_github_task("github.list", &json!({"continuation":reference,"limit":20}))
        .unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 20);
    assert_eq!(first["collection_complete"], Value::Null);
    for (n, item) in first["items"].as_array().unwrap().iter().enumerate() {
        let (owner, target) = rt
            .load_github_target(item["target_ref"].as_str().unwrap(), &json!({}))
            .unwrap();
        assert_eq!(owner, scope);
        assert!(
            matches!(target,Target::File{path,revision,..} if path==format!("src/file-{n}.rs") && revision==SHA_HEAD)
        );
    }
    fn target_count(state: &Path) -> usize {
        fs::read_dir(state.join("locators"))
            .unwrap()
            .filter(|entry| {
                let path = entry.as_ref().unwrap().path();
                let data: Value =
                    serde_json::from_slice(&read_bounded(&path, 1024 * 1024).unwrap()).unwrap();
                data["kind"] == TARGET
            })
            .count()
    }
    assert_eq!(
        target_count(&lab.0),
        21,
        "only twenty returned files and their directory target may have refs"
    );
    let continuation = first["continuation"].as_str().unwrap();
    let saved: Cursor = rt.locators.get(&scope, CONTINUATION, continuation).unwrap();
    let pending = saved.pending.unwrap();
    assert_eq!(pending.resource, resource.id);
    assert_eq!(pending.offset, 20);
    assert_eq!(saved.seen.len(), 20);
    let (_, path) = rt.store.resolve(&scope, &pending.resource).unwrap();
    let stored: Vec<Value> =
        serde_json::from_slice(&read_bounded(&path, 2 * 1024 * 1024).unwrap()).unwrap();
    assert_eq!(stored.len(), 1000);
    assert!(
        stored.iter().all(|entry| entry.get("target_ref").is_none()),
        "pending stores raw provider projections, not eagerly allocated opaque references"
    );
    let second = rt
        .call_github_task(
            "github.list",
            &json!({"continuation":continuation,"limit":1}),
        )
        .unwrap();
    assert_eq!(second["items"][0]["path"], "src/file-20.rs");
    assert_eq!(target_count(&lab.0), 22);
    for item in first["items"].as_array().unwrap() {
        rt.load_github_target(item["target_ref"].as_str().unwrap(), &json!({}))
            .unwrap();
    }
}

#[test]
fn deduplication_window_rolls_forward_instead_of_blocking_item_2049() {
    let lab = Lab::new();
    let mut rt = lab.runtime();
    let scope = Lab::scope();
    let raw = json!([{"id":3000,"number":3000,"title":"beyond the previous dedupe limit","is_pull_request":false},{"id":3001,"number":3001,"title":"next item","is_pull_request":false}]);
    let encoded = serde_json::to_vec(&raw).unwrap();
    let resource = rt
        .store
        .import(
            &scope,
            "rolling-window-fixture",
            "application/json",
            &mut encoded.as_slice(),
        )
        .unwrap();
    let cursor = Cursor {
        kind: ListKind::Issues,
        target: Some(repository()),
        page: 104,
        seen: (0..MAX_SEEN).map(|i| format!("item:{i}")).collect(),
        pending: Some(Pending {
            resource: resource.id,
            offset: 0,
            next_page: None,
            context: json!({"server_has_more":false}),
        }),
        changes: None,
    };
    let reference = rt.locators.put(&scope, CONTINUATION, &cursor).unwrap();
    let data = rt
        .call_github_task("github.list", &json!({"continuation":reference,"limit":1}))
        .unwrap();
    assert_eq!(data["items"][0]["number"], 3000);
    let saved: Cursor = rt
        .locators
        .get(&scope, CONTINUATION, data["continuation"].as_str().unwrap())
        .unwrap();
    assert_eq!(saved.seen.len(), MAX_SEEN);
    assert_eq!(saved.seen.front().unwrap(), "item:1");
    assert_eq!(saved.seen.back().unwrap(), "item:3000");
    assert_eq!(
        data["context"]["deduplication"]["mode"],
        "rolling_recent_keys"
    );
}

#[test]
fn typed_rate_limit_becomes_structured_and_blocks_new_sdk_reads_across_restart() {
    let lab = Lab::new();
    let mut rt = lab.runtime();
    let now = crate::outbox::now().unwrap();
    let scope = rt.scope(Platform::Github);
    let first = seed_rate_limit(&mut rt, &scope, now + 1800);
    assert_eq!(first["status"], "rate_limited");
    assert_eq!(first["reset_at"], now + 1800);
    assert_eq!(first["retry_at"], now + 1800);
    assert_eq!(first["retry_after_seconds"], Value::Null);
    assert_eq!(first["network_retry_started"], false);
    assert_eq!(first["blocked_locally"], false);
    assert_eq!(first["backoff_saved"], true);
    let path = rt.github_backoff_path();
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
    assert!(fs::metadata(&path).unwrap().len() < 4096);
    let saved: Value = serde_json::from_slice(&read_bounded(&path, 4096).unwrap()).unwrap();
    assert_eq!(saved["scope"], json!(scope));
    assert!(saved.get("token").is_none());
    // The blocked read returns before constructing a GitHub client or sending HTTP.
    let blocked = rt
        .call_github_task(
            "github.open",
            &json!({"reference":"fixture-owner/fixture-repo","account":"public"}),
        )
        .unwrap();
    assert_eq!(blocked["status"], "rate_limited");
    assert_eq!(blocked["blocked_locally"], true);
    assert_eq!(blocked["retry_at"], now + 1800);
    assert!(!blocked.to_string().contains(TASK_ERROR));
    assert!(!blocked.to_string().contains("RateLimited"));
    drop(rt);
    let mut rt = lab.runtime();
    let blocked = rt
        .call_github_task(
            "github.open",
            &json!({"reference":"fixture-owner/fixture-repo","account":"public"}),
        )
        .unwrap();
    assert_eq!(blocked["status"], "rate_limited");
    assert_eq!(blocked["network_retry_started"], false);
    // Public/IP backoff is not silently moved onto another selected account.
    let reference = rt
        .locators
        .put(&Lab::scope(), TARGET, &repository())
        .unwrap();
    let error = rt
        .call_github_task("github.open", &json!({"reference":reference}))
        .unwrap_err();
    assert!(error.contains("no saved token"));
    assert_eq!(rt.account, "public");
}

#[test]
fn rate_limit_honors_later_retry_header_and_uses_an_explicit_fallback_when_headers_are_absent() {
    let lab = Lab::new();
    let rt = lab.runtime();
    let now = crate::outbox::now().unwrap();
    let error = rt.github_sdk_error(platform_github::Error::RateLimited {
        retry_after_seconds: Some(120),
        reset_at: Some(now + 30),
    });
    let response = decode_task_error(&error).unwrap();
    assert_eq!(response["retry_after_seconds"], 120);
    assert_eq!(response["reset_at"], now + 30);
    assert!(response["retry_at"].as_u64().unwrap() >= now + 120);
    assert_eq!(response["retry_at_source"], "server_retry_after");
    let error = rt.github_sdk_error(platform_github::Error::RateLimited {
        retry_after_seconds: None,
        reset_at: None,
    });
    let response = decode_task_error(&error).unwrap();
    assert_eq!(response["reset_at"], Value::Null);
    assert_eq!(response["retry_after_seconds"], Value::Null);
    assert_eq!(response["retry_at_source"], "local_default_60s");
    assert!(response["retry_at"].as_u64().unwrap() >= now + DEFAULT_BACKOFF_SECONDS);
    let human = human_task_error(error);
    assert!(!human.contains(TASK_ERROR));
    assert!(human.contains("rate_limited"));
    for (error, status) in [
        (platform_github::Error::LoginRequired, "needs_auth"),
        (
            platform_github::Error::PermissionDenied,
            "permission_denied",
        ),
    ] {
        let result = decode_task_error(&rt.github_sdk_error(error)).unwrap();
        assert_eq!(result["status"], status);
        assert_eq!(result["network_retry_started"], false);
    }
}

#[test]
fn expired_backoff_allows_future_reads_and_foreign_scope_or_symlink_record_fails_closed() {
    use std::os::unix::fs::symlink;
    let lab = Lab::new();
    let rt = lab.runtime();
    let now = crate::outbox::now().unwrap();
    let scope = rt.scope(Platform::Github);
    let expired = Backoff::new(scope.clone(), now - 120, Some(1), None);
    rt.github_save_backoff(&expired).unwrap();
    assert!(rt.github_check_backoff().is_ok());
    let path = rt.github_backoff_path();
    fs::remove_file(&path).unwrap();
    let wrong = Backoff::new(Lab::scope(), now, None, Some(now + 3600));
    save_private_new(&path, &json!(wrong)).unwrap();
    assert!(
        rt.github_check_backoff()
            .unwrap_err()
            .contains("invalid account-local")
    );
    fs::remove_file(&path).unwrap();
    let fixture = path.with_extension("fixture");
    save_private_new(
        &fixture,
        &json!(Backoff::new(scope, now, None, Some(now + 3600))),
    )
    .unwrap();
    symlink(&fixture, &path).unwrap();
    assert!(rt.github_check_backoff().is_err());
}

#[test]
fn rate_limited_rejected_comment_replay_preserves_the_durable_action_and_sent_readback_never_becomes_unknown()
 {
    let lab = Lab::new();
    let rt = lab.runtime();
    let reference = record(&lab, &rt, Status::Rejected);
    let outbox = Outbox::open(&lab.0).unwrap();
    let mut operation = outbox.load("fixture-comment", &Lab::scope()).unwrap();
    let response = Backoff::new(
        Lab::scope(),
        crate::outbox::now().unwrap(),
        None,
        Some(crate::outbox::now().unwrap() + 60),
    )
    .response(crate::outbox::now().unwrap(), false);
    operation.verification = Some(json!({"verified":false,"provider_error":response}));
    outbox.finish(&operation).unwrap();
    let data = comment_outcome(&operation, &reference, true);
    assert_eq!(data["status"], "rate_limited");
    assert_eq!(data["operation_status"], "rejected");
    assert_eq!(data["same_operation_will_retry"], false);
    assert_eq!(data["request_sent"], true);
    operation.status = Status::Sent;
    operation.verification = Some(json!({"verified":false,"read_error":response}));
    let data = comment_outcome(&operation, &reference, true);
    assert_eq!(data["status"], "sent");
    assert_eq!(data["unknown"], false);
    assert_eq!(data["readback_verified"], false);
    assert_eq!(data["verification"]["read_error"]["status"], "rate_limited");
}

#[test]
fn inline_review_evidence_requires_exact_actor_commit_coordinates_and_distinct_ids() {
    let line = platform_github::ReviewLine {
        path: "src/lib.rs".into(),
        body: "specific review".into(),
        line: 8,
        side: "RIGHT".into(),
        start_line: Some(7),
        start_side: Some("RIGHT".into()),
    };
    let actual = json!({"id":91,"user":{"id":42},"pull_request_review_id":90,"body":line.body,"path":line.path,"commit_id":SHA_HEAD,"line":8,"side":"RIGHT","start_line":7,"start_side":"RIGHT","pull_request_url":"https://api.github.com/repos/fixture-owner/fixture-repo/pulls/7"});
    assert!(inline_comments_match(
        &[line.clone()],
        &[actual.clone()],
        42,
        90,
        SHA_HEAD,
        "fixture-owner",
        "fixture-repo",
        7
    ));
    assert!(!inline_comments_match(
        &[line.clone(), line.clone()],
        &[actual.clone()],
        42,
        90,
        SHA_HEAD,
        "fixture-owner",
        "fixture-repo",
        7
    ));
    for (key, v) in [
        ("line", json!(9)),
        ("side", json!("LEFT")),
        ("pull_request_review_id", json!(89)),
        ("commit_id", json!(SHA_BASE)),
        (
            "pull_request_url",
            json!("https://api.github.com/repos/fixture-owner/other/pulls/7"),
        ),
    ] {
        let mut wrong = actual.clone();
        wrong[key] = v;
        assert!(!inline_comments_match(
            &[line.clone()],
            &[wrong],
            42,
            90,
            SHA_HEAD,
            "fixture-owner",
            "fixture-repo",
            7
        ));
    }
    assert!(!inline_comments_match(
        &[line],
        &[actual],
        43,
        90,
        SHA_HEAD,
        "fixture-owner",
        "fixture-repo",
        7
    ));
}
#[test]
fn collaboration_unknown_result_is_returned_without_credentials_or_network() {
    let lab = Lab::new();
    let mut rt = lab.runtime();
    let reference = rt
        .locators
        .put(&Lab::scope(), TARGET, &repository())
        .unwrap();
    let input = json!({"action":"create_issue","title":"synthetic","message":"fixture"});
    let mut r = Operation::new(
        "collaboration-once",
        Lab::scope(),
        "42".into(),
        serde_json::to_value(repository()).unwrap(),
        "create_issue",
    )
    .unwrap();
    r.request = Some(json!({"action":"github_collaboration","input":input}));
    let outbox = Outbox::open(&lab.0).unwrap();
    outbox.insert(&r).unwrap();
    outbox.begin_attempt(&mut r).unwrap();
    let result=rt.call_github_task("github.action",&json!({"action":"create_issue","title":"synthetic","message":"fixture","target_ref":reference,"operation_id":"collaboration-once"})).unwrap();
    assert_eq!(result["status"], "unknown");
    assert_eq!(result["reused_operation"], true);
    assert_eq!(
        outbox
            .load("collaboration-once", &Lab::scope())
            .unwrap()
            .status,
        Status::Unknown
    );
}

#[test]
fn oauth_metadata_rotates_atomically_preserves_alias_and_accepts_legacy_tokens() {
    let lab = Lab::new();
    let runtime = Runtime::open(&lab.0, "oauth-account", None).unwrap();
    let old: Session = serde_json::from_value(
        json!({"schema":1,"token":"synthetic-old-pat","id":42,"login":"fixture-user"}),
    )
    .unwrap();
    assert!(old.oauth.is_none());
    runtime.save_github_session(&old).unwrap();
    let tokens = platform_github::auth::Tokens {
        access_token: "synthetic-oauth-access".into(),
        refresh_token: Some("synthetic-oauth-refresh".into()),
        expires_in: Some(28800),
        refresh_expires_in: Some(15_552_000),
        scope: "read:user,public_repo,notifications".into(),
    };
    let metadata =
        OAuthSession::from_tokens(&tokens, platform_github::auth::DEFAULT_CLIENT_ID, 1000).unwrap();
    assert_eq!(metadata.expires_at, Some(29800));
    let replacement = Session {
        schema: 1,
        token: tokens.access_token,
        id: 42,
        login: "fixture-user".into(),
        oauth: Some(metadata),
    };
    runtime.save_github_session(&replacement).unwrap();
    let directory = account_directory(&lab.0, "github", "oauth-account").unwrap();
    let path = directory.join("session.json");
    let before = github_private(&path, 16384).unwrap();
    let stored: Session = serde_json::from_slice(&before).unwrap();
    assert_eq!(stored.token, "synthetic-oauth-access");
    assert_eq!(
        stored.oauth.unwrap().refresh_token.as_deref(),
        Some("synthetic-oauth-refresh")
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
        0
    );
    let wrong = Session {
        id: 99,
        ..replacement
    };
    assert!(runtime.save_github_session(&wrong).is_err());
    assert_eq!(github_private(&path, 16384).unwrap(), before);
    assert!(!std::fs::read_dir(directory).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[test]
fn malformed_oauth_state_is_not_accepted_as_a_saved_identity() {
    for extra in [
        json!({"client_id":"https://evil.invalid","expires_at":1,"refresh_token":"synthetic","refresh_expires_at":2,"scope":"read:user"}),
        json!({"client_id":platform_github::auth::DEFAULT_CLIENT_ID,"expires_at":1,"refresh_token":null,"refresh_expires_at":null,"scope":"read:user"}),
    ] {
        let session:Session=serde_json::from_value(json!({"schema":1,"token":"synthetic-access","id":42,"login":"fixture-user","oauth":extra})).unwrap();
        assert!(session.validate().is_err());
    }
}
