// SPDX-License-Identifier: MIT
//! Portable viewing progress. Requested/prepared sampling is never full observation.
use crate::feature_state::Store;
use runtime_core::{Result, Scope};
use runtime_jobs::AgentContext;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct MediaPreferences {
    pub transcribe: bool,
    pub include_subtitles: bool,
    pub speech_language: Option<String>,
    pub subtitle_language: Option<String>,
}
impl Default for MediaPreferences {
    fn default() -> Self {
        Self {
            transcribe: true,
            include_subtitles: true,
            speech_language: None,
            subtitle_language: None,
        }
    }
}
impl MediaPreferences {
    fn validate(&self) -> Result<()> {
        if self
            .speech_language
            .iter()
            .chain(self.subtitle_language.iter())
            .any(|v| v.is_empty() || v.len() > 64 || v.chars().any(char::is_control))
        {
            return Err("invalid viewing language preference".into());
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub id: String,
    pub persona: String,
    pub conversation: Option<String>,
    pub scope: Scope,
    pub video: String,
    pub page: u32,
    pub next_ms: u64,
    pub duration_ms: Option<u64>,
    pub window_ms: u64,
    pub interval_ms: u64,
    pub max_edge: u32,
    #[serde(default)]
    pub preferences: MediaPreferences,
    pub revision: u64,
    pub prepared_windows: Vec<[u64; 2]>,
    pub sampled_frames: u64,
    pub finished: bool,
    pub updated_at: u64,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    revision: u64,
    sessions: BTreeMap<String, Session>,
}
pub struct Sessions(Store);
impl Sessions {
    pub fn open(state: &Path) -> Result<Self> {
        Ok(Self(Store::open(state, "watch-sessions", 512 * 1024)?))
    }
    pub fn start(
        &self,
        ctx: &AgentContext,
        scope: Scope,
        video: String,
        page: u32,
        start: u64,
        window: u64,
        interval: u64,
        edge: u32,
        now: u64,
    ) -> Result<Session> {
        self.start_with_preferences(
            ctx,
            scope,
            video,
            page,
            start,
            window,
            interval,
            edge,
            MediaPreferences::default(),
            now,
        )
    }
    pub fn start_with_preferences(
        &self,
        ctx: &AgentContext,
        scope: Scope,
        video: String,
        page: u32,
        start: u64,
        window: u64,
        interval: u64,
        edge: u32,
        preferences: MediaPreferences,
        now: u64,
    ) -> Result<Session> {
        preferences.validate()?;
        ctx.validate()?;
        scope.validate()?;
        if video.is_empty()
            || video.len() > 64
            || page == 0
            || !(1..=60000).contains(&window)
            || interval == 0
            || edge == 0
            || edge > 4096
        {
            return Err("invalid viewing settings".into());
        }
        let mut bytes = [0u8; 16];
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .map_err(|e| e.to_string())?;
        let id = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        self.0.update::<State, _>(|state| {
            if state.sessions.len() >= 128 {
                return Err("viewing-session quota reached; finish/drop old sessions".into());
            }
            state.revision = state
                .revision
                .checked_add(1)
                .ok_or("viewing revision exhausted")?;
            let session = Session {
                id: id.clone(),
                persona: ctx.persona.clone(),
                conversation: ctx.conversation.clone(),
                scope,
                video,
                page,
                next_ms: start,
                duration_ms: None,
                window_ms: window,
                interval_ms: interval,
                max_edge: edge,
                preferences,
                revision: state.revision,
                prepared_windows: Vec::new(),
                sampled_frames: 0,
                finished: false,
                updated_at: now,
            };
            state.sessions.insert(id, session.clone());
            Ok(session)
        })
    }
    pub fn get(&self, ctx: &AgentContext, id: &str) -> Result<Session> {
        let state: State = self.0.load()?;
        let session = state.sessions.get(id).ok_or("viewing session not found")?;
        if session.persona != ctx.persona {
            return Err("viewing session belongs to another persona".into());
        }
        Ok(session.clone())
    }
    pub fn list(&self, ctx: &AgentContext) -> Result<Vec<Session>> {
        let state: State = self.0.load()?;
        Ok(state
            .sessions
            .into_values()
            .filter(|s| s.persona == ctx.persona && s.conversation == ctx.conversation)
            .collect())
    }
    pub fn commit(
        &self,
        ctx: &AgentContext,
        id: &str,
        expected: u64,
        start: u64,
        end: u64,
        frames: u64,
        duration: Option<u64>,
        finished: bool,
        now: u64,
    ) -> Result<Session> {
        if end < start {
            return Err("invalid viewing interval".into());
        }
        self.edit(ctx, id, expected, now, |session| {
            if frames > 0 {
                session.prepared_windows.push([start, end]);
                session.prepared_windows.sort_unstable();
                let mut joined: Vec<[u64; 2]> = Vec::new();
                for range in &session.prepared_windows {
                    if let Some(last) = joined.last_mut()
                        && range[0] <= last[1]
                    {
                        last[1] = last[1].max(range[1]);
                    } else {
                        joined.push(*range)
                    }
                }
                if joined.len() > 128 {
                    return Err("too many disjoint viewing windows".into());
                }
                session.prepared_windows = joined;
                session.sampled_frames = session.sampled_frames.saturating_add(frames);
                session.next_ms = end;
            }
            if duration.is_some() {
                session.duration_ms = duration;
            }
            session.finished =
                finished || session.duration_ms.is_some_and(|d| session.next_ms >= d);
            Ok(())
        })
    }
    pub fn configure(
        &self,
        ctx: &AgentContext,
        id: &str,
        expected: u64,
        preferences: MediaPreferences,
        now: u64,
    ) -> Result<Session> {
        preferences.validate()?;
        self.edit(ctx, id, expected, now, |session| {
            session.preferences = preferences;
            Ok(())
        })
    }
    pub fn seek(
        &self,
        ctx: &AgentContext,
        id: &str,
        expected: u64,
        start: u64,
        now: u64,
    ) -> Result<Session> {
        self.edit(ctx, id, expected, now, |session| {
            session.next_ms = start;
            session.finished = false;
            Ok(())
        })
    }
    pub fn finish(&self, ctx: &AgentContext, id: &str, expected: u64, now: u64) -> Result<Session> {
        self.edit(ctx, id, expected, now, |session| {
            session.finished = true;
            Ok(())
        })
    }
    pub fn drop_session(&self, ctx: &AgentContext, id: &str, expected: u64) -> Result<()> {
        self.0.update::<State, _>(|state| {
            let s = state.sessions.get(id).ok_or("viewing session not found")?;
            if s.persona != ctx.persona || s.revision != expected {
                return Err("viewing identity/revision mismatch".into());
            }
            state.sessions.remove(id);
            Ok(())
        })
    }
    fn edit(
        &self,
        ctx: &AgentContext,
        id: &str,
        expected: u64,
        now: u64,
        apply: impl FnOnce(&mut Session) -> Result<()>,
    ) -> Result<Session> {
        self.0.update::<State, _>(|state| {
            let revision = state
                .revision
                .checked_add(1)
                .ok_or("viewing revision exhausted")?;
            let s = state
                .sessions
                .get_mut(id)
                .ok_or("viewing session not found")?;
            if s.persona != ctx.persona || s.revision != expected {
                return Err("viewing identity/revision mismatch; reread progress".into());
            }
            apply(s)?;
            s.revision = revision;
            s.updated_at = now;
            let result = s.clone();
            state.revision = revision;
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_survives_restart_merges_only_prepared_windows_and_checks_scope_revision() {
        let root = std::env::temp_dir().join(format!(
            "watch-session-test-{}-{}",
            std::process::id(),
            crate::agent_host::now().unwrap()
        ));
        let ctx = AgentContext {
            conversation: Some("room".into()),
            ..Default::default()
        };
        let store = Sessions::open(&root).unwrap();
        let s = store
            .start(
                &ctx,
                Scope {
                    platform: runtime_core::Platform::Bilibili,
                    account: "fixture".into(),
                },
                "BV17x411w7KC".into(),
                1,
                0,
                10000,
                2500,
                480,
                1,
            )
            .unwrap();
        let mut legacy = serde_json::to_value(&s).unwrap();
        legacy.as_object_mut().unwrap().remove("preferences");
        assert_eq!(
            serde_json::from_value::<Session>(legacy)
                .unwrap()
                .preferences,
            MediaPreferences::default()
        );
        let s = store
            .commit(&ctx, &s.id, s.revision, 0, 10000, 4, Some(30000), false, 2)
            .unwrap();
        assert!(
            store
                .commit(&ctx, &s.id, 1, 10000, 20000, 4, None, false, 3)
                .is_err()
        );
        let reopened = Sessions::open(&root).unwrap();
        assert_eq!(reopened.get(&ctx, &s.id).unwrap(), s);
        let s = reopened
            .commit(&ctx, &s.id, s.revision, 10000, 20000, 4, None, false, 3)
            .unwrap();
        assert_eq!(s.prepared_windows, vec![[0, 20000]]);
        let s = reopened
            .commit(&ctx, &s.id, s.revision, 20000, 30000, 0, None, false, 4)
            .unwrap();
        assert_eq!(s.next_ms, 20000);
        assert!(!s.finished);
        let other = AgentContext {
            persona: "other".into(),
            ..Default::default()
        };
        assert!(reopened.get(&other, &s.id).is_err());
        let s = reopened.seek(&ctx, &s.id, s.revision, 25000, 5).unwrap();
        assert_eq!(s.next_ms, 25000);
        reopened.drop_session(&ctx, &s.id, s.revision).unwrap();
        assert!(reopened.list(&ctx).unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
