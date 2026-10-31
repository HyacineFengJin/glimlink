// SPDX-License-Identifier: MIT
//! Small private mutable feature records. No background service or resident index.
use runtime_core::{
    Result,
    resources::{lock_private, open_regular, private_dir},
};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub struct Store {
    root: PathBuf,
    path: PathBuf,
    max: usize,
}
impl Store {
    pub fn open(state: &Path, name: &str, max: usize) -> Result<Self> {
        if name.is_empty()
            || name.len() > 48
            || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
            || max == 0
            || max > 4 * 1024 * 1024
        {
            return Err("invalid feature store".into());
        }
        private_dir(state)?;
        let root = state.join(name);
        private_dir(&root)?;
        Ok(Self {
            path: root.join("state.json"),
            root,
            max,
        })
    }
    fn read<T: DeserializeOwned + Default>(&self) -> Result<T> {
        let file = match fs::symlink_metadata(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
            Err(_) => return Err("cannot inspect feature state".into()),
            Ok(_) => open_regular(&self.path, self.max as u64)?,
        };
        if file
            .metadata()
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("feature state must be private".into());
        }
        let mut bytes = Vec::new();
        file.take(self.max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > self.max {
            return Err("feature state exceeds budget".into());
        }
        serde_json::from_slice(&bytes).map_err(|_| "invalid feature state".into())
    }
    pub fn load<T: DeserializeOwned + Default>(&self) -> Result<T> {
        let _lock = lock_private(&self.root.join("state.lock"))?;
        self.read()
    }
    pub fn update<T: DeserializeOwned + Serialize + Default, R>(
        &self,
        edit: impl FnOnce(&mut T) -> Result<R>,
    ) -> Result<R> {
        let _lock = lock_private(&self.root.join("state.lock"))?;
        let mut value: T = self.read()?;
        let result = edit(&mut value)?;
        let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        if bytes.len() > self.max {
            return Err("feature state quota exceeded".into());
        }
        let temporary = self.root.join("state.next");
        if fs::symlink_metadata(&temporary).is_ok() {
            open_regular(&temporary, self.max as u64)?;
            fs::remove_file(&temporary).map_err(|e| e.to_string())?;
        }
        let write = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|e| e.to_string())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            fs::rename(&temporary, &self.path).map_err(|e| e.to_string())?;
            File::open(&self.root)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        if write.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write?;
        Ok(result)
    }
}
