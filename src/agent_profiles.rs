// SPDX-License-Identifier: MIT
//! Operator-only, persistent execution policy. Models cannot configure profiles.
use runtime_core::{
    Result,
    resources::{FileLock, lock_private, open_regular, private_dir},
};
use runtime_jobs::{AgentContext, ExecutionProfile};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
const MAX_BYTES: u64 = 64 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileInput {
    pub engine: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub default_accounts: BTreeMap<String, String>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    profiles: BTreeMap<String, ExecutionProfile>,
}
pub struct ProfileStore {
    directory: PathBuf,
    document: Document,
    _lock: FileLock,
}
impl ProfileStore {
    pub fn open(state: &Path) -> Result<Self> {
        let directory = state.join("agent-profiles");
        private_dir(&directory)?;
        let lock = lock_private(&directory.join("profiles.lock"))?;
        let path = directory.join("profiles.json");
        let document = match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Document::default(),
            Err(e) => return Err(e.to_string()),
            Ok(_) => {
                let mut file = open_regular(&path, MAX_BYTES)?;
                if file
                    .metadata()
                    .map_err(|e| e.to_string())?
                    .permissions()
                    .mode()
                    & 0o077
                    != 0
                {
                    return Err("agent profiles must be private (0600)".into());
                }
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                serde_json::from_slice::<Document>(&bytes).map_err(|_| "invalid agent profiles")?
            }
        };
        if document.profiles.len() > 32 {
            return Err("agent profile quota exceeded".into());
        }
        for (agent, profile) in &document.profiles {
            validate_agent(agent)?;
            profile.validate()?;
            crate::agent_host::selected_tools(profile.allowed_tools.as_deref())?;
        }
        Ok(Self {
            directory,
            document,
            _lock: lock,
        })
    }
    pub fn get(&self, agent: &str) -> Result<Option<ExecutionProfile>> {
        validate_agent(agent)?;
        Ok(self.document.profiles.get(agent).cloned())
    }
    pub fn list(&self) -> BTreeMap<String, ExecutionProfile> {
        self.document.profiles.clone()
    }
    pub fn set(
        &mut self,
        agent: &str,
        input: ProfileInput,
        expected_revision: Option<u64>,
    ) -> Result<ExecutionProfile> {
        validate_agent(agent)?;
        let revision = self.document.profiles.get(agent).map_or(0, |p| p.revision);
        if expected_revision.is_some_and(|r| r != revision) {
            return Err("agent profile revision changed; reread before replacing".into());
        }
        if revision == 0 && self.document.profiles.len() >= 32 {
            return Err("at most 32 agent profiles".into());
        }
        let profile = ExecutionProfile {
            revision: revision
                .checked_add(1)
                .ok_or("profile revision exhausted")?,
            engine: input.engine,
            model: input.model,
            allowed_tools: input.allowed_tools,
            default_accounts: input.default_accounts,
        };
        profile.validate()?;
        crate::agent_host::selected_tools(profile.allowed_tools.as_deref())?;
        let old = self.document.profiles.insert(agent.into(), profile.clone());
        if let Err(error) = self.save() {
            if let Some(old) = old {
                self.document.profiles.insert(agent.into(), old);
            } else {
                self.document.profiles.remove(agent);
            }
            return Err(error);
        }
        Ok(profile)
    }
    fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.document).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("profile document exceeds 64 KiB".into());
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "profile clock unavailable")?
            .as_nanos();
        let temporary = self
            .directory
            .join(format!(".profiles-{}-{nonce}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let result = (|| {
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            fs::rename(&temporary, self.directory.join("profiles.json"))
                .map_err(|e| e.to_string())?;
            File::open(&self.directory)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}
fn validate_agent(agent: &str) -> Result<()> {
    AgentContext {
        agent: agent.into(),
        ..Default::default()
    }
    .validate()
}

/// References retain their recorded account. Defaults apply only to fresh input.
pub fn apply_default_account(profile: &ExecutionProfile, tool: &str, params: &mut Value) {
    let Some(map) = params.as_object_mut() else {
        return;
    };
    if map.contains_key("account")
        || map.iter().any(|(key, value)| {
            !value.is_null()
                && (key.ends_with("_ref")
                    || key == "continuation"
                    || key == "resource"
                    || key == "reference" && value.as_str().is_some_and(opaque))
        })
    {
        return;
    }
    let platform = if tool == "social_tieba" || tool.starts_with("tieba_") {
        Some("tieba")
    } else if tool == "social_bilibili" || tool.starts_with("bilibili_") {
        Some("bilibili")
    } else if tool.starts_with("github_") {
        Some("github")
    } else if tool.starts_with("qq_") || matches!(tool, "social_chats" | "social_messages") {
        Some("qq")
    } else if matches!(
        tool,
        "social_search" | "social_discover" | "social_emotes" | "social_favorites"
    ) {
        Some("bilibili")
    } else {
        map.get("platform")
            .or_else(|| map.get("latest_own"))
            .and_then(Value::as_str)
            .or_else(|| {
                map.get("reference").and_then(Value::as_str).and_then(|r| {
                    if r.starts_with("https://tieba.baidu.com/") {
                        Some("tieba")
                    } else if [
                        "https://x.com/",
                        "https://www.x.com/",
                        "https://twitter.com/",
                        "https://www.twitter.com/",
                    ]
                    .iter()
                    .any(|prefix| r.starts_with(prefix))
                    {
                        Some("x")
                    } else if r.parse::<platform_bilibili::VideoId>().is_ok()
                        || r.starts_with("https://t.bilibili.com/")
                        || r.starts_with("https://www.bilibili.com/")
                        || r.starts_with("https://b23.tv/")
                    {
                        Some("bilibili")
                    } else {
                        None
                    }
                })
            })
    };
    if let Some(account) = platform
        .and_then(|p| profile.default_accounts.get(p))
        .cloned()
    {
        map.insert("account".into(), Value::String(account));
    }
}
fn opaque(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
