use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::model::BlobRefV1;
use crate::workspace::StoreError;

/// Content-addressed raw payload storage.
pub trait BlobStore: Send + Sync {
    fn contains(&self, content_hash: &str) -> bool;
}

#[derive(Debug, Clone)]
pub struct FsBlobStore {
    root: PathBuf,
}

impl FsBlobStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        set_mode(&root, 0o700)?;
        Ok(Self { root })
    }

    pub fn put(&self, bytes: &[u8]) -> Result<BlobRefV1, StoreError> {
        let hash = hex::encode(Sha256::digest(bytes));
        let path = self.path(&hash);
        if path.exists() {
            return Ok(BlobRefV1 {
                sha256: hash,
                original_bytes: bytes.len() as u64,
            });
        }
        let parent = path.parent().expect("blob path has a parent");
        fs::create_dir_all(parent)?;
        set_mode(parent, 0o700)?;
        let temp = parent.join(format!(".{hash}.{}.tmp", std::process::id()));
        let compressed = zstd::stream::encode_all(bytes, 3)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        set_mode(&temp, 0o600)?;
        file.write_all(&compressed)?;
        file.sync_all()?;
        match fs::rename(&temp, &path) {
            Ok(()) => {}
            Err(error) if path.exists() => {
                let _ = fs::remove_file(&temp);
                let _ = error;
            }
            Err(error) => return Err(error.into()),
        }
        sync_directory(parent)?;
        Ok(BlobRefV1 {
            sha256: hash,
            original_bytes: bytes.len() as u64,
        })
    }

    pub fn get(&self, hash: &str, preview_limit: usize) -> Result<Vec<u8>, StoreError> {
        let mut file = File::open(self.path(hash))?;
        let mut compressed = Vec::new();
        file.read_to_end(&mut compressed)?;
        let mut decoded = zstd::stream::Decoder::new(compressed.as_slice())?;
        let mut result = Vec::new();
        decoded
            .by_ref()
            .take(preview_limit as u64)
            .read_to_end(&mut result)?;
        Ok(result)
    }

    fn path(&self, hash: &str) -> PathBuf {
        let prefix = hash.get(..2).unwrap_or("00");
        self.root.join(prefix).join(format!("{hash}.zst"))
    }
}

impl BlobStore for FsBlobStore {
    fn contains(&self, content_hash: &str) -> bool {
        self.path(content_hash).exists()
    }
}

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), StoreError> {
    Ok(())
}
