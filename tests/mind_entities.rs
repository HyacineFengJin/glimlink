// SPDX-License-Identifier: MIT
use agent_runtime::{agent_mind, app::Runtime, mind_store::Store};
use runtime_jobs::{AgentContext, Outcome, Queue};
use serde_json::json;
use std::{fs, path::PathBuf};
struct Lab(PathBuf);
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn typed_recap_is_portable_across_runtimes_and_engines_without_promoting_it_to_instructions() {
    let lab = Lab(std::env::temp_dir().join(format!("portable-recap-{}", std::process::id())));
    let state = lab.0.join("state-one");
    let other = lab.0.join("state-two");
    let shared = lab.0.join("mind");
    let mut runtime = Runtime::open(&state, "public", None).unwrap();
    runtime.set_mind_state(&shared).unwrap();
    let context = AgentContext {
        agent: "writer".into(),
        conversation: Some("room".into()),
        ..Default::default()
    };
    runtime.set_agent_context(context.clone()).unwrap();
    let queue = Queue::open(&state).unwrap();
    queue
        .enqueue_with_context(
            "earlier",
            "synthetic completed turn",
            None,
            context.clone(),
            10,
        )
        .unwrap();
    let claim = queue.claim_next(11).unwrap().unwrap();
    queue
        .finish(&claim.lease, Outcome::completed("ordinary dialogue"), 12)
        .unwrap();
    let saved=runtime.call("runtime.memory",&json!({"action":"recap_save","expected_revision":0,"recap":{"summary":"RECAP_DATA_MARKER ordinary remembered data","covered_jobs":["earlier"]}})).unwrap();
    assert_eq!(saved["durable"], true);
    assert!(runtime.call("runtime.memory",&json!({"action":"recap_save","expected_revision":saved["result"]["revision"],"recap":{"summary":"invented provenance","covered_jobs":["unrelated"]}})).is_err());
    let mut reader = Runtime::open(&other, "public", None).unwrap();
    reader.set_mind_state(&shared).unwrap();
    let mut second = context.clone();
    second.agent = "reader".into();
    reader.set_agent_context(second.clone()).unwrap();
    let recalled = reader
        .call("runtime.memory", &json!({"action":"recap_get"}))
        .unwrap();
    assert_eq!(
        recalled["result"]["recap"]["data"]["summary"],
        "RECAP_DATA_MARKER ordinary remembered data"
    );
    let prepared =
        agent_mind::assemble_with_store(&other, &shared, &second, "continue", None, None).unwrap();
    assert!(
        !prepared
            .developer_instructions
            .contains("RECAP_DATA_MARKER")
    );
    assert!(prepared.inputs.iter().any(
        |v| matches!(v,runtime_engine::Input::Text{text} if text.contains("RECAP_DATA_MARKER"))
    ));
    let unrelated = AgentContext {
        conversation: Some("other-room".into()),
        ..second
    };
    assert!(
        agent_runtime::mind_entities::recap(&Store::open(&shared).unwrap(), &unrelated)
            .unwrap()
            .is_none()
    );
}

#[test]
fn returned_entity_keys_are_directly_usable_for_read_update_relationship_and_episode() {
    let lab = Lab(std::env::temp_dir().join(format!("entity-roundtrip-{}", std::process::id())));
    let mut rt = Runtime::open(&lab.0, "public", None).unwrap();
    let saved=rt.call("runtime.memory",&json!({"action":"person_save","key":"owner","expected_revision":0,"person":{"name":"Owner"}})).unwrap();
    let key = saved["result"]["key"].clone();
    let read = rt
        .call("runtime.memory", &json!({"action":"person_get","key":key}))
        .unwrap();
    assert_eq!(read["result"]["key"], key);
    let updated=rt.call("runtime.memory",&json!({"action":"person_save","key":key,"expected_revision":saved["result"]["revision"],"person":{"name":"Owner","notes":"updated"}})).unwrap();
    assert_eq!(updated["result"]["key"], key);
    rt.call("runtime.memory",&json!({"action":"person_save","key":"friend","expected_revision":0,"person":{"name":"Friend","relationships":[{"person_key":key,"relation":"knows"}]}})).unwrap();
    let episode=rt.call("runtime.memory",&json!({"action":"event_save","key":"meeting","expected_revision":0,"event":{"summary":"A meeting","occurred_at":"2026-10-31","people":[key]}})).unwrap();
    let event_key = episode["result"]["key"].clone();
    let recalled = rt
        .call(
            "runtime.memory",
            &json!({"action":"event_get","key":event_key}),
        )
        .unwrap();
    assert_eq!(recalled["result"]["key"], event_key);
}
