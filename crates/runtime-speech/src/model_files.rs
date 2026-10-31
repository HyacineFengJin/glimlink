// SPDX-License-Identifier: MIT
//! Fixed public model artifacts. Installation is an explicit management action.
use crate::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
pub const REVISION: &str = "02d8350e5402f18725eadb6101b4963d181b0b5e";
pub const MODEL: &str = "whisper-tiny-multilingual-q80";
#[derive(Clone, Copy, Serialize)]
pub struct Artifact {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
    #[serde(skip)]
    url: &'static str,
}
pub const ARTIFACTS: [Artifact; 4] = [
    Artifact {
        name: "model-tiny-q80.gguf",
        bytes: 41841632,
        sha256: "edcc907db61aef092f1244dc1e53c55056b472be343e9bbb4dc12ebd4740392f",
        url: "https://huggingface.co/lmz/candle-whisper/resolve/02d8350e5402f18725eadb6101b4963d181b0b5e/model-tiny-q80.gguf",
    },
    Artifact {
        name: "config-tiny.json",
        bytes: 1983,
        sha256: "ffdccec4f3211f4c63310f2b7098f309fe70f3952cedc5e4d11e43f5b2379b98",
        url: "https://huggingface.co/lmz/candle-whisper/resolve/02d8350e5402f18725eadb6101b4963d181b0b5e/config-tiny.json",
    },
    Artifact {
        name: "tokenizer-tiny.json",
        bytes: 2480452,
        sha256: "dfc530298b6fbed1a97c6472c575b026453706e2a204c7f7038f2c9d208b0759",
        url: "https://huggingface.co/lmz/candle-whisper/resolve/02d8350e5402f18725eadb6101b4963d181b0b5e/tokenizer-tiny.json",
    },
    Artifact {
        name: "melfilters.bytes",
        bytes: 64320,
        sha256: "85818f156f7e189453901a515e4726d270d307f976e161cf9403e8caab405498",
        url: "https://raw.githubusercontent.com/huggingface/candle/0.9.1/candle-examples/examples/whisper/melfilters.bytes",
    },
];
#[derive(Serialize)]
pub struct ModelStatus {
    pub model: &'static str,
    pub revision: &'static str,
    pub quantization: &'static str,
    pub installed: bool,
    pub files: Vec<ArtifactStatus>,
    pub max_window_ms: u64,
    pub on_demand: bool,
}
#[derive(Serialize)]
pub struct ArtifactStatus {
    #[serde(flatten)]
    pub artifact: Artifact,
    pub verified: bool,
}
fn open(path: &Path, max: u64) -> Result<File> {
    let meta =
        fs::symlink_metadata(path).map_err(|_| "speech model artifact missing".to_owned())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > max {
        return Err("invalid speech model artifact".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(0o400000 | 0o4000)
        .open(path)
        .map_err(|_| "cannot open speech model artifact".to_owned())?;
    let current = file
        .metadata()
        .map_err(|_| "cannot stat speech model artifact".to_owned())?;
    if !current.is_file() || current.len() > max {
        return Err("invalid speech model artifact".into());
    }
    Ok(file)
}
fn verify(dir: &Path, a: Artifact) -> Result<()> {
    let mut file = open(&dir.join(a.name), a.bytes)?;
    if file.metadata().map_err(|_| "model stat failed")?.len() != a.bytes {
        return Err("speech model artifact size mismatch".into());
    }
    let mut hash = Sha256::new();
    let mut buf = [0; 32768];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|_| "speech model read failed")?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    if format!("{:x}", hash.finalize()) != a.sha256 {
        return Err("speech model artifact hash mismatch".into());
    }
    Ok(())
}
pub fn status(dir: &Path) -> Result<ModelStatus> {
    let files = ARTIFACTS
        .into_iter()
        .map(|artifact| ArtifactStatus {
            verified: verify(dir, artifact).is_ok(),
            artifact,
        })
        .collect::<Vec<_>>();
    Ok(ModelStatus {
        model: MODEL,
        revision: REVISION,
        quantization: "q80",
        installed: files.iter().all(|f| f.verified),
        files,
        max_window_ms: crate::MAX_WINDOW_MS,
        on_demand: true,
    })
}
pub(crate) fn checked_paths(dir: &Path) -> Result<[PathBuf; 4]> {
    for a in ARTIFACTS {
        verify(dir, a)?;
    }
    Ok(ARTIFACTS.map(|a| dir.join(a.name)))
}
fn checked_url(raw: &str) -> Result<()> {
    if raw.len() > 16384 || raw.contains(['\\', '#']) || raw.bytes().any(|b| b <= 32 || b == 127) {
        return Err("model download URL policy".into());
    }
    let uri: ureq::http::Uri = raw.parse().map_err(|_| "model download URL policy")?;
    let h = uri.host().ok_or("model download host missing")?;
    if uri.scheme_str() != Some("https")
        || !matches!(uri.port_u16(), None | Some(443))
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || !(h == "huggingface.co" || h.ends_with(".hf.co") || h == "raw.githubusercontent.com")
    {
        return Err("model download URL policy".into());
    }
    Ok(())
}
fn download(a: Artifact, path: &Path) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .proxy(None)
        .max_idle_connections(0)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(240)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .user_agent("agent-runtime-speech/0.1")
        .build()
        .into();
    let mut url = a.url.to_owned();
    for _ in 0..6 {
        checked_url(&url)?;
        let mut response = agent
            .get(&url)
            .call()
            .map_err(|_| "speech model download failed")?;
        if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            let location = response
                .headers()
                .get("location")
                .and_then(|h| h.to_str().ok())
                .ok_or("model redirect missing")?;
            url = if location.starts_with('/') && !location.starts_with("//") {
                let uri: ureq::http::Uri = url.parse().map_err(|_| "model URL invalid")?;
                format!(
                    "https://{}{location}",
                    uri.authority().ok_or("model authority missing")?
                )
            } else {
                location.into()
            };
            continue;
        }
        if response.status().as_u16() != 200 {
            return Err("speech model download HTTP error".into());
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| "speech install temporary file exists or unavailable")?;
        let result = (|| {
            let mut total = 0u64;
            let mut hash = Sha256::new();
            let mut buf = [0; 32768];
            let mut reader = response.body_mut().as_reader();
            loop {
                let n = reader
                    .read(&mut buf)
                    .map_err(|_| "speech model download interrupted")?;
                if n == 0 {
                    break;
                }
                total = total
                    .checked_add(n as u64)
                    .filter(|n| *n <= a.bytes)
                    .ok_or("speech model download size limit")?;
                hash.update(&buf[..n]);
                file.write_all(&buf[..n])
                    .map_err(|_| "speech model write failed")?;
            }
            if total != a.bytes || format!("{:x}", hash.finalize()) != a.sha256 {
                return Err("speech model downloaded artifact hash/size mismatch".into());
            }
            file.sync_all()
                .map_err(|_| "speech model sync failed".to_owned())
        })();
        if result.is_err() {
            drop(file);
            let _ = fs::remove_file(path);
        }
        return result;
    }
    Err("speech model redirect limit".into())
}
pub fn install(dir: &Path) -> Result<ModelStatus> {
    fs::create_dir_all(dir).map_err(|_| "cannot create speech model directory")?;
    let meta = fs::symlink_metadata(dir).map_err(|_| "cannot inspect model directory")?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("speech model directory must be a real directory".into());
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|_| "cannot secure model directory")?;
    for a in ARTIFACTS {
        if verify(dir, a).is_ok() {
            continue;
        }
        if dir.join(a.name).exists() {
            return Err("existing speech model artifact failed verification; remove the damaged file explicitly".into());
        }
        let temporary = dir.join(format!(".{}-{}.part", a.name, std::process::id()));
        download(a, &temporary)?;
        if let Err(_) = fs::hard_link(&temporary, dir.join(a.name)) {
            let _ = fs::remove_file(&temporary);
            return Err("cannot install model without overwriting another artifact".into());
        }
        fs::remove_file(&temporary).map_err(|_| "cannot remove model temporary file")?;
        File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(|_| "cannot sync model directory")?;
    }
    status(dir)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn downloader_rejects_non_model_origins() {
        for u in [
            "http://huggingface.co/a",
            "https://127.0.0.1/a",
            "https://huggingface.co@evil.invalid/a",
            "https://evil.huggingface.co/a",
            "https://raw.githubusercontent.com:444/a",
            "https://cdn.hf.co/a#fragment",
        ] {
            assert!(checked_url(u).is_err(), "{u}");
        }
        assert!(checked_url(ARTIFACTS[0].url).is_ok());
        assert!(checked_url("https://cas-bridge.xethub.hf.co/x?signature=opaque").is_ok());
    }
    #[test]
    fn missing_model_is_explicit_and_does_not_download() {
        let p = std::env::temp_dir().join(format!("speech-uninstalled-{}", std::process::id()));
        let s = status(&p).unwrap();
        assert!(!s.installed);
        assert_eq!(s.files.len(), 4);
        assert!(checked_paths(&p).is_err());
        assert!(!p.exists());
    }
}
