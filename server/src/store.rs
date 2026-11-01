//! JSON-file persistence. The whole dataset is small, so it lives in memory behind a
//! lock and is written atomically (temp file + fsync + rename) after every change.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
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

    /// Apply a change and persist it. The change is made on a copy and only becomes
    /// visible once it is safely on disk, so a failed write leaves nothing half-applied.
    ///
    /// # Errors
    /// Returns an error if the data file cannot be written; in-memory state is unchanged.
    pub async fn mutate<T>(&self, f: impl FnOnce(&mut Data) -> T) -> Result<T> {
        let mut guard = self.data.write().await;
        let mut next = guard.clone();
        let out = f(&mut next);
        self.write(&next).await?;
        *guard = next;
        Ok(out)
    }

    async fn write(&self, data: &Data) -> Result<()> {
        let json = serde_json::to_vec_pretty(data)?;
        let tmp = self.path.with_extension("json.tmp");
        // Start from a fresh file so the permissions below always apply.
        let _ = tokio::fs::remove_file(&tmp).await;
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600); // the file contains MyAnimeList tokens
        let mut file = opts
            .open(&tmp)
            .await
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        file.write_all(&json).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&tmp, &self.path)
            .await
            .with_context(|| format!("cannot replace {}", self.path.display()))?;
        // Make the rename itself durable across a power loss.
        #[cfg(unix)]
        if let Some(dir) = self.path.parent() {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            // The new file is already in place, so a failure here is only logged.
            let synced = async { tokio::fs::File::open(dir).await?.sync_all().await }.await;
            if let Err(e) = synced {
                tracing::warn!("could not sync directory {}: {e}", dir.display());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_write_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        let store = Store::open(&path).unwrap();
        store.mutate(|d| d.next_notification_id = 1).await.unwrap();

        // Make the final rename fail by putting a non-empty directory in the way.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir_all(path.join("blocker")).unwrap();
        assert!(store.mutate(|d| d.next_notification_id = 2).await.is_err());
        assert_eq!(store.read().await.next_notification_id, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn data_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        let store = Store::open(&path).unwrap();
        store.mutate(|_| ()).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(Store::open(&path).is_ok(), "round-trips");
    }
}
