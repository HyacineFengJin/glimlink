// SPDX-License-Identifier: MIT
use super::*;

impl Runtime {
    pub(super) fn call_gif(&mut self, params: &Value) -> Result<Value> {
        let (scope, source, path) = self.resolve_any(required_str(params, "resource")?)?;
        let start = number(params, "start_ms", 0)?;
        let duration = number(params, "duration_ms", 3000)?;
        let interval = number(params, "interval_ms", 250)?;
        let edge = u32::try_from(number(params, "max_edge", 480)?)
            .map_err(|_| "invalid GIF image size")?;
        let work = WorkDir::new(&self.state)?;
        let output = work.path.join("gif");
        private_dir(&output)?;
        let window = runtime_media::animation::prepare_gif(
            &path,
            &output,
            start,
            duration,
            interval,
            edge,
            &runtime_media::Limits::default(),
        )
        .map_err(|e| e.to_string())?;
        let mut batch = self.store.batch();
        let mut frames = Vec::new();
        for sample in &window.samples {
            let image = batch.import(
                &scope,
                "gif-frame",
                "image/jpeg",
                &mut File::open(&sample.path).map_err(|e| e.to_string())?,
            )?;
            frames.push(
                json!({"image":image,"frame_index":sample.frame_index,"target_ms":sample.target_ms,
                "actual_pts_ms":sample.frame_start_ms,"actual_end_ms":sample.frame_end_ms,
                "frame_delay_ms":sample.frame_delay_ms,"sampling_delay_ms":sample.sampling_delay_ms,
                "width":sample.width,"height":sample.height}),
            );
        }
        let result = json!({"source":source,"probe":window.source,"requested_start_ms":window.requested_start_ms,
            "requested_end_ms":window.requested_end_ms,"frames_decoded":window.frames_decoded,
            "decoded_until_ms":window.decoded_until_ms,"end_of_animation":window.end_of_animation,
            "truncated":window.truncated,"truncation_reason":window.truncation_reason,
            "visual_coverage":window.visual_coverage,"frames":frames,"model_observed":false});
        let manifest = batch.import(
            &scope,
            "gif-window-manifest",
            "application/json",
            &mut serde_json::to_vec(&result)
                .map_err(|e| e.to_string())?
                .as_slice(),
        )?;
        batch.commit();
        Ok(json!({"window":result,"manifest":manifest}))
    }
}
