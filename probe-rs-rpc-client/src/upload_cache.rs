//! Content-addressed client upload cache for remote RPC sessions.
//!
//! Remote uploads are keyed by `(canonical source path, content hash)` so
//! changed bytes at the same path produce a new upload instead of reusing the
//! stale one. Entries are never evicted: the server keeps every uploaded temp
//! file for the lifetime of the connection, so a recorded upload stays valid
//! and dropping the entry would only cause the same bytes to be sent again.

use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// SHA-256 digest of a local file's contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    pub fn from_bytes(data: &[u8]) -> Self {
        let digest = Sha256::digest(data);
        Self(digest.into())
    }
}

/// A resolved local upload: canonical path, content identity, and the path
/// the RPC server should read (remote temp path, or the local path in-process).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedUpload {
    pub canonical_path: PathBuf,
    pub content_hash: ContentHash,
    pub remote_path: PathBuf,
}

impl ResolvedUpload {
    pub fn server_path(&self) -> &Path {
        &self.remote_path
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct UploadCacheKey {
    canonical_path: PathBuf,
    content_hash: ContentHash,
}

/// Cache of prior remote uploads for one connection.
#[derive(Debug, Default)]
pub(crate) struct UploadCache {
    entries: HashMap<UploadCacheKey, PathBuf>,
}

impl UploadCache {
    pub(crate) fn lookup(
        &self,
        canonical_path: &Path,
        content_hash: ContentHash,
    ) -> Option<PathBuf> {
        self.entries
            .get(&UploadCacheKey {
                canonical_path: canonical_path.to_path_buf(),
                content_hash,
            })
            .cloned()
    }

    /// Record a successful upload.
    pub(crate) fn insert(
        &mut self,
        canonical_path: PathBuf,
        content_hash: ContentHash,
        remote_path: PathBuf,
    ) {
        self.entries.insert(
            UploadCacheKey {
                canonical_path,
                content_hash,
            },
            remote_path,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(bytes: &[u8]) -> ContentHash {
        ContentHash::from_bytes(bytes)
    }

    #[test]
    fn changed_content_at_same_path_is_a_miss() {
        let mut cache = UploadCache::default();
        let path = PathBuf::from("/tmp/firmware.elf");

        cache.insert(path.clone(), hash(b"v1"), PathBuf::from("/remote/v1"));

        assert_eq!(
            cache.lookup(&path, hash(b"v1")),
            Some(PathBuf::from("/remote/v1"))
        );
        assert_eq!(cache.lookup(&path, hash(b"v2")), None);
    }

    /// Rebuilding back to a previously uploaded binary reuses that upload,
    /// since the server still holds the temp file.
    #[test]
    fn earlier_content_at_same_path_stays_cached() {
        let mut cache = UploadCache::default();
        let path = PathBuf::from("/tmp/firmware.elf");

        cache.insert(path.clone(), hash(b"v1"), PathBuf::from("/remote/v1"));
        cache.insert(path.clone(), hash(b"v2"), PathBuf::from("/remote/v2"));

        assert_eq!(
            cache.lookup(&path, hash(b"v1")),
            Some(PathBuf::from("/remote/v1"))
        );
        assert_eq!(
            cache.lookup(&path, hash(b"v2")),
            Some(PathBuf::from("/remote/v2"))
        );
    }

    #[test]
    fn identical_content_at_different_paths_is_tracked_separately() {
        let mut cache = UploadCache::default();
        let elf = PathBuf::from("/tmp/firmware.elf");
        let copy = PathBuf::from("/tmp/copy.elf");

        cache.insert(elf.clone(), hash(b"elf"), PathBuf::from("/remote/elf"));

        assert_eq!(
            cache.lookup(&elf, hash(b"elf")),
            Some(PathBuf::from("/remote/elf"))
        );
        assert_eq!(cache.lookup(&copy, hash(b"elf")), None);
    }
}
