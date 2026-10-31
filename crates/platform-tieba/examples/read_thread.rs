use platform_tieba::{Client, Config, Fragment, MediaKind, ThreadId};
use serde_json::json;
use std::{
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
};
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let id: ThreadId = args.next().unwrap_or_else(|| "8537603600".into()).parse()?;
    let output = args.next();
    let client = Client::new(Config::default())?;
    let first = client.thread_page(&id, 1)?;
    let mut second = None;
    if first.data.page.total_pages > 1 {
        second = Some(client.thread_page(&id, 2)?);
    }
    let mut comment_result = None;
    if let Some(post) = first.data.posts.iter().find(|v| v.reply_count > 0) {
        comment_result = Some(match client.comments(&id, post.id, 1) {
            Ok(page) => {
                json!({"parent_post_id":post.id,"comments":page.data.comments.len(),"page":page.data.page})
            }
            Err(error) => json!({"error":error.to_string()}),
        });
    }
    let mut image_result = None;
    if let Some(dir) = output {
        let dir = Path::new(&dir);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let mut source = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join("thread-source.pb"))?;
        source.write_all(&first.raw_source)?;
        let mut report = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join("thread-page.json"))?;
        serde_json::to_writer(&mut report, &first.data)?;
        let image = first
            .data
            .posts
            .iter()
            .chain(second.iter().flat_map(|p| p.data.posts.iter()))
            .flat_map(|p| p.fragments.iter())
            .find_map(|v| {
                if let Fragment::Media { resource, .. } = v {
                    if resource.kind == MediaKind::Image {
                        Some(resource)
                    } else {
                        None
                    }
                } else {
                    None
                }
            });
        if let Some(resource) = image {
            image_result = Some(
                match client.download_to_path(resource, &dir.join("image.bin"), 16 * 1024 * 1024) {
                    Ok(receipt) => json!({"bytes":receipt.bytes,"mime":receipt.mime}),
                    Err(error) => json!({"error":error.to_string()}),
                },
            );
        }
    }
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|v| {
            v.lines().find_map(|line| {
                line.strip_prefix("VmHWM:")
                    .and_then(|v| v.split_whitespace().next())
                    .and_then(|v| v.parse::<u64>().ok())
            })
        });
    println!(
        "{}",
        json!({"thread_id":id,"title":first.data.title,"forum":first.data.forum,"page":first.data.page,"posts":first.data.posts.len(),"response_bytes":first.response_bytes,"source_bytes":first.raw_source.len(),"second_page_posts":second.as_ref().map(|p|p.data.posts.len()),"comments":comment_result,"image":image_result,"max_rss_kib":rss,"cgroup_memory_peak_bytes":std::fs::read_to_string("/sys/fs/cgroup/memory.peak").ok().and_then(|s|s.trim().parse::<u64>().ok())})
    );
    Ok(())
}
