//! JSON-file persistence. The whole dataset is small, so it lives in memory behind a
//! lock and is written atomically (temp file + rename) after every change.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::sync::{RwLock, RwLockReadGuard};

use crate::model::Data;

pub struct Store {
    path: PathBuf,
    data: RwLock<Data>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let data = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s)
                .with_context(|| format!("corrupt data file {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        Ok(Self {
            path,
            data: RwLock::new(data),
        })
    }

    pub async fn read(&self) -> RwLockReadGuard<'_, Data> {
        self.data.read().await
    }

    /// Apply a change and persist it.
    pub async fn mutate<T>(&self, f: impl FnOnce(&mut Data) -> T) -> Result<T> {
        let mut guard = self.data.write().await;
        let out = f(&mut guard);
        let json = serde_json::to_vec_pretty(&*guard)?;
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, json)
            .await
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(out)
    }
}
