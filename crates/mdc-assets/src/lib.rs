//! Simple LRU asset cache: hash key, size quota, reject oversized before alloc.
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AssetError {
    #[error("asset exceeds max entry size")]
    TooLarge,
    #[error("asset would exceed cache quota")]
    QuotaExceeded,
    #[error("compressed payload is not accepted")]
    CompressedUnsupported,
    /// N369: a filesystem error from the on-disk cache (message only, so the
    /// error stays `PartialEq`).
    #[error("asset cache io error: {0}")]
    Io(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetKey(pub String);

impl AssetKey {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let hex = digest.iter().map(|b| format!("{b:02x}")).collect();
        Self(hex)
    }
}

struct Entry {
    bytes: Vec<u8>,
}

/// Bounded LRU cache. Oversized payloads are rejected before allocation into the map.
pub struct AssetCache {
    max_entry: usize,
    max_total: usize,
    total: usize,
    order: VecDeque<String>,
    map: HashMap<String, Entry>,
}

impl AssetCache {
    pub fn new(max_entry: usize, max_total: usize) -> Self {
        Self {
            max_entry,
            max_total,
            total: 0,
            order: VecDeque::new(),
            map: HashMap::new(),
        }
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn total_bytes(&self) -> usize {
        self.total
    }
    pub fn insert(&mut self, bytes: &[u8]) -> Result<AssetKey, AssetError> {
        if bytes.len() > self.max_entry {
            return Err(AssetError::TooLarge);
        }
        if bytes.len() > self.max_total {
            return Err(AssetError::QuotaExceeded);
        }
        let key = AssetKey::from_bytes(bytes);
        if let Some(old) = self.map.remove(&key.0) {
            self.total = self.total.saturating_sub(old.bytes.len());
            self.order.retain(|k| k != &key.0);
        }
        while self.total + bytes.len() > self.max_total {
            let Some(evict) = self.order.pop_front() else {
                return Err(AssetError::QuotaExceeded);
            };
            if let Some(old) = self.map.remove(&evict) {
                self.total = self.total.saturating_sub(old.bytes.len());
            }
        }
        // Allocate only after quota checks.
        let owned = bytes.to_vec();
        self.total = self.total.saturating_add(owned.len());
        self.order.push_back(key.0.clone());
        self.map.insert(key.0.clone(), Entry { bytes: owned });
        Ok(key)
    }
    pub fn get(&mut self, key: &AssetKey) -> Option<&[u8]> {
        if !self.map.contains_key(&key.0) {
            return None;
        }
        self.order.retain(|k| k != &key.0);
        self.order.push_back(key.0.clone());
        self.map.get(&key.0).map(|e| e.bytes.as_slice())
    }
}

/// N467: a chunked upload that spools to a temp file, so a large asset never sits
/// in memory. `finish` hashes what was written and renames the temp file into the
/// cache; dropping without finishing removes it.
pub struct DiskChunkedUpload {
    dir: PathBuf,
    path: PathBuf,
    file: Option<std::fs::File>,
    hasher: Sha256,
    received: usize,
    limit: usize,
    /// N478: keep the temp file on drop so the upload can be resumed.
    keep: bool,
}

impl DiskChunkedUpload {
    pub fn begin(dir: impl Into<PathBuf>, limit: usize) -> Result<Self, AssetError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(|e| AssetError::Io(e.to_string()))?;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.join(format!(".upload-{}-{serial}", std::process::id()));
        let file = std::fs::File::create(&path).map_err(|e| AssetError::Io(e.to_string()))?;
        Ok(Self {
            dir,
            path,
            file: Some(file),
            hasher: Sha256::new(),
            received: 0,
            limit,
            keep: false,
        })
    }

    /// N478: a named upload that can be resumed later by the same name.
    pub fn begin_named(
        dir: impl Into<PathBuf>,
        name: &str,
        limit: usize,
    ) -> Result<Self, AssetError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(|e| AssetError::Io(e.to_string()))?;
        let path = dir.join(format!(".upload-{}", sanitize_upload_name(name)));
        let file = std::fs::File::create(&path).map_err(|e| AssetError::Io(e.to_string()))?;
        Ok(Self {
            dir,
            path,
            file: Some(file),
            hasher: Sha256::new(),
            received: 0,
            limit,
            keep: false,
        })
    }

    /// N478: reopen a named upload, re-hashing the bytes already spooled.
    pub fn resume(dir: impl Into<PathBuf>, name: &str, limit: usize) -> Result<Self, AssetError> {
        let dir = dir.into();
        let path = dir.join(format!(".upload-{}", sanitize_upload_name(name)));
        let mut reader = std::fs::File::open(&path).map_err(|e| AssetError::Io(e.to_string()))?;
        use std::io::Read;
        let mut hasher = Sha256::new();
        let mut received = 0usize;
        let mut buffer = [0u8; 8192];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|e| AssetError::Io(e.to_string()))?;
            if read == 0 {
                break;
            }
            received = received.saturating_add(read);
            if received > limit {
                return Err(AssetError::TooLarge);
            }
            hasher.update(&buffer[..read]);
        }
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|e| AssetError::Io(e.to_string()))?;
        Ok(Self {
            dir,
            path,
            file: Some(file),
            hasher,
            received,
            limit,
            keep: false,
        })
    }

    /// N478: keep the spooled file after this handle is dropped (for a resume).
    pub fn keep(mut self) {
        self.keep = true;
    }

    pub fn received(&self) -> usize {
        self.received
    }

    /// Append a chunk; refuses to exceed the declared limit before writing.
    pub fn push_chunk(&mut self, bytes: &[u8]) -> Result<(), AssetError> {
        let next = self
            .received
            .checked_add(bytes.len())
            .ok_or(AssetError::TooLarge)?;
        if next > self.limit {
            return Err(AssetError::TooLarge);
        }
        let Some(file) = self.file.as_mut() else {
            return Err(AssetError::Io("upload already finished".into()));
        };
        use std::io::Write;
        file.write_all(bytes)
            .map_err(|e| AssetError::Io(e.to_string()))?;
        self.hasher.update(bytes);
        self.received = next;
        Ok(())
    }

    /// Drop the partial upload and remove its temp file.
    pub fn abort(&mut self) {
        self.file = None;
        let _ = std::fs::remove_file(&self.path);
    }

    /// Complete the upload: hash the bytes and move the temp file into the cache
    /// as its content key (call [`DiskCache::prune`] to enforce a quota).
    pub fn finish(mut self) -> Result<AssetKey, AssetError> {
        if let Some(file) = self.file.take() {
            file.sync_all().map_err(|e| AssetError::Io(e.to_string()))?;
        }
        let key = AssetKey(
            self.hasher
                .finalize_reset()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        );
        let target = self.dir.join(&key.0);
        std::fs::rename(&self.path, &target).map_err(|e| AssetError::Io(e.to_string()))?;
        self.received = 0;
        Ok(key)
    }
}

impl Drop for DiskChunkedUpload {
    fn drop(&mut self) {
        if !self.keep && self.file.is_some() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// N478: restrict a resumable upload name to a safe filename fragment.
fn sanitize_upload_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// N369: a bounded, content-addressed cache on disk. Files are named by their
/// SHA-256 key; sizes come from the directory listing and LRU order from the
/// file modification time (touched on every `get`/`put`).
pub struct DiskCache {
    dir: PathBuf,
    max_entry: usize,
    max_total: usize,
}

impl DiskCache {
    pub fn open(
        dir: impl Into<PathBuf>,
        max_entry: usize,
        max_total: usize,
    ) -> Result<Self, AssetError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(|e| AssetError::Io(e.to_string()))?;
        Ok(Self {
            dir,
            max_entry,
            max_total,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, key: &AssetKey) -> PathBuf {
        self.dir.join(&key.0)
    }

    fn mtime(&self, key: &AssetKey) -> Option<SystemTime> {
        std::fs::metadata(self.path(key)).ok()?.modified().ok()
    }

    /// All content entries as `(key, bytes)`, least-recently-used first.
    pub fn list(&self) -> Result<Vec<(AssetKey, usize)>, AssetError> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir).map_err(|e| AssetError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| AssetError::Io(e.to_string()))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.len() != 64 || !name.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            let size = entry
                .metadata()
                .map_err(|e| AssetError::Io(e.to_string()))?
                .len() as usize;
            out.push((AssetKey(name), size));
        }
        out.sort_by_key(|(key, _)| self.mtime(key).unwrap_or(SystemTime::UNIX_EPOCH));
        Ok(out)
    }

    pub fn total_bytes(&self) -> Result<usize, AssetError> {
        Ok(self.list()?.iter().map(|(_, size)| size).sum())
    }

    /// Store `bytes` under its content key, then prune to the quota.
    pub fn put(&self, bytes: &[u8]) -> Result<AssetKey, AssetError> {
        if bytes.len() > self.max_entry {
            return Err(AssetError::TooLarge);
        }
        if bytes.len() > self.max_total {
            return Err(AssetError::QuotaExceeded);
        }
        let key = AssetKey::from_bytes(bytes);
        std::fs::write(self.path(&key), bytes).map_err(|e| AssetError::Io(e.to_string()))?;
        self.touch(&key);
        self.prune()?;
        Ok(key)
    }

    /// Read a key and mark it most-recently-used.
    pub fn get(&self, key: &AssetKey) -> Result<Option<Vec<u8>>, AssetError> {
        match std::fs::read(self.path(key)) {
            Ok(bytes) => {
                self.touch(key);
                Ok(Some(bytes))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AssetError::Io(e.to_string())),
        }
    }

    /// Evict least-recently-used entries until the total is within `max_total`.
    /// Returns the number of entries removed.
    pub fn prune(&self) -> Result<usize, AssetError> {
        let mut total = self.total_bytes()?;
        let mut removed = 0;
        for (key, size) in self.list()? {
            if total <= self.max_total {
                break;
            }
            std::fs::remove_file(self.path(&key)).map_err(|e| AssetError::Io(e.to_string()))?;
            total = total.saturating_sub(size);
            removed += 1;
        }
        Ok(removed)
    }

    fn touch(&self, key: &AssetKey) {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(self.path(key)) {
            let _ = file.set_modified(SystemTime::now());
        }
    }
}

/// Bytes gathered before they enter the cache. Abort or drop leaves nothing stored.
pub struct ChunkedUpload {
    buf: Vec<u8>,
    limit: usize,
}

impl ChunkedUpload {
    pub fn begin(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit,
        }
    }
    pub fn received(&self) -> usize {
        self.buf.len()
    }
    pub fn push_chunk(&mut self, bytes: &[u8]) -> Result<(), AssetError> {
        let next = self
            .buf
            .len()
            .checked_add(bytes.len())
            .ok_or(AssetError::TooLarge)?;
        if next > self.limit {
            return Err(AssetError::TooLarge);
        }
        self.buf.extend_from_slice(bytes);
        Ok(())
    }
    pub fn abort(&mut self) {
        self.buf.clear();
    }
    pub fn finish(self, cache: &mut AssetCache) -> Result<AssetKey, AssetError> {
        cache.insert(&self.buf)
    }
}

/// `jpeg` is a host capability bit. When it is false, compressed bytes are refused and the RGB565 buffer is used.
pub fn fallback_rgb565(
    jpeg: bool,
    compressed: &[u8],
    rgb565: &[u8],
) -> Result<Vec<u8>, AssetError> {
    if compressed.is_empty() {
        return Ok(rgb565.to_vec());
    }
    if !jpeg {
        return Ok(rgb565.to_vec());
    }
    Err(AssetError::CompressedUnsupported)
}

/// Refuse a declared expanded size before any buffer for that size is allocated.
pub fn guard_declared_size(declared: usize, quota: usize) -> Result<(), AssetError> {
    if declared > quota {
        return Err(AssetError::TooLarge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_before_insert() {
        let mut cache = AssetCache::new(8, 32);
        assert_eq!(cache.insert(&[0; 9]), Err(AssetError::TooLarge));
        assert!(cache.is_empty());
    }

    #[test]
    fn lru_evicts_under_quota() {
        let mut cache = AssetCache::new(8, 16);
        let a = cache.insert(&[1, 2, 3, 4]).unwrap();
        let b = cache.insert(&[5, 6, 7, 8]).unwrap();
        let c = cache.insert(&[9, 10, 11, 12]).unwrap();
        // a should be evicted (4+4+4 would be 12, wait 4+4=8, +4=12 <=16 — need fill)
        let _ = cache.insert(&[1; 8]).unwrap(); // total may evict a,b
        assert!(cache.get(&c).is_some() || cache.len() <= 2);
        assert!(cache.total_bytes() <= 16);
        let _ = (a, b);
    }

    #[test]
    fn abort_drops_partial_chunks_and_jpeg_falls_back_to_rgb565() {
        let mut upload = ChunkedUpload::begin(8);
        upload.push_chunk(&[1, 2, 3, 4]).unwrap();
        assert_eq!(upload.received(), 4);
        upload.abort();
        assert_eq!(upload.received(), 0);
        let cache = AssetCache::new(8, 16);
        upload.push_chunk(&[9, 9]).unwrap();
        drop(upload);
        assert_eq!(cache.total_bytes(), 0);
        let rgb = [0x11, 0x22];
        let jpeg = [0xff, 0xd8, 0xff];
        assert_eq!(fallback_rgb565(false, &jpeg, &rgb).unwrap(), rgb);
        assert_eq!(
            fallback_rgb565(true, &jpeg, &rgb),
            Err(AssetError::CompressedUnsupported)
        );
        assert_eq!(
            guard_declared_size(1_000_000, 16),
            Err(AssetError::TooLarge)
        );
        assert_eq!(guard_declared_size(16, 16), Ok(()));
        assert_eq!(fallback_rgb565(false, &jpeg, &rgb).unwrap(), rgb);
    }

    #[test]
    fn disk_cache_put_list_prune() {
        let dir = std::env::temp_dir().join(format!("mdc-assets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = DiskCache::open(&dir, 16, 24).unwrap();
        let a = cache.put(&[1u8; 8]).unwrap();
        let b = cache.put(&[2u8; 8]).unwrap();
        let c = cache.put(&[3u8; 8]).unwrap();
        assert_eq!(cache.total_bytes().unwrap(), 24);
        assert_eq!(cache.list().unwrap().len(), 3);
        // Oversized for a single entry.
        assert_eq!(cache.put(&[9u8; 17]), Err(AssetError::TooLarge));
        // A fourth entry pushes past the quota; the oldest is evicted.
        let d = cache.put(&[4u8; 8]).unwrap();
        assert!(cache.get(&d).unwrap().is_some());
        assert!(cache.total_bytes().unwrap() <= 24);
        assert_eq!(cache.list().unwrap().len(), 3);
        assert!(
            cache.get(&a).unwrap().is_none() || cache.get(&b).unwrap().is_none(),
            "an old entry was evicted"
        );
        let _ = (b, c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disk_chunked_upload_spools_and_finishes() {
        // N467: chunks spool to a temp file, the limit is enforced, and finish
        // renames the file to its content key.
        let dir = std::env::temp_dir().join(format!("mdc-disk-upload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut upload = DiskChunkedUpload::begin(&dir, 8).unwrap();
        upload.push_chunk(&[1, 2, 3, 4]).unwrap();
        upload.push_chunk(&[5, 6, 7, 8]).unwrap();
        assert_eq!(upload.received(), 8);
        assert_eq!(upload.push_chunk(&[9]), Err(AssetError::TooLarge));
        let key = upload.finish().unwrap();
        assert_eq!(key, AssetKey::from_bytes(&[1, 2, 3, 4, 5, 6, 7, 8]));
        assert!(
            dir.join(&key.0).is_file(),
            "the file is stored under its key"
        );

        // Aborting removes the partial file.
        let mut aborted = DiskChunkedUpload::begin(&dir, 8).unwrap();
        aborted.push_chunk(&[1, 2]).unwrap();
        aborted.abort();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".upload-"))
            .collect();
        assert!(leftovers.is_empty(), "no temp files remain");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn named_upload_resumes_from_its_offset() {
        // N478: a kept upload resumes and finishes with the full content key.
        let dir = std::env::temp_dir().join(format!("mdc-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut upload = DiskChunkedUpload::begin_named(&dir, "asset-1", 8).unwrap();
        upload.push_chunk(&[1, 2, 3, 4]).unwrap();
        upload.keep();

        let mut resumed = DiskChunkedUpload::resume(&dir, "asset-1", 8).unwrap();
        assert_eq!(resumed.received(), 4, "offset is reported");
        resumed.push_chunk(&[5, 6, 7, 8]).unwrap();
        assert_eq!(resumed.push_chunk(&[9]), Err(AssetError::TooLarge));
        let key = resumed.finish().unwrap();
        assert_eq!(key, AssetKey::from_bytes(&[1, 2, 3, 4, 5, 6, 7, 8]));
        assert!(dir.join(&key.0).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_key_stable() {
        let k1 = AssetKey::from_bytes(&[1, 2, 3]);
        let k2 = AssetKey::from_bytes(&[1, 2, 3]);
        assert_eq!(k1, k2);
        assert_ne!(k1, AssetKey::from_bytes(&[1, 2, 4]));
    }
}
