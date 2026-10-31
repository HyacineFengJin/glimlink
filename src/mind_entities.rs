// SPDX-License-Identifier: MIT
//! Typed, attributed people/event memory; identities are explicit claims, never credentials.
use crate::mind_store::{Memory, Origin, Remember, Store};
use runtime_core::Result;
use runtime_jobs::{AgentContext, Queue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub platform: String,
    pub subject: String,
    /// How the association was established, not an inferred same-name match.
    pub evidence: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relationship {
    pub person_key: String,
    pub relation: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    pub name: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub identities: Vec<Identity>,
    #[serde(default)]
    pub relationships: Vec<Relationship>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Episode {
    pub summary: String,
    pub occurred_at: String,
    #[serde(default)]
    pub people: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recap {
    pub summary: String,
    pub covered_jobs: Vec<String>,
}
fn text(s: &str, max: usize, empty: bool) -> Result<()> {
    if (!empty && s.trim().is_empty()) || s.len() > max || s.chars().any(|c| c == '\0') {
        return Err("invalid structured memory text".into());
    }
    Ok(())
}
fn key(params: &Value, prefix: &str) -> Result<String> {
    let id = params
        .get("key")
        .and_then(Value::as_str)
        .ok_or("key is required")?;
    // Typed operations accept their returned key unchanged.
    let namespace = format!("{prefix}:");
    let id = id.strip_prefix(&namespace).unwrap_or(id);
    if id.is_empty()
        || id.len() > 48
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
    {
        return Err("invalid structured memory key".into());
    }
    Ok(format!("{prefix}:{id}"))
}
fn view(memory: Memory) -> Result<Value> {
    let data: Value =
        serde_json::from_str(&memory.text).map_err(|_| "stored memory is not structured data")?;
    Ok(
        json!({"key":memory.key,"data":data,"revision":memory.revision,"source":memory.source,"visibility":memory.visibility,"conversation":memory.conversation,"created_at":memory.created_at,"updated_at":memory.updated_at}),
    )
}
fn visibility(params: &Value) -> Result<&str> {
    match params
        .get("visibility")
        .and_then(Value::as_str)
        .unwrap_or("shared")
    {
        v @ ("shared" | "conversation") => Ok(v),
        _ => Err("invalid visibility".into()),
    }
}
fn save(
    store: &Store,
    ctx: &AgentContext,
    job: Option<&str>,
    params: &Value,
    key: String,
    data: Value,
    kind: &str,
    vis: &str,
) -> Result<Value> {
    let expected = params
        .get("expected_revision")
        .and_then(Value::as_u64)
        .ok_or("expected_revision required (0=create)")?;
    let memory = store.remember(
        ctx,
        Remember {
            key,
            text: serde_json::to_string(&data).map_err(|e| e.to_string())?,
            tags: vec![format!("type:{kind}")],
            visibility: vis.into(),
            expected_revision: expected,
            source: Origin::agent(&ctx.agent, job),
        },
    )?;
    Ok(
        json!({"status":"ok","result":view(memory)?,"durable":true,"identity_links_are_permissions":false,"network_requests":0}),
    )
}
pub fn recap(store: &Store, ctx: &AgentContext) -> Result<Option<Value>> {
    if ctx.conversation.is_none() {
        return Ok(None);
    }
    match store.get(ctx, "dialogue-recap", "conversation") {
        Ok(m) => Ok(Some(view(m)?)),
        Err(e) if e == "memory not found" => Ok(None),
        Err(e) => Err(e),
    }
}
pub fn call(
    state: &Path,
    mind: &Path,
    ctx: &AgentContext,
    job: Option<&str>,
    params: &Value,
) -> Result<Value> {
    ctx.validate()?;
    let store = Store::open(mind)?;
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .ok_or("memory action required")?;
    let vis = visibility(params)?;
    let result = match action {
        "person_save" => {
            let person: Person = serde_json::from_value(
                params
                    .get("person")
                    .cloned()
                    .ok_or("person object required")?,
            )
            .map_err(|_| "invalid person object")?;
            text(&person.name, 128, false)?;
            text(&person.notes, 768, true)?;
            if person.identities.len() > 8 || person.relationships.len() > 8 {
                return Err("person identities/relationships quota exceeded".into());
            }
            let mut seen = std::collections::BTreeSet::new();
            for id in &person.identities {
                if !["qq", "weixin", "bilibili", "tieba", "github"].contains(&id.platform.as_str())
                {
                    return Err("invalid identity platform".into());
                };
                text(&id.subject, 128, false)?;
                text(&id.evidence, 256, false)?;
                if !seen.insert((&id.platform, &id.subject)) {
                    return Err("duplicate identity claim".into());
                }
            }
            for rel in &person.relationships {
                text(&rel.relation, 128, false)?;
                let linked = key(&json!({"key":rel.person_key}), "person")?;
                store.get(ctx, &linked, vis)?;
            }
            return save(
                &store,
                ctx,
                job,
                params,
                key(params, "person")?,
                serde_json::to_value(person).map_err(|e| e.to_string())?,
                "person",
                vis,
            );
        }
        "person_get" => view(store.get(ctx, &key(params, "person")?, vis)?)?,
        "event_save" => {
            let event: Episode = serde_json::from_value(
                params
                    .get("event")
                    .cloned()
                    .ok_or("event object required")?,
            )
            .map_err(|_| "invalid event object")?;
            text(&event.summary, 1024, false)?;
            text(&event.occurred_at, 64, false)?;
            if event.people.len() > 8 || event.evidence_refs.len() > 8 {
                return Err("event references quota exceeded".into());
            }
            for person in &event.people {
                store.get(ctx, &key(&json!({"key":person}), "person")?, vis)?;
            }
            for evidence in &event.evidence_refs {
                text(evidence, 128, false)?;
            }
            return save(
                &store,
                ctx,
                job,
                params,
                key(params, "event")?,
                serde_json::to_value(event).map_err(|e| e.to_string())?,
                "event",
                vis,
            );
        }
        "event_get" => view(store.get(ctx, &key(params, "event")?, vis)?)?,
        "people" | "events" => {
            let kind = if action == "people" {
                "person"
            } else {
                "event"
            };
            let query = params.get("query").and_then(Value::as_str).unwrap_or("");
            text(query, 2048, true)?;
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(8);
            if !(1..=16).contains(&limit) {
                return Err("limit must be 1..16".into());
            }
            let found = store.search(ctx, &format!("type:{kind} {query}"), Some(vis), 16)?;
            let values = found
                .into_iter()
                .filter(|m| m.tags.contains(&format!("type:{kind}")))
                .take(limit as usize)
                .map(view)
                .collect::<Result<Vec<_>>>()?;
            json!({"items":values,"bounded_retrieval":true})
        }
        "recap_get" => json!({"recap":recap(&store,ctx)?}),
        "recap_save" => {
            if ctx.conversation.is_none() {
                return Err("recap requires an explicit conversation".into());
            }
            let recap: Recap = serde_json::from_value(
                params
                    .get("recap")
                    .cloned()
                    .ok_or("recap object required")?,
            )
            .map_err(|_| "invalid recap object")?;
            text(&recap.summary, 1536, false)?;
            if recap.covered_jobs.is_empty() || recap.covered_jobs.len() > 16 {
                return Err("recap must cite 1..16 completed jobs".into());
            }
            let queue = Queue::open(state)?;
            for id in &recap.covered_jobs {
                let covered = queue.get(id)?;
                if covered.context.persona != ctx.persona
                    || covered.context.conversation != ctx.conversation
                    || covered.finished_at.is_none()
                {
                    return Err("recap job must be finished in this persona/conversation".into());
                }
            }
            return save(
                &store,
                ctx,
                job,
                params,
                "dialogue-recap".into(),
                serde_json::to_value(recap).map_err(|e| e.to_string())?,
                "recap",
                "conversation",
            );
        }
        _ => return Err("unknown structured memory action".into()),
    };
    Ok(json!({"status":"ok","result":result,"network_requests":0}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_identity_and_episode_are_shared_between_roles_with_cas_and_provenance() {
        let path = std::env::temp_dir().join(format!(
            "typed-mind-{}-{}",
            std::process::id(),
            crate::agent_host::now().unwrap()
        ));
        let one = AgentContext::default();
        let mut two = one.clone();
        two.agent = "other".into();
        let p = json!({"action":"person_save","key":"owner","expected_revision":0,"person":{"name":"Owner","identities":[{"platform":"qq","subject":"10001","evidence":"operator supplied the account mapping"}]}});
        let saved = call(&path, &path, &one, Some("job-a"), &p).unwrap();
        assert_eq!(saved["result"]["source"]["job"], "job-a");
        let read = call(
            &path,
            &path,
            &two,
            None,
            &json!({"action":"person_get","key":"owner"}),
        )
        .unwrap();
        assert_eq!(read["result"]["data"]["identities"][0]["subject"], "10001");
        assert!(call(&path, &path, &two, None, &p).is_err());
        let e = json!({"action":"event_save","key":"meeting","expected_revision":0,"event":{"summary":"Discussed project requirements","occurred_at":"2026-10-30","people":["owner"]}});
        call(&path, &path, &two, Some("job-b"), &e).unwrap();
        assert_eq!(
            call(&path, &path, &one, None, &json!({"action":"events"})).unwrap()["result"]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let mut bad = p.clone();
        bad["key"] = json!("stranger");
        bad["person"]["identities"][0]["evidence"] = json!("");
        assert!(call(&path, &path, &one, None, &bad).is_err());
        assert!(call(&path,&path,&one,None,&json!({"action":"recap_save","expected_revision":0,"recap":{"summary":"no conversation","covered_jobs":["missing"]}})).is_err());
        std::fs::remove_dir_all(path).unwrap();
    }
}
