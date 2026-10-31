//! Offline fixture exerciser. All paths are caller supplied; no network or account access.
use runtime_media::{
    Limits,
    audio::{probe_audio, segment_audio_wav},
    pictures::{OutputFormat, prepare_image, probe_image},
    video::{probe_video, sample_video_window},
};
use std::path::Path;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let get = |i: usize| a.get(i).map(String::as_str).ok_or("missing argument");
    let limits = Limits::default();
    let result = match get(1)? {
        "video-probe" => serde_json::to_value(probe_video(Path::new(get(2)?), &limits)?)?,
        "video" => serde_json::to_value(sample_video_window(
            Path::new(get(2)?),
            Path::new(get(3)?),
            get(4)?.parse()?,
            get(5)?.parse()?,
            get(6)?.parse()?,
            480,
            &limits,
        )?)?,
        "audio-probe" => serde_json::to_value(probe_audio(Path::new(get(2)?), &limits)?)?,
        "audio" => serde_json::to_value(segment_audio_wav(
            Path::new(get(2)?),
            Path::new(get(3)?),
            get(4)?.parse()?,
            get(5)?.parse()?,
            &limits,
        )?)?,
        "image-probe" => serde_json::to_value(probe_image(Path::new(get(2)?), &limits)?)?,
        "image" => serde_json::to_value(prepare_image(
            Path::new(get(2)?),
            Path::new(get(3)?),
            480,
            OutputFormat::Jpeg,
            &limits,
        )?)?,
        _ => {
            return Err(
                "expected video-probe, video, audio-probe, audio, image-probe, or image".into(),
            );
        }
    };
    let peak = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .map(str::to_string)
        });
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({"result":result,"peak_rss":peak}))?
    );
    Ok(())
}
