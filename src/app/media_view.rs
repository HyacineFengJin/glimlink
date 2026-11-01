// SPDX-License-Identifier: MIT
//! Source-aware rereading. View coordinates refer to the full source, not to
//! an already downsampled JPEG. No browser or enlarged-preview substitute.
use super::*;
pub(super) const FRAME_ORIGIN: &str = "task-frame-origin";
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct FrameOrigin {
    pub video: String,
    pub page: u32,
    pub at_ms: u64,
}
pub(super) fn quality(
    params: &Value,
    default: platform_bilibili::VideoQuality,
) -> Result<platform_bilibili::VideoQuality> {
    params
        .get("quality")
        .map(|v| {
            serde_json::from_value(v.clone())
                .map_err(|_| "quality must be preview, 480p, 720p or 1080p".into())
        })
        .unwrap_or(Ok(default))
}
pub(super) fn view(params: &Value) -> Result<runtime_media::view::ViewOptions> {
    let region = params
        .get("region")
        .map(|v| {
            serde_json::from_value::<runtime_media::view::Region>(v.clone())
                .map_err(|_| "region requires normalized x/y/width/height".to_string())
        })
        .transpose()?;
    if let Some(r) = region {
        r.validate().map_err(|e| e.to_string())?;
    }
    let format = match params
        .get("format")
        .map(|v| v.as_str().ok_or("format must be jpeg or png"))
        .transpose()?
    {
        Some("png") => runtime_media::pictures::OutputFormat::Png,
        Some("jpeg") => runtime_media::pictures::OutputFormat::Jpeg,
        None if region.is_some() => runtime_media::pictures::OutputFormat::Png,
        None => runtime_media::pictures::OutputFormat::Jpeg,
        _ => return Err("format must be jpeg or png".into()),
    };
    Ok(runtime_media::view::ViewOptions { region, format })
}
impl Runtime {
    pub(super) fn inspect_video(&mut self, params: &Value) -> Result<Value> {
        view(params)?;
        let selectors = ["frame_ref", "watch_ref", "target_ref"]
            .iter()
            .filter(|k| params.get(**k).is_some())
            .count();
        if selectors != 1 {
            return Err("inspect requires exactly one frame_ref, watch_ref or target_ref".into());
        }
        let mut request = params.clone();
        request["action"] = json!("window");
        let duration = number(params, "duration_ms", 1000)?;
        if duration == 0 || duration > 1000 || number(params, "interval_ms", duration)? != duration
        {
            return Err(
                "inspect returns one frame: duration 1..1000ms and interval equal to duration"
                    .into(),
            );
        }
        request["duration_ms"] = json!(duration);
        request["interval_ms"] = json!(duration);
        if request.get("quality").is_none() {
            request["quality"] = json!("1080p");
        }
        if request.get("format").is_none() {
            request["format"] = json!("png");
        }
        request["transcribe"] = json!(false);
        request["include_audio"] = json!(false);
        request["include_subtitles"] = json!(false);
        request["_single_frame_detail"] = json!(true);
        let mut session = None;
        let result = if let Some(id) = params.get("frame_ref").and_then(Value::as_str) {
            let scope = self.locators.scope_for(FRAME_ORIGIN, id)?;
            super::tasks::check_account(params, &scope)?;
            let origin: FrameOrigin = self.locators.get(&scope, FRAME_ORIGIN, id)?;
            if params
                .get("start_ms")
                .is_some_and(|v| v.as_u64() != Some(origin.at_ms))
                || params
                    .get("page")
                    .is_some_and(|v| v.as_u64() != Some(u64::from(origin.page)))
            {
                return Err("frame_ref fixes its video part and timestamp".into());
            }
            request.as_object_mut().unwrap().remove("frame_ref");
            request["video"] = json!(origin.video);
            request["page"] = json!(origin.page);
            request["start_ms"] = json!(origin.at_ms);
            self.with_task_scope(&scope, |rt| rt.watch_video_window(&request))?
        } else if let Some(id) = params.get("watch_ref").and_then(Value::as_str) {
            let s =
                crate::watch_sessions::Sessions::open(&self.state)?.get(&self.mind_context, id)?;
            super::tasks::check_account(params, &s.scope)?;
            if params
                .get("expected_revision")
                .is_some_and(|v| v.as_u64() != Some(s.revision))
            {
                return Err("viewing progress changed; reread status".into());
            }
            if params.get("start_ms").is_none() {
                return Err("inspect with watch_ref requires start_ms".into());
            }
            request.as_object_mut().unwrap().remove("watch_ref");
            request["video"] = json!(s.video);
            request["page"] = json!(s.page);
            session =
                Some(json!({"watch_ref":s.id,"revision":s.revision,"next_start_ms":s.next_ms}));
            self.with_task_scope(&s.scope, |rt| rt.watch_video_window(&request))?
        } else {
            if params.get("start_ms").is_none() {
                return Err("inspect with target_ref requires start_ms".into());
            }
            self.watch_video_window(&request)?
        };
        let mut result = result;
        result["inspection"] = json!({"single_frame":true,"source_reread":true,"progress_advanced":false,"session":session,"coordinates":"normalized full source"});
        Ok(result)
    }
    pub(super) fn link_watch_frames(
        &self,
        result: &mut Value,
        scope: &Scope,
        video: &str,
        page: u32,
    ) -> Result<()> {
        if let Some(frames) = result
            .pointer_mut("/window/frames")
            .and_then(Value::as_array_mut)
        {
            let mut batch = self.locators.batch();
            for f in frames {
                let at_ms = f["actual_pts_ms"].as_u64().ok_or("frame time is missing")?;
                let id = batch.put(
                    scope,
                    FRAME_ORIGIN,
                    &FrameOrigin {
                        video: video.into(),
                        page,
                        at_ms,
                    },
                )?;
                f["frame_ref"] = json!(id);
                f["reread_with"] = json!({"tool":"media_watch","arguments":{"action":"inspect","frame_ref":id,"quality":"1080p"}});
            }
            batch.commit();
        }
        Ok(())
    }
}
