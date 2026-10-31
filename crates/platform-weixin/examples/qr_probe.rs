use platform_weixin::{Client, Config, Result};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, path::Path};
fn run() -> Result<()> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/state/weixin-probe".into());
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).map_err(|_| platform_weixin::Error::Io)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| platform_weixin::Error::Io)?;
    let client = Client::new(Config::default())?;
    let mut attempt = client.begin_login()?;
    let receipt = client.download_qr(&mut attempt, &dir.join("qrcode.img"))?;
    attempt.save(&dir.join("attempt.json"))?;
    let status = if std::env::args().any(|v| v == "--poll-once") {
        let status = client.poll_login(&mut attempt)?;
        attempt.save(&dir.join("attempt.json"))?;
        Some(status)
    } else {
        None
    };
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("VmHWM:")
                    .and_then(|v| v.split_whitespace().next())
                    .and_then(|v| v.parse::<u64>().ok())
            })
        });
    println!(
        "{}",
        json!({"qr":receipt,"poll_status":status,"attempt_saved":true,"account_login_completed":false,"messages_sent":0,"max_rss_kib":rss,
            "cgroup_memory_peak_bytes":std::fs::read_to_string("/sys/fs/cgroup/memory.peak").ok().and_then(|s|s.trim().parse::<u64>().ok())})
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{}", serde_json::to_string(&error).unwrap());
        std::process::exit(1);
    }
}
