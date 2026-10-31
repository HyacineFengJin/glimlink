// SPDX-License-Identifier: MIT
// Adapted from the Agent Runtime QQ Rust experiment; protocol references in NOTICE.
use crate::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

#[derive(Clone, Deserialize, Serialize)]
pub struct Session {
    pub version: u32,
    pub uin: u32,
    pub uid: String,
    pub sequence: u32,
    #[serde(default, skip_serializing_if = "crate::AppProfile::is_legacy")]
    pub app_profile: crate::AppProfile,
    tgt: String,
    d2: String,
    d2_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    temp_pwd: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    tgtgt: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cookies: String,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Device {
    pub guid: String,
    pub device_name: String,
    pub system_kernel: String,
    #[serde(default = "default_kernel")]
    pub kernel_version: String,
}
fn default_kernel() -> String {
    "6.1.0".into()
}
impl Session {
    pub fn empty() -> Self {
        Self {
            version: 1,
            uin: 0,
            uid: String::new(),
            sequence: 1,
            app_profile: crate::AppProfile::default(),
            tgt: String::new(),
            d2: String::new(),
            d2_key: STANDARD.encode([0; 16]),
            temp_pwd: String::new(),
            tgtgt: String::new(),
            cookies: String::new(),
            extra: BTreeMap::new(),
        }
    }
    pub fn load(path: &Path) -> Result<Self> {
        let meta = fs::metadata(path).map_err(|e| e.to_string())?;
        if meta.mode() & 0o077 != 0 || meta.len() > 65536 {
            return Err("invalid session file permissions or size".into());
        }
        let s: Self = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|_| "invalid session JSON")?;
        s.validate()?;
        Ok(s)
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err("unsupported or incomplete session".into());
        }
        if self.key()?.len() != 16 {
            return Err("invalid session key length".into());
        }
        self.tgt()?;
        self.d2()?;
        self.temp_pwd()?;
        Ok(())
    }
    pub fn has_tickets(&self) -> Result<bool> {
        Ok(self.uin != 0
            && !self.uid.is_empty()
            && !self.tgt()?.is_empty()
            && !self.d2()?.is_empty())
    }
    pub fn temp_pwd(&self) -> Result<Vec<u8>> {
        STANDARD
            .decode(&self.temp_pwd)
            .map_err(|_| "invalid session credential encoding".into())
    }
    pub fn clear_outer(&mut self) {
        self.tgt.clear();
        self.d2.clear();
        self.d2_key = STANDARD.encode([0; 16]);
    }
    pub fn set_credentials(
        &mut self,
        tgt: &[u8],
        d2: &[u8],
        key: &[u8],
        temp_pwd: &[u8],
        uid: Option<&str>,
    ) -> Result<()> {
        if self.uin == 0 || tgt.is_empty() || d2.is_empty() || key.len() != 16 {
            return Err("incomplete login credentials".into());
        }
        if let Some(uid) = uid.filter(|v| !v.is_empty()) {
            self.uid = uid.to_owned();
        }
        if self.uid.is_empty() {
            return Err("login did not provide UID".into());
        }
        self.tgt = STANDARD.encode(tgt);
        self.d2 = STANDARD.encode(d2);
        self.d2_key = STANDARD.encode(key);
        if !temp_pwd.is_empty() {
            self.temp_pwd = STANDARD.encode(temp_pwd);
        }
        Ok(())
    }
    pub fn set_tgtgt(&mut self, value: &[u8]) {
        self.tgtgt = STANDARD.encode(value);
    }
    pub fn tgt(&self) -> Result<Vec<u8>> {
        STANDARD
            .decode(&self.tgt)
            .map_err(|_| "invalid session credential encoding".into())
    }
    pub fn d2(&self) -> Result<Vec<u8>> {
        STANDARD
            .decode(&self.d2)
            .map_err(|_| "invalid session credential encoding".into())
    }
    pub fn key(&self) -> Result<Vec<u8>> {
        STANDARD
            .decode(&self.d2_key)
            .map_err(|_| "invalid session credential encoding".into())
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        write_private_json(path, self)
    }
}
impl Device {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::load(path);
        }
        let mut guid = [0; 16];
        crate::tea::random(&mut guid)?;
        let device = Self {
            guid: guid.iter().map(|v| format!("{v:02x}")).collect(),
            device_name: "Agent-Runtime-Lab".into(),
            system_kernel: "Linux".into(),
            kernel_version: default_kernel(),
        };
        write_private_json(path, &device)?;
        Ok(device)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let meta = fs::metadata(path).map_err(|e| e.to_string())?;
        if meta.mode() & 0o077 != 0 || meta.len() > 4096 {
            return Err("invalid device file permissions or size".into());
        }
        let d: Self = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|_| "invalid device file")?;
        d.guid_bytes()?;
        Ok(d)
    }
    pub fn guid_bytes(&self) -> Result<Vec<u8>> {
        if self.guid.len() != 32 || !self.guid.is_ascii() {
            return Err("invalid device GUID".into());
        }
        (0..32)
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&self.guid[i..i + 2], 16)
                    .map_err(|_| "invalid device GUID".into())
            })
            .collect()
    }
}

pub fn write_private_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| "cannot serialize private state")?;
    write_private_bytes(path, &bytes)
}
pub fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut random = [0; 8];
    crate::tea::random(&mut random)?;
    let suffix = random
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect::<String>();
    let temp = path.with_extension(format!("tmp-{suffix}"));
    let result = (|| -> Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        f.flush()
            .and_then(|_| f.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
