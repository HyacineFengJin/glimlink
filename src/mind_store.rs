// SPDX-License-Identifier: MIT
//! Shared, model-independent persona and ordinary memory. One bounded private
//! JSON snapshot, short write transactions, no resident index or model service.
//! A remembered observation never becomes an operator's personality directive.
use runtime_core::{Result, resources::open_regular};
use runtime_jobs::AgentContext;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const MAX_PERSONAS: usize = 16;
pub const MAX_ROLES: usize = 32;
pub const MAX_MEMORIES: usize = 512;
pub const MAX_DISK_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_PERSONA_BYTES: usize = 4096;
pub const MAX_ROLE_BYTES: usize = 2048;
pub const MAX_MEMORY_BYTES: usize = 2048;
pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024;
pub const MAX_RECALLED_BYTES: usize = 6 * 1024;
pub const MAX_QUERY_BYTES: usize = 16 * 1024;
const LOCK_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Persona {
    pub id: String,
    pub text: String,
    pub revision: u64,
    pub updated_at: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    pub agent: String,
    pub text: String,
    pub revision: u64,
    pub updated_at: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    /// Exactly operator or agent. Only the trusted embedding may select operator.
    pub kind: String,
    pub agent: Option<String>,
    pub job: Option<String>,
}
impl Origin {
    pub fn operator() -> Self {
        Self {
            kind: "operator".into(),
            agent: None,
            job: None,
        }
    }
    pub fn agent(agent: &str, job: Option<&str>) -> Self {
        Self {
            kind: "agent".into(),
            agent: Some(agent.into()),
            job: job.map(str::to_owned),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    pub key: String,
    pub text: String,
    pub tags: Vec<String>,
    pub visibility: String,
    pub conversation: Option<String>,
    pub persona: String,
    /// Globally monotonic; a deleted/recreated key never reuses its old revision.
    pub revision: u64,
    pub source: Origin,
    pub created_at: u64,
    pub updated_at: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remember {
    pub key: String,
    pub text: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub visibility: String,
    pub expected_revision: u64,
    pub source: Origin,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub persona: Persona,
    pub role: Role,
    pub memories: Vec<Memory>,
    pub store_revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    revision: u64,
    personas: Vec<Persona>,
    roles: Vec<Role>,
    memories: Vec<Memory>,
}
impl Default for Record {
    fn default() -> Self {
        Self {
            schema: 1,
            revision: 0,
            personas: vec![],
            roles: vec![],
            memories: vec![],
        }
    }
}
impl Record {
    fn persona(&self, id: &str) -> Result<Persona> {
        namespace(id)?;
        if let Some(persona) = self.personas.iter().find(|p| p.id == id) {
            return Ok(persona.clone());
        }
        if id != "main" {
            return Err("persona is not configured".into());
        }
        Ok(Persona {
            id: "main".into(),
            text: String::new(),
            revision: 0,
            updated_at: 0,
        })
    }
    fn role(&self, agent: &str) -> Result<Role> {
        namespace(agent)?;
        Ok(self
            .roles
            .iter()
            .find(|r| r.agent == agent)
            .cloned()
            .unwrap_or_else(|| Role {
                agent: agent.into(),
                text: String::new(),
                revision: 0,
                updated_at: 0,
            }))
    }
    fn advance(&mut self) -> Result<u64> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("mind revision exhausted")?;
        Ok(self.revision)
    }
    fn validate(&self) -> Result<()> {
        if self.schema != 1
            || self.personas.len() > MAX_PERSONAS
            || self.roles.len() > MAX_ROLES
            || self.memories.len() > MAX_MEMORIES
        {
            return Err("mind store schema or quota is invalid".into());
        }
        let mut personas = BTreeSet::new();
        for p in &self.personas {
            namespace(&p.id)?;
            text(&p.text, MAX_PERSONA_BYTES, false)?;
            if p.revision == 0 || p.revision > self.revision || !personas.insert(p.id.clone()) {
                return Err("invalid persona identity or revision".into());
            }
        }
        let mut roles = BTreeSet::new();
        for role in &self.roles {
            namespace(&role.agent)?;
            text(&role.text, MAX_ROLE_BYTES, false)?;
            if role.revision == 0 || role.revision > self.revision || !roles.insert(&role.agent) {
                return Err("invalid agent role identity or revision".into());
            }
        }
        let mut keys = BTreeSet::new();
        for memory in &self.memories {
            validate_memory(memory)?;
            if memory.revision == 0
                || memory.revision > self.revision
                || (memory.persona != "main" && !personas.contains(&memory.persona))
                || !keys.insert((
                    &memory.persona,
                    &memory.visibility,
                    &memory.conversation,
                    &memory.key,
                ))
            {
                return Err("invalid memory owner, revision or duplicate key".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    state: PathBuf,
    root: PathBuf,
}
struct Transaction {
    file: File,
}
impl Drop for Transaction {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
impl Store {
    /// Opening/reading a missing store never creates directories or lock files.
    pub fn open(state: &Path) -> Result<Self> {
        let store = Self {
            state: state.into(),
            root: state.join("mind"),
        };
        check_optional_directory(&store.state)?;
        check_optional_directory(&store.root)?;
        Ok(store)
    }
    pub fn persona(&self, id: &str) -> Result<Persona> {
        self.read()?.persona(id)
    }
    pub fn role(&self, agent: &str) -> Result<Role> {
        self.read()?.role(agent)
    }
    pub fn set_persona(&self, id: &str, value: &str, expected_revision: u64) -> Result<Persona> {
        namespace(id)?;
        text(value, MAX_PERSONA_BYTES, false)?;
        let _lock = self.lock()?;
        let mut record = self.read()?;
        let index = record.personas.iter().position(|p| p.id == id);
        check_revision(
            index.map_or(0, |i| record.personas[i].revision),
            expected_revision,
        )?;
        if let Some(i) = index {
            if record.personas[i].text == value {
                return Ok(record.personas[i].clone());
            }
        } else if record.personas.len() >= MAX_PERSONAS {
            return Err("persona quota reached".into());
        }
        let updated_at = now()?.max(index.map_or(0, |i| record.personas[i].updated_at));
        let persona = Persona {
            id: id.into(),
            text: value.into(),
            revision: record.advance()?,
            updated_at,
        };
        match index {
            Some(i) => record.personas[i] = persona.clone(),
            None => record.personas.push(persona.clone()),
        };
        self.save(&record)?;
        Ok(persona)
    }
    pub fn set_role(&self, agent: &str, value: &str, expected_revision: u64) -> Result<Role> {
        namespace(agent)?;
        text(value, MAX_ROLE_BYTES, false)?;
        let _lock = self.lock()?;
        let mut record = self.read()?;
        let index = record.roles.iter().position(|r| r.agent == agent);
        check_revision(
            index.map_or(0, |i| record.roles[i].revision),
            expected_revision,
        )?;
        if let Some(i) = index {
            if record.roles[i].text == value {
                return Ok(record.roles[i].clone());
            }
        } else if record.roles.len() >= MAX_ROLES {
            return Err("agent role quota reached".into());
        }
        let updated_at = now()?.max(index.map_or(0, |i| record.roles[i].updated_at));
        let role = Role {
            agent: agent.into(),
            text: value.into(),
            revision: record.advance()?,
            updated_at,
        };
        match index {
            Some(i) => record.roles[i] = role.clone(),
            None => record.roles.push(role.clone()),
        };
        self.save(&record)?;
        Ok(role)
    }
    pub fn snapshot(&self, ctx: &AgentContext, query: &str) -> Result<Snapshot> {
        ctx.validate()?;
        check_query(query)?;
        let record = self.read()?;
        let mut result = Snapshot {
            persona: record.persona(&ctx.persona)?,
            role: record.role(&ctx.agent)?,
            memories: vec![],
            store_revision: record.revision,
        };
        let mut candidates = select(&record, ctx, query, None, 16)?;
        // A task such as "continue" still receives a small recent baseline.
        if candidates.is_empty() {
            candidates = select(&record, ctx, "", None, 16)?;
        }
        for candidate in candidates {
            result.memories.push(candidate);
            if encode(&result.memories, MAX_RECALLED_BYTES).is_err()
                || encode(&result, MAX_SNAPSHOT_BYTES).is_err()
            {
                result.memories.pop();
            }
        }
        encode(&result, MAX_SNAPSHOT_BYTES)?;
        Ok(result)
    }
    pub fn search(
        &self,
        ctx: &AgentContext,
        query: &str,
        visibility: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Memory>> {
        ctx.validate()?;
        check_query(query)?;
        let record = self.read()?;
        record.persona(&ctx.persona)?;
        select(&record, ctx, query, visibility, limit)
    }
    pub fn get(&self, ctx: &AgentContext, key: &str, visibility: &str) -> Result<Memory> {
        ctx.validate()?;
        memory_key(key)?;
        let conversation = conversation(ctx, visibility)?;
        let record = self.read()?;
        record.persona(&ctx.persona)?;
        record
            .memories
            .into_iter()
            .find(|m| {
                m.persona == ctx.persona
                    && m.visibility == visibility
                    && m.conversation == conversation
                    && m.key == key
            })
            .ok_or_else(|| "memory not found".into())
    }
    pub fn remember(&self, ctx: &AgentContext, input: Remember) -> Result<Memory> {
        ctx.validate()?;
        memory_key(&input.key)?;
        text(&input.text, MAX_MEMORY_BYTES, true)?;
        tags(&input.tags)?;
        origin(&input.source)?;
        if input.source.kind == "agent" && input.source.agent.as_deref() != Some(ctx.agent.as_str())
        {
            return Err("memory origin differs from the current agent".into());
        }
        let conversation = conversation(ctx, &input.visibility)?;
        let _lock = self.lock()?;
        let mut record = self.read()?;
        record.persona(&ctx.persona)?;
        let index = record.memories.iter().position(|m| {
            m.persona == ctx.persona
                && m.visibility == input.visibility
                && m.conversation == conversation
                && m.key == input.key
        });
        if let Some(i) = index {
            if record.memories[i].source.kind == "operator" && input.source.kind != "operator" {
                return Err("operator memory can only be revised through management".into());
            }
        }
        check_revision(
            index.map_or(0, |i| record.memories[i].revision),
            input.expected_revision,
        )?;
        if let Some(i) = index {
            let m = &record.memories[i];
            if m.text == input.text && m.tags == input.tags && m.source == input.source {
                return Ok(m.clone());
            }
        } else if record.memories.len() >= MAX_MEMORIES {
            return Err("memory quota reached; forget an explicit record first".into());
        }
        let stamp = now()?.max(index.map_or(0, |i| record.memories[i].updated_at));
        let memory = Memory {
            key: input.key,
            text: input.text,
            tags: input.tags,
            visibility: input.visibility,
            conversation,
            persona: ctx.persona.clone(),
            revision: record.advance()?,
            source: input.source,
            created_at: index.map_or(stamp, |i| record.memories[i].created_at),
            updated_at: stamp,
        };
        match index {
            Some(i) => record.memories[i] = memory.clone(),
            None => record.memories.push(memory.clone()),
        };
        self.save(&record)?;
        Ok(memory)
    }
    /// Ordinary AI forget is revision-checked and cannot remove operator facts.
    /// This deletes only the active index, not previous model threads or backups.
    pub fn forget(
        &self,
        ctx: &AgentContext,
        key: &str,
        visibility: &str,
        expected_revision: u64,
    ) -> Result<bool> {
        self.forget_inner(ctx, key, visibility, expected_revision, false)
    }
    /// Management-only variant; do not expose this authority as a model field.
    pub fn forget_as_operator(
        &self,
        ctx: &AgentContext,
        key: &str,
        visibility: &str,
        expected_revision: u64,
    ) -> Result<bool> {
        self.forget_inner(ctx, key, visibility, expected_revision, true)
    }
    fn forget_inner(
        &self,
        ctx: &AgentContext,
        key: &str,
        visibility: &str,
        expected_revision: u64,
        operator: bool,
    ) -> Result<bool> {
        ctx.validate()?;
        memory_key(key)?;
        let conversation = conversation(ctx, visibility)?;
        let _lock = self.lock()?;
        let mut record = self.read()?;
        record.persona(&ctx.persona)?;
        let Some(index) = record.memories.iter().position(|m| {
            m.persona == ctx.persona
                && m.visibility == visibility
                && m.conversation == conversation
                && m.key == key
        }) else {
            return Ok(false);
        };
        if record.memories[index].source.kind == "operator" && !operator {
            return Err("operator memory can only be forgotten through management".into());
        }
        check_revision(record.memories[index].revision, expected_revision)?;
        record.memories.remove(index);
        record.advance()?;
        self.save(&record)?;
        Ok(true)
    }
    pub fn status(&self) -> Result<Value> {
        let record = self.read()?;
        Ok(
            json!({"store_revision":record.revision,"personas":record.personas.len(),"roles":record.roles.len(),"memories":record.memories.len(),"max_personas":MAX_PERSONAS,"max_roles":MAX_ROLES,"max_memories":MAX_MEMORIES,"max_committed_json_bytes":MAX_DISK_BYTES,"max_recalled_memory_bytes":MAX_RECALLED_BYTES,"max_snapshot_bytes":MAX_SNAPSHOT_BYTES,"resident_index":false,"forget_scope":"active_store_only"}),
        )
    }

    fn read(&self) -> Result<Record> {
        check_optional_directory(&self.state)?;
        if !check_optional_directory(&self.root)? {
            return Ok(Record::default());
        }
        let path = self.root.join("store.json");
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Record::default());
            }
            Err(_) => return Err("cannot inspect private mind store".into()),
            Ok(_) => {}
        }
        let file = open_regular(&path, MAX_DISK_BYTES as u64)?;
        check_private_file(&file)?;
        let mut bytes = Vec::new();
        file.take(MAX_DISK_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read private mind store")?;
        if bytes.len() > MAX_DISK_BYTES {
            return Err("mind store byte quota exceeded".into());
        }
        let record: Record =
            serde_json::from_slice(&bytes).map_err(|_| "invalid private mind store")?;
        record.validate()?;
        Ok(record)
    }
    fn lock(&self) -> Result<Transaction> {
        ensure_directory(&self.state)?;
        ensure_directory(&self.root)?;
        let path = self.root.join("write.lock");
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                open_regular(&path, 0)?
            }
            Err(_) => return Err("cannot open mind transaction lock".into()),
        };
        check_private_file(&file)?;
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Transaction { file }),
                Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < LOCK_WAIT => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err("mind transaction busy; retry later".into());
                }
                Err(std::fs::TryLockError::Error(_)) => {
                    return Err("cannot lock mind transaction".into());
                }
            }
        }
    }
    fn save(&self, record: &Record) -> Result<()> {
        record.validate()?;
        let bytes = encode(record, MAX_DISK_BYTES)?;
        check_optional_directory(&self.state)?;
        if !check_optional_directory(&self.root)? {
            return Err("mind directory disappeared".into());
        }
        // One fixed, bounded replacement under the exclusive transaction lock.
        // A crashed replacement never becomes active and is removed safely.
        let temporary = self.root.join("store.next");
        match fs::symlink_metadata(&temporary) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("cannot inspect mind replacement".into()),
            Ok(_) => {
                let stale = open_regular(&temporary, MAX_DISK_BYTES as u64)?;
                check_private_file(&stale)?;
                fs::remove_file(&temporary).map_err(|_| "cannot remove stale mind replacement")?;
                sync_dir(&self.root)?;
            }
        }
        let saved = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|_| "cannot create mind replacement")?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "cannot save mind replacement")?;
            fs::rename(&temporary, self.root.join("store.json"))
                .map_err(|_| "cannot commit mind replacement")?;
            sync_dir(&self.root)
        })();
        if saved.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        saved
    }
}

fn namespace(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:-".contains(&b))
    {
        return Err("mind namespace must be 1..96 ASCII letters, digits, '_', ':' or '-'".into());
    }
    Ok(())
}
fn memory_key(value: &str) -> Result<()> {
    namespace(value)?;
    if value.len() > 64 {
        return Err("memory key must be at most 64 bytes".into());
    }
    Ok(())
}
fn text(value: &str, limit: usize, nonempty: bool) -> Result<()> {
    if value.len() > limit
        || (nonempty && value.trim().is_empty())
        || value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(
            "mind text is empty, contains unsupported controls or exceeds its byte budget".into(),
        );
    }
    Ok(())
}
fn tags(value: &[String]) -> Result<()> {
    let mut seen = BTreeSet::new();
    if value.len() > 8
        || value.iter().any(|tag| {
            tag.is_empty()
                || tag.len() > 32
                || tag.chars().any(char::is_control)
                || !seen.insert(tag)
        })
    {
        return Err(
            "memory tags must be distinct, nonempty and within 8 tags / 32 bytes each".into(),
        );
    }
    Ok(())
}
fn origin(value: &Origin) -> Result<()> {
    match value.kind.as_str() {
        "operator" if value.agent.is_none() && value.job.is_none() => Ok(()),
        "agent" => {
            namespace(
                value
                    .agent
                    .as_deref()
                    .ok_or("agent memory lacks its source")?,
            )?;
            if let Some(job) = &value.job {
                namespace(job)?;
            }
            Ok(())
        }
        _ => Err("invalid memory source authority".into()),
    }
}
fn conversation(ctx: &AgentContext, visibility: &str) -> Result<Option<String>> {
    match visibility {
        "shared" => Ok(None),
        "conversation" => {
            Ok(Some(ctx.conversation.clone().ok_or(
                "conversation memory requires a bound conversation",
            )?))
        }
        _ => Err("memory visibility must be shared or conversation".into()),
    }
}
fn validate_memory(memory: &Memory) -> Result<()> {
    namespace(&memory.persona)?;
    memory_key(&memory.key)?;
    text(&memory.text, MAX_MEMORY_BYTES, true)?;
    tags(&memory.tags)?;
    origin(&memory.source)?;
    if memory.updated_at < memory.created_at
        || !matches!(memory.visibility.as_str(), "shared" | "conversation")
        || (memory.visibility == "shared") != memory.conversation.is_none()
    {
        return Err("invalid memory visibility or timestamp".into());
    }
    // Let the shared context validator own the conversation-key syntax.
    let context = AgentContext {
        persona: memory.persona.clone(),
        agent: memory.source.agent.clone().unwrap_or_else(|| "main".into()),
        conversation: memory.conversation.clone(),
    };
    context.validate()
}
fn check_revision(actual: u64, expected: u64) -> Result<()> {
    if actual != expected {
        Err("mind revision conflict; read the current record before revising it".into())
    } else {
        Ok(())
    }
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| "invalid mind clock".into())
}
fn check_query(query: &str) -> Result<()> {
    if query.len() > MAX_QUERY_BYTES || query.contains('\0') {
        Err("memory query exceeds its input budget".into())
    } else {
        Ok(())
    }
}
fn select(
    record: &Record,
    ctx: &AgentContext,
    query: &str,
    visibility: Option<&str>,
    limit: usize,
) -> Result<Vec<Memory>> {
    if !(1..=16).contains(&limit) {
        return Err("memory result limit must be 1..16".into());
    }
    if let Some(visibility) = visibility {
        conversation(ctx, visibility)?;
    }
    let query = query.trim().to_lowercase();
    let chars = query.chars().take(256).collect::<Vec<_>>();
    let words = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .take(32)
        .map(|s| s.chars().take(32).collect::<String>())
        .collect::<BTreeSet<_>>();
    let pairs = chars
        .windows(2)
        .filter(|pair| pair.iter().all(|c| c.is_alphanumeric() && !c.is_ascii()))
        .map(|pair| pair.iter().collect::<String>())
        .take(128)
        .collect::<BTreeSet<_>>();
    let mut candidates = Vec::new();
    for memory in &record.memories {
        if memory.persona != ctx.persona
            || visibility.is_some_and(|v| v != memory.visibility)
            || (memory.visibility == "conversation" && memory.conversation != ctx.conversation)
        {
            continue;
        }
        let haystack = memory.text.to_lowercase();
        let tags = memory
            .tags
            .iter()
            .map(|tag| tag.to_lowercase())
            .collect::<Vec<_>>();
        let mut score = if query.is_empty() { 1 } else { 0 };
        if !query.is_empty()
            && (haystack.contains(&query) || memory.key.to_lowercase().contains(&query))
        {
            score += 100;
        }
        for word in &words {
            if tags.iter().any(|tag| tag == word) {
                score += 30;
            }
            if haystack.contains(word) || memory.key.to_lowercase().contains(word) {
                score += 8;
            }
        }
        for pair in &pairs {
            if haystack.contains(pair) || tags.iter().any(|tag| tag.contains(pair)) {
                score += 1;
            }
        }
        if score > 0 {
            candidates.push((score, memory));
        }
    }
    candidates.sort_by(|(a_score, a), (b_score, b)| {
        b_score
            .cmp(a_score)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| b.revision.cmp(&a.revision))
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.visibility.cmp(&b.visibility))
    });
    Ok(candidates
        .into_iter()
        .take(limit)
        .map(|(_, memory)| memory.clone())
        .collect())
}
fn check_optional_directory(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("cannot inspect mind directory".into()),
        Ok(meta)
            if meta.is_dir()
                && !meta.file_type().is_symlink()
                && meta.permissions().mode() & 0o077 == 0 =>
        {
            Ok(true)
        }
        Ok(_) => Err("mind directories must be real and private (0700)".into()),
    }
}
fn ensure_directory(path: &Path) -> Result<()> {
    if !check_optional_directory(path)? {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|_| "cannot create private mind directory")?;
        // Persist a newly installed namespace as well as the files inside it.
        sync_dir(path)?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            sync_dir(parent)?;
        }
    }
    if !check_optional_directory(path)? {
        return Err("private mind directory is unavailable".into());
    }
    Ok(())
}
fn check_private_file(file: &File) -> Result<()> {
    let metadata = file
        .metadata()
        .map_err(|_| "cannot inspect private mind file")?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err("mind files must be regular and private (0600)".into());
    }
    Ok(())
}
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| "cannot sync mind directory".into())
}
struct Bounded {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("mind output budget"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    let mut writer = Bounded {
        bytes: vec![],
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| "mind JSON byte budget exceeded")?;
    Ok(writer.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::symlink,
        sync::{
            Arc, Barrier,
            atomic::{AtomicU64, Ordering},
        },
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Lab(PathBuf);
    impl Lab {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "runtime-mind-{}-{}-{}",
                std::process::id(),
                now().unwrap(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn store(&self) -> Store {
            Store::open(&self.0).unwrap()
        }
        fn record(&self) -> PathBuf {
            self.0.join("mind/store.json")
        }
    }
    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn ctx(agent: &str, session: Option<&str>) -> AgentContext {
        AgentContext {
            persona: "main".into(),
            agent: agent.into(),
            conversation: session.map(str::to_owned),
        }
    }
    fn input(agent: &str, key: &str, text: &str, visibility: &str, revision: u64) -> Remember {
        Remember {
            key: key.into(),
            text: text.into(),
            tags: vec![],
            visibility: visibility.into(),
            expected_revision: revision,
            source: Origin::agent(agent, None),
        }
    }
    fn private_file(path: &Path, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }
    #[test]
    fn empty_read_does_not_create_state_and_unknown_persona_is_not_silently_main() {
        let lab = Lab::new();
        let store = lab.store();
        let context = ctx("worker", Some("first"));
        let snapshot = store.snapshot(&context, "remember me").unwrap();
        assert_eq!(snapshot.persona.id, "main");
        assert_eq!(snapshot.persona.revision, 0);
        assert!(snapshot.persona.text.is_empty() && snapshot.role.text.is_empty());
        assert!(snapshot.memories.is_empty());
        assert!(store.search(&context, "", None, 16).unwrap().is_empty());
        assert_eq!(store.status().unwrap()["memories"], 0);
        assert!(store.persona("not-configured").is_err());
        assert!(store.get(&context, "absent", "shared").is_err());
        assert!(
            !lab.0.exists(),
            "read paths must not create even a lock or empty store"
        );
    }
    #[test]
    fn different_agents_share_personality_and_memories_but_keep_roles_and_conversations() {
        let lab = Lab::new();
        let store = lab.store();
        let persona = store
            .set_persona("main", "Speak warmly and use concise answers.", 0)
            .unwrap();
        store
            .set_role("reader", "Read content carefully.", 0)
            .unwrap();
        store
            .set_role("writer", "Draft concise replies.", 0)
            .unwrap();
        let reader = ctx("reader", Some("qq-self"));
        let writer = ctx("writer", Some("bili-comment"));
        let shared = store
            .remember(
                &reader,
                input(
                    "reader",
                    "preferred-color",
                    "The user's preferred color is violet.",
                    "shared",
                    0,
                ),
            )
            .unwrap();
        let private = store
            .remember(
                &reader,
                input(
                    "reader",
                    "private-draft",
                    "A draft only for this conversation.",
                    "conversation",
                    0,
                ),
            )
            .unwrap();
        let reopened = lab.store();
        let snapshot = reopened.snapshot(&writer, "preferred color").unwrap();
        assert_eq!(snapshot.persona, persona);
        assert_eq!(snapshot.role.text, "Draft concise replies.");
        assert_eq!(
            snapshot
                .memories
                .iter()
                .map(|m| m.key.as_str())
                .collect::<Vec<_>>(),
            [shared.key.as_str()]
        );
        assert_eq!(
            reopened.get(&writer, "preferred-color", "shared").unwrap(),
            shared
        );
        assert!(
            reopened
                .get(&writer, "private-draft", "conversation")
                .is_err()
        );
        let same_chat = ctx("writer", Some("qq-self"));
        assert_eq!(
            reopened
                .get(&same_chat, "private-draft", "conversation")
                .unwrap(),
            private
        );
        assert!(
            reopened
                .search(&ctx("writer", None), "", Some("conversation"), 8)
                .is_err()
        );
        assert_eq!(
            reopened
                .search(&ctx("writer", None), "", None, 8)
                .unwrap()
                .len(),
            1
        );
        store
            .set_persona("another", "Another independently configured persona.", 0)
            .unwrap();
        let isolated = AgentContext {
            persona: "another".into(),
            ..writer
        };
        assert!(reopened.search(&isolated, "", None, 16).unwrap().is_empty());
        assert!(
            reopened
                .get(&isolated, "preferred-color", "shared")
                .is_err()
        );
    }
    #[test]
    fn chinese_terms_and_tags_retrieve_real_text_with_attribution() {
        let lab = Lab::new();
        let store = lab.store();
        let context = ctx("worker", None);
        store
            .remember(
                &context,
                input(
                    "worker",
                    "tea",
                    "用户喜欢乌龙茶，周末会自己泡茶。",
                    "shared",
                    0,
                ),
            )
            .unwrap();
        let mut tagged = input(
            "worker",
            "editor",
            "Use the Rust editor configuration.",
            "shared",
            0,
        );
        tagged.tags = vec!["workspace".into(), "tools".into()];
        tagged.source.job = Some("job-observed".into());
        let tagged = store.remember(&context, tagged).unwrap();
        assert_eq!(
            store.search(&context, "喜欢什么茶", None, 8).unwrap()[0].key,
            "tea"
        );
        let found = store
            .search(&context, "workspace", Some("shared"), 8)
            .unwrap();
        assert_eq!(found, [tagged]);
        assert_eq!(found[0].source.kind, "agent");
        assert_eq!(found[0].source.job.as_deref(), Some("job-observed"));
        assert!(
            store
                .search(&context, "genuinely-unrelated-query", None, 8)
                .unwrap()
                .is_empty()
        );
        assert!(store.search(&context, "", None, 0).is_err());
        assert!(store.search(&context, "", None, 17).is_err());
    }
    #[test]
    fn compare_and_swap_prevents_concurrent_writers_and_aba_after_forgetting() {
        let lab = Lab::new();
        let store = lab.store();
        let context = ctx("worker", None);
        let initial = store
            .remember(&context, input("worker", "fact", "initial", "shared", 0))
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = vec![];
        for value in ["first writer", "second writer"] {
            let context = context.clone();
            let store = store.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                store.remember(
                    &context,
                    input("worker", "fact", value, "shared", initial.revision),
                )
            }));
        }
        barrier.wait();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(
            results
                .iter()
                .filter_map(|r| r.as_ref().err())
                .all(|e| e.contains("revision conflict"))
        );
        let current = store.get(&context, "fact", "shared").unwrap();
        assert!(current.revision > initial.revision);
        assert!(
            store
                .forget(&context, "fact", "shared", initial.revision)
                .is_err()
        );
        assert!(
            store
                .forget(&context, "fact", "shared", current.revision)
                .unwrap()
        );
        let recreated = store
            .remember(
                &context,
                input("worker", "fact", "new incarnation", "shared", 0),
            )
            .unwrap();
        assert!(recreated.revision > current.revision);
        let before = fs::read(lab.record()).unwrap();
        assert!(
            store
                .remember(
                    &context,
                    input(
                        "worker",
                        "fact",
                        "stale overwrite",
                        "shared",
                        initial.revision
                    )
                )
                .is_err()
        );
        assert_eq!(fs::read(lab.record()).unwrap(), before);
        assert_eq!(
            lab.store().get(&context, "fact", "shared").unwrap().text,
            "new incarnation"
        );
    }
    #[test]
    fn operator_source_cannot_be_rewritten_forgotten_or_impersonated_by_an_agent() {
        let lab = Lab::new();
        let store = lab.store();
        let context = ctx("worker", None);
        let mut managed = input(
            "worker",
            "owner-rule",
            "The owner's explicit preference.",
            "shared",
            0,
        );
        managed.source = Origin::operator();
        let managed = store.remember(&context, managed).unwrap();
        let before = fs::read(lab.record()).unwrap();
        assert!(
            store
                .remember(
                    &context,
                    input(
                        "worker",
                        "owner-rule",
                        "untrusted replacement",
                        "shared",
                        managed.revision
                    )
                )
                .is_err()
        );
        assert!(
            store
                .forget(&context, "owner-rule", "shared", managed.revision)
                .is_err()
        );
        let mut wrong_agent = input(
            "another-agent",
            "impersonation",
            "wrong source",
            "shared",
            0,
        );
        assert!(store.remember(&context, wrong_agent.clone()).is_err());
        wrong_agent.source = Origin {
            kind: "operator".into(),
            agent: Some("worker".into()),
            job: None,
        };
        assert!(store.remember(&context, wrong_agent).is_err());
        assert_eq!(fs::read(lab.record()).unwrap(), before);
        assert!(
            store
                .forget_as_operator(&context, "owner-rule", "shared", managed.revision)
                .unwrap()
        );
        assert!(store.search(&context, "", None, 8).unwrap().is_empty());
    }
    #[test]
    fn snapshot_has_a_real_total_budget_even_for_maximal_escaped_personality_and_memories() {
        let lab = Lab::new();
        let store = lab.store();
        let context = ctx("worker", Some("chat"));
        store
            .set_persona("main", &"\\".repeat(MAX_PERSONA_BYTES), 0)
            .unwrap();
        store
            .set_role("worker", &"\"".repeat(MAX_ROLE_BYTES), 0)
            .unwrap();
        for n in 0..12 {
            store
                .remember(
                    &context,
                    input(
                        "worker",
                        &format!("fact-{n}"),
                        &"\\".repeat(MAX_MEMORY_BYTES),
                        "shared",
                        0,
                    ),
                )
                .unwrap();
        }
        let snapshot = store.snapshot(&context, "continue").unwrap();
        assert_eq!(snapshot.persona.text.len(), MAX_PERSONA_BYTES);
        assert_eq!(snapshot.role.text.len(), MAX_ROLE_BYTES);
        assert!(serde_json::to_vec(&snapshot).unwrap().len() <= MAX_SNAPSHOT_BYTES);
        assert!(serde_json::to_vec(&snapshot.memories).unwrap().len() <= MAX_RECALLED_BYTES);
        assert!(
            snapshot.memories.len() < 12,
            "bounded context must not dump the entire memory store"
        );
        assert_eq!(store.search(&context, "", None, 16).unwrap().len(), 12);
    }
    #[test]
    fn count_byte_input_and_profile_quotas_fail_without_overwriting_committed_data() {
        let lab = Lab::new();
        let store = lab.store();
        let context = ctx("worker", None);
        let initial = store
            .remember(&context, input("worker", "first", "keep me", "shared", 0))
            .unwrap();
        let mut record = store.read().unwrap();
        record.memories = (0..MAX_MEMORIES)
            .map(|n| Memory {
                key: format!("fact-{n}"),
                ..initial.clone()
            })
            .collect();
        {
            let _lock = store.lock().unwrap();
            store.save(&record).unwrap();
        }
        let before = fs::read(lab.record()).unwrap();
        assert!(
            store
                .remember(
                    &context,
                    input("worker", "over-count", "not stored", "shared", 0)
                )
                .is_err()
        );
        let mut enormous = record.clone();
        for memory in &mut enormous.memories {
            memory.text = "\\".repeat(MAX_MEMORY_BYTES);
        }
        {
            let _lock = store.lock().unwrap();
            assert!(store.save(&enormous).is_err());
        }
        assert_eq!(fs::read(lab.record()).unwrap(), before);
        assert!(
            store
                .remember(
                    &context,
                    input("worker", "../escape", "not stored", "shared", 0)
                )
                .is_err()
        );
        assert!(
            store
                .remember(
                    &context,
                    input(
                        "worker",
                        "oversized",
                        &"a".repeat(MAX_MEMORY_BYTES + 1),
                        "shared",
                        0
                    )
                )
                .is_err()
        );
        assert!(
            store
                .remember(
                    &context,
                    input("worker", "chat-only", "not stored", "conversation", 0)
                )
                .is_err()
        );
        assert!(
            store
                .set_persona("main", &"p".repeat(MAX_PERSONA_BYTES + 1), 0)
                .is_err()
        );
        assert!(
            store
                .set_role("worker", &"r".repeat(MAX_ROLE_BYTES + 1), 0)
                .is_err()
        );
        for n in 0..MAX_PERSONAS {
            store.set_persona(&format!("p-{n}"), "persona", 0).unwrap();
        }
        assert!(store.set_persona("over-persona", "persona", 0).is_err());
        for n in 0..MAX_ROLES {
            store.set_role(&format!("agent-{n}"), "role", 0).unwrap();
        }
        assert!(store.set_role("over-role", "role", 0).is_err());
    }
    #[test]
    fn malformed_or_nonprivate_files_and_symlinks_are_rejected_without_repairs() {
        let lab = Lab::new();
        let store = lab.store();
        store.set_persona("main", "private", 0).unwrap();
        let path = lab.record();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.persona("main").is_err());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let saved = lab.0.join("saved.json");
        fs::rename(&path, &saved).unwrap();
        symlink(&saved, &path).unwrap();
        assert!(store.snapshot(&ctx("worker", None), "").is_err());
        fs::remove_file(&path).unwrap();
        private_file(&path, b"{broken");
        assert!(store.persona("main").is_err());
        assert!(store.set_role("worker", "role", 0).is_err());
        let bad_root = lab.0.join("alias");
        symlink(lab.0.join("mind"), &bad_root).unwrap();
        assert!(Store::open(&bad_root).is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(&saved, &path).unwrap();
        fs::set_permissions(lab.0.join("mind"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Store::open(&lab.0).is_err());
    }
    #[test]
    fn stale_private_replacement_is_not_read_and_is_cleaned_on_next_committed_write() {
        let lab = Lab::new();
        let store = lab.store();
        let initial = store.set_persona("main", "committed", 0).unwrap();
        private_file(&lab.0.join("mind/store.next"), b"uncommitted garbage");
        assert_eq!(lab.store().persona("main").unwrap(), initial);
        let changed = store
            .set_persona("main", "next committed", initial.revision)
            .unwrap();
        assert_eq!(lab.store().persona("main").unwrap(), changed);
        assert!(!lab.0.join("mind/store.next").exists());
        assert_eq!(
            fs::metadata(lab.record()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(lab.0.join("mind"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}
