//! Read-only public smoke test. No cookie loading and no writes to Bilibili.
use platform_bilibili::{Client, Config, SelectedMedia, VideoId};
use serde_json::json;
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let id: VideoId = args.next().ok_or("expected BV/av ID")?.parse()?;
    // Any destination is explicit; there is no access to host or runtime cookies.
    let destination = match args.next().as_deref() {
        Some("--download-dir") => Some(PathBuf::from(args.next().ok_or("expected directory")?)),
        None => None,
        _ => return Err("expected --download-dir or no extra arguments".into()),
    };
    if args.next().is_some() {
        return Err("unexpected arguments".into());
    }
    let mut client = Client::new(Config::default())?;
    let info = client.video_info(&id)?;
    println!(
        "{}",
        json!({"stage":"metadata","aid":info.aid,"bvid":info.bvid,"title":info.title,"pages":info.pages.len(),"duration_seconds":info.duration_seconds})
    );
    let play = client.playback(&info, 1)?;
    let resources = match &play.media {
        SelectedMedia::Dash { video, audio } => {
            println!(
                "{}",
                json!({"stage":"playback","kind":"dash","video_codec":video.codec,"audio_codec":audio.codec,"height":video.height,"subtitles":play.subtitles.as_ref().map(|s|s.tracks.len()),"subtitle_error":play.subtitle_error,"preview_only":play.preview_only})
            );
            vec![
                ("video.m4s".to_owned(), &video.resource),
                ("audio.m4s".to_owned(), &audio.resource),
            ]
        }
        SelectedMedia::Progressive { segments } => {
            println!(
                "{}",
                json!({"stage":"playback","kind":"progressive","segments":segments.len(),"subtitles":play.subtitles.as_ref().map(|s|s.tracks.len()),"subtitle_error":play.subtitle_error,"preview_only":play.preview_only})
            );
            segments
                .iter()
                .enumerate()
                .map(|(i, r)| (format!("segment-{i}.mp4"), r))
                .collect()
        }
    };
    if let Some(directory) = destination {
        if !directory.is_dir() {
            return Err("download destination must already exist".into());
        }
        for (name, resource) in resources {
            let receipt =
                client.download_to_path(resource, &directory.join(&name), 32 * 1024 * 1024)?;
            println!(
                "{}",
                json!({"stage":"download","file":name,"bytes":receipt.bytes,"content_type":receipt.content_type})
            );
        }
    }
    Ok(())
}
