// SPDX-License-Identifier: MIT
//! A viewing session owns the next window, account and cross-window progress.
use super::tasks::{TaskTarget, check_account};
use super::*;
use crate::watch_sessions::{MediaPreferences, Session, Sessions};

fn language_parameter(params: &Value, key: &str) -> Result<Option<String>> {
    params
        .get(key)
        .map(|v| {
            let value = v
                .as_str()
                .ok_or_else(|| format!("{key} must be text"))?
                .trim();
            if value.is_empty() || value.len() > 64 || value.chars().any(char::is_control) {
                return Err(format!("invalid {key}"));
            }
            Ok(value.to_owned())
        })
        .transpose()
}
pub(super) fn media_preferences(
    params: &Value,
    inherited: &MediaPreferences,
) -> Result<MediaPreferences> {
    let mut result = inherited.clone();
    result.quality = super::media_view::quality(params, inherited.quality)?;
    for (key, target) in [
        ("transcribe", &mut result.transcribe),
        ("include_subtitles", &mut result.include_subtitles),
    ] {
        if let Some(value) = params.get(key) {
            *target = value
                .as_bool()
                .ok_or_else(|| format!("{key} must be boolean"))?;
        }
    }
    if let Some(language) = language_parameter(params, "language")? {
        result.speech_language = super::speech::normalize_speech_language(&language)?;
        result.subtitle_language = (!language.eq_ignore_ascii_case("auto")).then_some(language);
    }
    if let Some(language) = language_parameter(params, "speech_language")? {
        result.speech_language = super::speech::normalize_speech_language(&language)?;
    }
    if let Some(language) = language_parameter(params, "subtitle_language")? {
        result.subtitle_language = (!language.eq_ignore_ascii_case("auto")).then_some(language);
    }
    Ok(result)
}
fn subtitle_language_matches(actual: &str, id: &str, requested: &str) -> bool {
    if actual.eq_ignore_ascii_case(requested) || id == requested {
        return true;
    }
    let requested = requested.to_ascii_lowercase();
    let Some(base) = super::speech::normalize_speech_language(&requested)
        .ok()
        .flatten()
    else {
        return false;
    };
    // Locale tags and explicit AI/track selectors remain exact. Only a bare
    // language code asks Runtime to choose a locale/automatic subtitle track.
    requested == base
        && super::speech::normalize_speech_language(actual)
            .ok()
            .flatten()
            .is_some_and(|language| language == base)
}

fn projection(session: &Session) -> Value {
    json!({"watch_ref":session.id,"revision":session.revision,"scope":session.scope,
        "page":session.page,"next_start_ms":session.next_ms,"duration_ms":session.duration_ms,
        "finished":session.finished,"timeline_exhausted":session.duration_ms.is_some_and(|d|session.next_ms>=d),
        "prepared_windows":session.prepared_windows,"sampled_frames":session.sampled_frames,
        "preferences":session.preferences,"full_video_watched":false,"coverage_kind":"prepared_sampled_visual_windows",
        "max_edge":session.max_edge,
        "note":"Window coverage records prepared samples, not every frame or proof of model understanding."})
}
impl Runtime {
    pub(super) fn watch_video(&mut self, params: &Value) -> Result<Value> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("window");
        if action == "inspect" {
            return self.inspect_video(params);
        }
        if action == "window" {
            return self.watch_video_window(params);
        }
        let store = Sessions::open(&self.state)?;
        let context = self.mind_context.clone();
        if action == "list" {
            return Ok(
                json!({"status":"ok","sessions":store.list(&context)?.iter().map(projection).collect::<Vec<_>>()}),
            );
        }
        let mut session = if action == "start" {
            let target = required_str(params, "target_ref")?;
            let (scope, object) = self.load_task_target(target)?;
            check_account(params, &scope)?;
            let video = match object {
                TaskTarget::Video { video } | TaskTarget::BiliComment { video, .. } => video,
                _ => return Err("viewing sessions require a video target".into()),
            };
            let hint = self.target_playback_hint(target)?;
            let page = u32::try_from(number(
                params,
                "page",
                hint.as_ref().map(|h| u64::from(h.page)).unwrap_or(1),
            )?)
            .map_err(|_| "invalid video part")?;
            let start = number(
                params,
                "start_ms",
                hint.as_ref().map(|h| h.start_ms).unwrap_or(0),
            )?;
            let window = number(params, "duration_ms", 10000)?;
            let interval = number(params, "interval_ms", window.div_ceil(4).max(1))?;
            let preferences = media_preferences(params, &MediaPreferences::default())?;
            let edge = u32::try_from(number(
                params,
                "max_edge",
                u64::from(preferences.quality.default_edge()),
            )?)
            .map_err(|_| "invalid viewing size")?;
            store.start_with_preferences(
                &context,
                scope,
                video,
                page,
                start,
                window,
                interval,
                edge,
                preferences,
                crate::agent_host::now()?,
            )?
        } else {
            let id = required_str(params, "watch_ref")?;
            let session = store.get(&context, id)?;
            check_account(params, &session.scope)?;
            session
        };
        if let Some(expected) = params.get("expected_revision") {
            if expected.as_u64() != Some(session.revision) {
                return Err("viewing progress changed; reread status".into());
            }
        }
        if action != "start" && matches!(action, "next" | "seek") {
            let preferences = media_preferences(params, &session.preferences)?;
            if preferences != session.preferences {
                session = store.configure(
                    &context,
                    &session.id,
                    session.revision,
                    preferences,
                    crate::agent_host::now()?,
                )?;
            }
        }
        match action {
            "status" => Ok(json!({"status":"ok","session":projection(&session)})),
            "drop" => {
                store.drop_session(&context, &session.id, session.revision)?;
                Ok(json!({"status":"ok","dropped":true}))
            }
            "finish" => {
                let s = store.finish(
                    &context,
                    &session.id,
                    session.revision,
                    crate::agent_host::now()?,
                )?;
                Ok(json!({"status":"ok","session":projection(&s)}))
            }
            "seek" => {
                let s = store.seek(
                    &context,
                    &session.id,
                    session.revision,
                    number(params, "start_ms", session.next_ms)?,
                    crate::agent_host::now()?,
                )?;
                if params.get("read_now") == Some(&json!(false)) {
                    Ok(json!({"status":"ok","session":projection(&s)}))
                } else {
                    self.read_watch_session(&store, &s, params)
                }
            }
            "start" | "next" => {
                if params.get("read_now") == Some(&json!(false)) {
                    return Ok(json!({"status":"ok","session":projection(&session)}));
                }
                self.read_watch_session(&store, &session, params)
            }
            _ => Err("unknown viewing action".into()),
        }
    }

    fn read_watch_session(
        &mut self,
        store: &Sessions,
        session: &Session,
        params: &Value,
    ) -> Result<Value> {
        if session.finished {
            return Ok(
                json!({"status":"end_of_media","prepared":false,"input_available":false,"session":projection(session)}),
            );
        }
        let start = session.next_ms;
        let requested_duration = number(params, "duration_ms", session.window_ms)?;
        let duration = session
            .duration_ms
            .map(|d| requested_duration.min(d.saturating_sub(start)))
            .unwrap_or(requested_duration);
        if duration == 0 {
            return Ok(
                json!({"status":"end_of_media","prepared":false,"input_available":false,"session":projection(session)}),
            );
        }
        let mut request = json!({"video":session.video,"page":session.page,"start_ms":start,"duration_ms":duration,"interval_ms":number(params,"interval_ms",session.interval_ms)?,"max_edge":number(params,"max_edge",u64::from(session.max_edge))?,"transcribe":session.preferences.transcribe,"include_subtitles":session.preferences.include_subtitles,"speech_language":session.preferences.speech_language,"subtitle_language":session.preferences.subtitle_language});
        request
            .as_object_mut()
            .unwrap()
            .retain(|_, value| !value.is_null());
        request["quality"] = json!(session.preferences.quality);
        for key in ["region", "format"] {
            if let Some(value) = params.get(key) {
                request[key] = value.clone();
            }
        }
        let result = self.with_task_scope(&session.scope, |rt| rt.watch_video_window(&request));
        let mut result = match result {
            Ok(value) => value,
            Err(error) => {
                return Ok(
                    json!({"status":"failed","session":projection(session),"error":error,"progress_advanced":false}),
                );
            }
        };
        let frames = result
            .pointer("/window/frames")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0) as u64;
        let detected = result
            .pointer("/window/probe/duration_ms")
            .and_then(Value::as_u64)
            .or_else(|| {
                result
                    .pointer("/video/duration_seconds")
                    .and_then(Value::as_u64)
                    .map(|d| d.saturating_mul(1000))
            });
        let end = start
            .checked_add(duration)
            .ok_or("viewing time overflow")?
            .min(detected.unwrap_or(u64::MAX))
            .max(start);
        let ended = result.get("status").and_then(Value::as_str) == Some("end_of_media");
        let updated = store.commit(
            &self.mind_context,
            &session.id,
            session.revision,
            start,
            end,
            frames,
            detected,
            ended,
            crate::agent_host::now()?,
        )?;
        result["session"] = projection(&updated);
        result["progress_advanced"] = json!(frames > 0);
        result["continue_with"] =
            json!({"tool":"media_watch","arguments":{"action":"next","watch_ref":session.id}});
        Ok(result)
    }
    pub(super) fn attach_watch_subtitles(
        &mut self,
        result: &mut Value,
        params: &Value,
        scope: &Scope,
        video: &str,
        page: u32,
        start: u64,
        duration: u64,
    ) -> Result<()> {
        let preferences = media_preferences(params, &MediaPreferences::default())?;
        if !preferences.include_subtitles {
            result["subtitles"] = json!({"status":"disabled","lines":[]});
            return Ok(());
        }
        let end = start
            .checked_add(duration)
            .ok_or("subtitle window overflow")?
            .min(
                result
                    .pointer("/window/probe/duration_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX),
            )
            .max(start);
        result["subtitles"] = self
            .watch_subtitle_window(
                scope,
                video,
                page,
                start,
                end,
                preferences.subtitle_language.as_deref(),
            )
            .unwrap_or_else(|error| json!({"status":"unavailable","error":error}));
        Ok(())
    }
    fn watch_subtitle_window(
        &mut self,
        scope: &Scope,
        video: &str,
        page: u32,
        start: u64,
        end: u64,
        language: Option<&str>,
    ) -> Result<Value> {
        let cache =
            crate::feature_state::Store::open(&self.state, "subtitle-cache", 2 * 1024 * 1024)?;
        let mut state: SubtitleCache = cache.load()?;
        let now = crate::agent_host::now()?;
        let entry = state
            .entries
            .iter()
            .find(|e| {
                e.scope == *scope
                    && e.video == video
                    && e.page == page
                    && now.saturating_sub(e.cached_at) < 300
                    && language.is_none_or(|l| subtitle_language_matches(&e.language, &e.id, l))
            })
            .cloned();
        let entry = match entry {
            Some(entry) => entry,
            None => {
                let video: VideoId = video
                    .parse()
                    .map_err(|e: platform_bilibili::Error| e.to_string())?;
                let info = self.bili.video_info(&video).map_err(|e| e.to_string())?;
                let part = info
                    .pages
                    .iter()
                    .find(|p| p.page == page)
                    .ok_or("video part absent")?;
                let index = self
                    .bili
                    .subtitles(&info, part.cid)
                    .map_err(|e| e.to_string())?;
                let track = if let Some(language) = language {
                    index
                        .tracks
                        .iter()
                        .find(|t| t.language.eq_ignore_ascii_case(language) || t.id == language)
                        .or_else(|| {
                            index
                                .tracks
                                .iter()
                                .find(|t| subtitle_language_matches(&t.language, &t.id, language))
                        })
                } else {
                    index
                        .tracks
                        .iter()
                        .find(|t| t.language.starts_with("zh"))
                        .or_else(|| index.tracks.first())
                };
                let Some(track) = track else {
                    return Ok(
                        json!({"status":"no_subtitles","requires_login":index.requires_login,"lines":[]}),
                    );
                };
                let lines = self.bili.subtitle_lines(track).map_err(|e| e.to_string())?;
                let entry = SubtitleEntry {
                    scope: scope.clone(),
                    video: video.to_string(),
                    page: page,
                    id: track.id.clone(),
                    language: track.language.clone(),
                    lines,
                    cached_at: now,
                };
                state.entries.retain(|e| {
                    !(e.scope == entry.scope
                        && e.video == entry.video
                        && e.page == entry.page
                        && e.language == entry.language)
                });
                state.entries.push(entry.clone());
                while state.entries.len() > 16 {
                    state.entries.remove(0);
                }
                let _ = cache.update::<SubtitleCache, _>(|saved| {
                    *saved = state;
                    Ok(())
                });
                entry
            }
        };
        let matched = entry
            .lines
            .iter()
            .filter(|l| l.to * 1000.0 > start as f64 && l.from * 1000.0 < end as f64)
            .collect::<Vec<_>>();
        Ok(
            json!({"status":"ok","language":entry.language,"track_id":entry.id,"window":{"start_ms":start,"end_ms":end},"lines":matched.iter().take(200).collect::<Vec<_>>(),"truncated":matched.len()>200,"source":"platform_subtitle","is_audio_understanding":false}),
        )
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SubtitleCache {
    entries: Vec<SubtitleEntry>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SubtitleEntry {
    scope: Scope,
    video: String,
    page: u32,
    id: String,
    language: String,
    lines: Vec<platform_bilibili::SubtitleLine>,
    cached_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    #[test]
    fn source_linked_inspection_is_cropped_lossless_account_bound_and_does_not_advance() {
        let root = std::env::temp_dir().join(format!(
            "inspect-view-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut runtime = Runtime::open(&root, "owner", None).unwrap();
        let scope = runtime.scope(Platform::Bilibili);
        let video = runtime
            .store
            .import(
                &scope,
                "synthetic-video",
                "video/mp4",
                &mut include_bytes!("../../tests/fixtures/watch-synthetic.mp4").as_slice(),
            )
            .unwrap();
        let manifest_data = json!({"schema":"agent-runtime.bilibili.v1","info":{"aid":170001,"bvid":"BV17x411w7KC","duration_seconds":2},"page":1,"video":video,"audio":null});
        let manifest = runtime
            .store
            .import(
                &scope,
                "synthetic-manifest",
                "application/json",
                &mut serde_json::to_vec(&manifest_data).unwrap().as_slice(),
            )
            .unwrap();
        for quality in ["preview", "1080p"] {
            runtime.locators.put(&scope,"task-video-cache",&json!({"schema":1,"quality":quality,"aid":170001,"bvid":"BV17x411w7KC","page":1,"video":video,"audio":null,"manifest":manifest,"created_at":crate::agent_host::now().unwrap()})).unwrap();
        }
        let target = runtime
            .save_task_target(&TaskTarget::Video {
                video: "BV17x411w7KC".into(),
            })
            .unwrap();
        let begun=runtime.watch_video(&json!({"action":"start","target_ref":target,"duration_ms":500,"interval_ms":500,"transcribe":false,"include_subtitles":false})).unwrap();
        let frame = begun["window"]["frames"][0]["frame_ref"].as_str().unwrap();
        let detail=runtime.read_media(&json!({"reference":frame,"quality":"1080p","region":{"x":0.5,"y":0.5,"width":0.5,"height":0.5}})).unwrap();
        assert_eq!(detail["window"]["frames"].as_array().unwrap().len(), 1);
        assert_eq!(detail["window"]["frames"][0]["image"]["mime"], "image/png");
        assert_eq!(detail["window"]["frames"][0]["width"], 32);
        assert_eq!(detail["source_quality"]["actual"]["width"], 64);
        assert_eq!(detail["source_quality"]["request_met"], false);
        assert_eq!(detail["inspection"]["progress_advanced"], false);
        assert_eq!(runtime.account, "owner");
        let id = begun["session"]["watch_ref"].as_str().unwrap();
        let reread = runtime
            .watch_video(
                &json!({"action":"inspect","watch_ref":id,"start_ms":500,"quality":"1080p"}),
            )
            .unwrap();
        assert_eq!(
            reread["inspection"]["session"]["revision"],
            begun["session"]["revision"]
        );
        let status = runtime
            .watch_video(&json!({"action":"status","watch_ref":id}))
            .unwrap();
        assert_eq!(
            status["session"]["next_start_ms"],
            begun["session"]["next_start_ms"]
        );
        assert_eq!(status["session"]["revision"], begun["session"]["revision"]);
        assert!(
            runtime
                .watch_video(&json!({"action":"inspect","frame_ref":frame,"account":"other"}))
                .is_err()
        );
        assert!(
            runtime
                .watch_video(&json!({"action":"inspect","frame_ref":frame,"start_ms":999}))
                .is_err()
        );
        assert!(runtime.watch_video(&json!({"action":"inspect","frame_ref":frame,"region":{"x":0.9,"y":0.,"width":0.5,"height":1.}})).is_err());
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }
    fn pcm() -> Vec<u8> {
        let bytes = 32000u32 * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + bytes).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&32000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&bytes.to_le_bytes());
        wav.resize(44 + bytes as usize, 0);
        wav
    }
    #[test]
    fn one_watch_call_observes_requested_preferences_aligns_subtitles_and_transcribes_at_most_once()
    {
        let root = std::env::temp_dir().join(format!(
            "watch-composed-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut runtime = Runtime::open(&root, "owner", None).unwrap();
        let scope = runtime.scope(Platform::Bilibili);
        let now = crate::agent_host::now().unwrap();
        let video = runtime
            .store
            .import(
                &scope,
                "synthetic-video",
                "video/mp4",
                &mut include_bytes!("../../tests/fixtures/watch-synthetic.mp4").as_slice(),
            )
            .unwrap();
        let audio = runtime
            .store
            .import(
                &scope,
                "synthetic-audio",
                "audio/wav",
                &mut pcm().as_slice(),
            )
            .unwrap();
        let data = json!({"schema":"agent-runtime.bilibili.v1","info":{"aid":170001,"bvid":"BV17x411w7KC","title":"synthetic","duration_seconds":2},"page":1,"video":video,"audio":audio,"subtitles":{"tracks":1},"preview_only":false});
        let manifest = runtime
            .store
            .import(
                &scope,
                "synthetic-manifest",
                "application/json",
                &mut serde_json::to_vec(&data).unwrap().as_slice(),
            )
            .unwrap();
        runtime.locators.put(&scope,"task-video-cache",&json!({"schema":1,"aid":170001,"bvid":"BV17x411w7KC","page":1,"video":video,"audio":audio,"manifest":manifest,"created_at":now})).unwrap();
        crate::feature_state::Store::open(&root, "subtitle-cache", 2 * 1024 * 1024)
            .unwrap()
            .update::<SubtitleCache, _>(|cache| {
                cache.entries.push(SubtitleEntry {
                    scope: scope.clone(),
                    video: "BV17x411w7KC".into(),
                    page: 1,
                    id: "54321".into(),
                    language: "zh-TW".into(),
                    lines: vec![platform_bilibili::SubtitleLine {
                        from: 0.5,
                        to: 1.5,
                        content: "wrong locale".into(),
                    }],
                    cached_at: now,
                });
                cache.entries.push(SubtitleEntry {
                    scope: scope.clone(),
                    video: "BV17x411w7KC".into(),
                    page: 1,
                    id: "67890".into(),
                    language: "ai-zh".into(),
                    lines: vec![platform_bilibili::SubtitleLine {
                        from: 0.5,
                        to: 1.5,
                        content: "cached aligned automatic cue".into(),
                    }],
                    cached_at: now,
                });
                cache.entries.push(SubtitleEntry {
                    scope: scope.clone(),
                    video: "BV17x411w7KC".into(),
                    page: 1,
                    id: "12345".into(),
                    language: "zh-CN".into(),
                    lines: vec![platform_bilibili::SubtitleLine {
                        from: 0.5,
                        to: 1.5,
                        content: "cached aligned cue".into(),
                    }],
                    cached_at: now,
                });
                Ok(())
            })
            .unwrap();
        let target = runtime
            .save_task_target(&TaskTarget::Video {
                video: "BV17x411w7KC".into(),
            })
            .unwrap();
        super::super::speech::reset_transcription_calls();
        let no_speech=runtime.watch_video(&json!({"target_ref":target,"duration_ms":2000,"interval_ms":1000,"transcribe":false,"language":"zh-CN"})).unwrap();
        assert_eq!(super::super::speech::transcription_calls(), 0);
        assert!(no_speech.get("speech").is_none());
        assert_eq!(
            no_speech["subtitles"]["lines"][0]["content"],
            "cached aligned cue"
        );
        assert_eq!(no_speech["subtitles"]["language"], "zh-CN");
        super::super::speech::reset_transcription_calls();
        let with_speech=runtime.watch_video(&json!({"target_ref":target,"duration_ms":2000,"interval_ms":1000,"language":"ai-zh"})).unwrap();
        assert_eq!(super::super::speech::transcription_calls(), 1);
        assert!(with_speech.get("speech").is_some());
        assert_eq!(
            with_speech["subtitles"]["lines"].as_array().unwrap().len(),
            1
        );
        assert_eq!(with_speech["subtitles"]["language"], "ai-zh");

        let silent=runtime.watch_video(&json!({"target_ref":target,"duration_ms":1000,"transcribe":false,"include_subtitles":false})).unwrap();
        assert_eq!(silent["subtitles"]["status"], "disabled");
        let preferences =
            media_preferences(&json!({"language":"12345"}), &MediaPreferences::default()).unwrap();
        assert_eq!(preferences.subtitle_language.as_deref(), Some("12345"));
        assert!(preferences.speech_language.is_none());
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }
}
