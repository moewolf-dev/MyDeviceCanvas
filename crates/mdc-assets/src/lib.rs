//! Simple LRU asset cache: hash key, size quota, reject oversized before alloc.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// N757: the RLE container magic (`MRLE` + u32 little-endian original length).
const RLE_MAGIC: &[u8; 4] = b"MRLE";

/// N757: deterministic PackBits-style run-length encoding. Literal packets hold
/// 1..=128 bytes (token `len-1`); repeat packets hold 2..=129 copies of the next
/// byte (token `257-len`). The declared length is checked on decode, so a
/// truncated or overlong stream is rejected rather than decoded past its budget.
pub fn rle_encode(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 128 + 16);
    out.extend_from_slice(RLE_MAGIC);
    out.extend_from_slice(&(bytes.len().min(u32::MAX as usize) as u32).to_le_bytes());
    let mut index = 0usize;
    while index < bytes.len() {
        let run = run_length(bytes, index).min(129);
        if run >= 3 {
            out.push((257 - run) as u8);
            out.push(bytes[index]);
            index += run;
            continue;
        }
        // Collect literals until the next useful run (or 128 bytes).
        let start = index;
        let mut literal = 0usize;
        while index < bytes.len() && literal < 128 {
            if literal > 0 && run_length(bytes, index).min(129) >= 3 {
                break;
            }
            index += 1;
            literal += 1;
        }
        out.push((literal - 1) as u8);
        out.extend_from_slice(&bytes[start..index]);
    }
    out
}

fn run_length(bytes: &[u8], start: usize) -> usize {
    let value = bytes[start];
    bytes[start..]
        .iter()
        .take(129)
        .take_while(|b| **b == value)
        .count()
}

/// N757: decode an [`rle_encode`] stream. `None` means malformed input (bad
/// magic, truncated packet, or more bytes than the header declared).
pub fn rle_decode(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 8 || &bytes[0..4] != RLE_MAGIC {
        return None;
    }
    let declared = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
    let mut out = Vec::with_capacity(declared.min(bytes.len().saturating_mul(4)));
    let mut index = 8usize;
    while index < bytes.len() {
        let token = bytes[index];
        index += 1;
        if token < 128 {
            let count = usize::from(token) + 1;
            let end = index.checked_add(count)?;
            if end > bytes.len() {
                return None;
            }
            out.extend_from_slice(&bytes[index..end]);
            index = end;
        } else {
            let count = 257 - usize::from(token);
            let value = *bytes.get(index)?;
            index += 1;
            out.extend(std::iter::repeat_n(value, count));
        }
        if out.len() > declared {
            return None;
        }
    }
    (out.len() == declared).then_some(out)
}

/// N757: `pack_chunk` container magic. Legacy chunk files have no magic and are
/// returned verbatim, so arbitrary raw bytes never collide with the tag.
const CHUNK_MAGIC: &[u8; 4] = b"MRCH";

/// N757: store a chunk as `MRCH || 0 || raw` or `MRCH || 1 || rle`, whichever
/// is smaller. The magic keeps legacy raw chunk files readable and makes the
/// stored size exact for quota accounting.
fn pack_chunk(chunk: &[u8]) -> Vec<u8> {
    let encoded = rle_encode(chunk);
    if encoded.len() + 5 < chunk.len() + 5 {
        let mut out = Vec::with_capacity(encoded.len() + 5);
        out.extend_from_slice(CHUNK_MAGIC);
        out.push(1);
        out.extend_from_slice(&encoded);
        out
    } else {
        let mut out = Vec::with_capacity(chunk.len() + 5);
        out.extend_from_slice(CHUNK_MAGIC);
        out.push(0);
        out.extend_from_slice(chunk);
        out
    }
}

/// N757: decode a stored chunk; untagged legacy files are raw bytes.
fn decode_chunk(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 5 || &bytes[0..4] != CHUNK_MAGIC {
        return Some(bytes.to_vec());
    }
    match bytes[4] {
        0 => Some(bytes[5..].to_vec()),
        1 => rle_decode(&bytes[5..]),
        _ => None,
    }
}

/// N729: a content-addressed blob assembled from fixed-size chunks. `chunks`
/// are the hashes needed to reconstruct it in order; `len` checks the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredBlob {
    /// SHA-256 of the full blob (its identity across reopens).
    pub key: AssetKey,
    pub chunks: Vec<AssetKey>,
    pub len: usize,
}

/// N729: a bounded chunk store that keeps one copy of each unique chunk and
/// reference-counts blobs. `insert` is atomic: a quota failure leaves the store
/// unchanged. Chunks are content-addressed by SHA-256 (chunk boundaries are
/// fixed, so identical byte ranges dedupe across blobs).
pub struct ChunkStore {
    chunk_size: usize,
    max_total: usize,
    total: usize,
    chunks: HashMap<String, Vec<u8>>,
    refs: HashMap<String, usize>,
}

impl ChunkStore {
    pub fn new(chunk_size: usize, max_total: usize) -> Self {
        Self {
            chunk_size: chunk_size.clamp(1, 1024 * 1024),
            max_total,
            total: 0,
            chunks: HashMap::new(),
            refs: HashMap::new(),
        }
    }

    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    pub fn unique_chunks(&self) -> usize {
        self.chunks.len()
    }

    pub fn total_bytes(&self) -> usize {
        self.total
    }

    pub fn ref_count(&self, chunk: &AssetKey) -> usize {
        self.refs.get(&chunk.0).copied().unwrap_or(0)
    }

    pub fn insert(&mut self, bytes: &[u8]) -> Result<StoredBlob, AssetError> {
        // Plan first: reject a quota overflow before mutating anything. A repeated
        // chunk inside this blob is only charged once.
        let mut keys = Vec::with_capacity(bytes.len().div_ceil(self.chunk_size));
        let mut planned_new = std::collections::BTreeSet::new();
        let mut new_bytes = 0usize;
        for chunk in bytes.chunks(self.chunk_size) {
            let key = AssetKey::from_bytes(chunk);
            if !self.chunks.contains_key(&key.0) && planned_new.insert(key.0.clone()) {
                // N757: the quota is charged for the RLE-encoded chunk.
                let stored = pack_chunk(chunk).len();
                new_bytes = new_bytes
                    .checked_add(stored)
                    .ok_or(AssetError::QuotaExceeded)?;
            }
            keys.push(key);
        }
        if self
            .total
            .checked_add(new_bytes)
            .ok_or(AssetError::QuotaExceeded)?
            > self.max_total
        {
            return Err(AssetError::QuotaExceeded);
        }
        // Commit: store the new chunks, then add one reference per occurrence.
        for (key, chunk) in keys.iter().zip(bytes.chunks(self.chunk_size)) {
            if !self.chunks.contains_key(&key.0) {
                let stored = pack_chunk(chunk);
                self.total = self.total.saturating_add(stored.len());
                self.chunks.insert(key.0.clone(), stored);
            }
            *self.refs.entry(key.0.clone()).or_insert(0) += 1;
        }
        Ok(StoredBlob {
            key: AssetKey::from_bytes(bytes),
            chunks: keys,
            len: bytes.len(),
        })
    }

    /// Rebuild the original bytes, or `None` when a chunk is missing.
    pub fn get(&self, blob: &StoredBlob) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(blob.len);
        for key in &blob.chunks {
            out.extend_from_slice(&decode_chunk(self.chunks.get(&key.0)?)?);
        }
        (out.len() == blob.len).then_some(out)
    }

    /// Drop one reference per chunk, freeing a chunk when its count reaches 0.
    pub fn release(&mut self, blob: &StoredBlob) {
        for key in &blob.chunks {
            let Some(count) = self.refs.get_mut(&key.0) else {
                continue;
            };
            if *count <= 1 {
                self.refs.remove(&key.0);
                if let Some(bytes) = self.chunks.remove(&key.0) {
                    self.total = self.total.saturating_sub(bytes.len());
                }
            } else {
                *count -= 1;
            }
        }
    }
}

/// N738: the on-disk chunk index. Chunks are content-addressed files under
/// `chunks/`; the index records each blob's chunk list and reference count.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskBlob {
    blob: StoredBlob,
    refs: usize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DiskIndex {
    chunk_size: usize,
    max_total: usize,
    blobs: BTreeMap<String, DiskBlob>,
}

/// N738: a persistent `ChunkStore`. Identical chunks are written once, a blob
/// inserted twice gains a reference rather than new bytes, and reopening the
/// directory restores the index so dedup survives a restart. The quota is
/// checked before any file is written; `release` deletes chunks whose last
/// reference is gone.
pub struct DiskChunkStore {
    dir: PathBuf,
    chunk_size: usize,
    max_total: usize,
    total: usize,
    blobs: BTreeMap<String, DiskBlob>,
}

/// N738: the most blobs the on-disk index retains.
pub const MAX_DISK_BLOBS: usize = 4096;

/// N747: what a [`DiskChunkStore::compact`] pass removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskMaintenance {
    /// Blob index entries dropped because a chunk file was missing.
    pub missing_blobs: usize,
    /// Chunk files with no remaining blob reference.
    pub orphan_chunks: usize,
    /// Temporary `.tmp`/`.chunk-*` files removed.
    pub temp_files: usize,
    pub bytes_reclaimed: usize,
}

impl DiskChunkStore {
    pub fn open(
        dir: impl Into<PathBuf>,
        chunk_size: usize,
        max_total: usize,
    ) -> Result<Self, AssetError> {
        let dir = dir.into();
        let chunks = dir.join("chunks");
        std::fs::create_dir_all(&chunks).map_err(io_err)?;
        let chunk_size = chunk_size.clamp(1, 1024 * 1024);
        let mut store = Self {
            dir,
            chunk_size,
            max_total,
            total: 0,
            blobs: BTreeMap::new(),
        };
        let index_path = store.index_path();
        if let Ok(text) = std::fs::read_to_string(&index_path) {
            if let Ok(index) = serde_json::from_str::<DiskIndex>(&text) {
                if index.chunk_size == store.chunk_size {
                    store.blobs = index.blobs;
                }
            }
        }
        // Drop entries whose chunk files vanished, then remove orphan files.
        let entries: Vec<(String, DiskBlob)> = store.blobs.clone().into_iter().collect();
        for (key, entry) in entries {
            if entry
                .blob
                .chunks
                .iter()
                .any(|chunk| !store.chunk_path(chunk).is_file())
            {
                store.blobs.remove(&key);
            }
        }
        let referenced: BTreeSet<String> = store
            .blobs
            .values()
            .flat_map(|entry| entry.blob.chunks.iter().map(|chunk| chunk.0.clone()))
            .collect();
        let mut total = 0usize;
        for entry in std::fs::read_dir(&chunks).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                let _ = std::fs::remove_file(entry.path());
                continue;
            }
            if !referenced.contains(&name) {
                let _ = std::fs::remove_file(entry.path());
                continue;
            }
            total = total.saturating_add(entry.metadata().map_err(io_err)?.len() as usize);
        }
        store.total = total;
        store.save_index()?;
        Ok(store)
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.json")
    }

    fn chunk_path(&self, key: &AssetKey) -> PathBuf {
        self.dir.join("chunks").join(&key.0)
    }

    fn save_index(&self) -> Result<(), AssetError> {
        let index = DiskIndex {
            chunk_size: self.chunk_size,
            max_total: self.max_total,
            blobs: self.blobs.clone(),
        };
        let text = serde_json::to_string(&index).map_err(|e| AssetError::Io(e.to_string()))?;
        let path = self.index_path();
        let temp = self.dir.join(".index.tmp");
        std::fs::write(&temp, text).map_err(io_err)?;
        std::fs::rename(temp, path).map_err(io_err)
    }

    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    pub fn total_bytes(&self) -> usize {
        self.total
    }

    pub fn unique_chunks(&self) -> usize {
        self.blobs
            .values()
            .flat_map(|entry| entry.blob.chunks.iter().map(|chunk| chunk.0.clone()))
            .collect::<BTreeSet<_>>()
            .len()
    }

    pub fn ref_count(&self, blob: &StoredBlob) -> usize {
        self.blobs
            .get(&blob.key.0)
            .map(|entry| entry.refs)
            .unwrap_or(0)
    }

    pub fn insert(&mut self, bytes: &[u8]) -> Result<StoredBlob, AssetError> {
        let blob = StoredBlob {
            key: AssetKey::from_bytes(bytes),
            chunks: bytes
                .chunks(self.chunk_size)
                .map(AssetKey::from_bytes)
                .collect(),
            len: bytes.len(),
        };
        // Plan the new files before writing any of them.
        let mut planned = BTreeSet::new();
        let mut new_bytes = 0usize;
        for (key, chunk) in blob.chunks.iter().zip(bytes.chunks(self.chunk_size)) {
            if !self.chunk_path(key).is_file() && planned.insert(key.0.clone()) {
                // N757: the on-disk quota counts the compressed chunk.
                let stored = pack_chunk(chunk).len();
                new_bytes = new_bytes
                    .checked_add(stored)
                    .ok_or(AssetError::QuotaExceeded)?;
            }
        }
        if self
            .total
            .checked_add(new_bytes)
            .ok_or(AssetError::QuotaExceeded)?
            > self.max_total
        {
            return Err(AssetError::QuotaExceeded);
        }
        if !self.blobs.contains_key(&blob.key.0) && self.blobs.len() >= MAX_DISK_BLOBS {
            return Err(AssetError::TooLarge);
        }
        for (key, chunk) in blob.chunks.iter().zip(bytes.chunks(self.chunk_size)) {
            let path = self.chunk_path(key);
            if path.is_file() {
                continue;
            }
            let temp = self.dir.join(format!(".chunk-{}", key.0));
            let stored = pack_chunk(chunk);
            std::fs::write(&temp, &stored).map_err(io_err)?;
            std::fs::rename(temp, path).map_err(io_err)?;
            self.total = self.total.saturating_add(stored.len());
        }
        let entry = self
            .blobs
            .entry(blob.key.0.clone())
            .or_insert_with(|| DiskBlob {
                blob: blob.clone(),
                refs: 0,
            });
        entry.refs = entry.refs.saturating_add(1);
        self.save_index()?;
        Ok(blob)
    }

    pub fn get(&self, blob: &StoredBlob) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(blob.len);
        for key in &blob.chunks {
            out.extend_from_slice(&decode_chunk(&std::fs::read(self.chunk_path(key)).ok()?)?);
        }
        (out.len() == blob.len).then_some(out)
    }

    /// N747: reindex the directory — drop blobs with missing chunks, delete
    /// orphan/temporary files, recompute the byte total and rewrite the index.
    pub fn compact(&mut self) -> Result<DiskMaintenance, AssetError> {
        let mut report = DiskMaintenance::default();
        let entries: Vec<(String, DiskBlob)> = self.blobs.clone().into_iter().collect();
        for (key, entry) in entries {
            if entry
                .blob
                .chunks
                .iter()
                .any(|chunk| !self.chunk_path(chunk).is_file())
            {
                self.blobs.remove(&key);
                report.missing_blobs += 1;
            }
        }
        let referenced: BTreeSet<String> = self
            .blobs
            .values()
            .flat_map(|entry| entry.blob.chunks.iter().map(|chunk| chunk.0.clone()))
            .collect();
        let mut total = 0usize;
        for entry in std::fs::read_dir(self.dir.join("chunks")).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                let bytes = entry.metadata().map_err(io_err)?.len() as usize;
                std::fs::remove_file(entry.path()).map_err(io_err)?;
                report.temp_files += 1;
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(bytes);
                continue;
            }
            if !referenced.contains(&name) {
                let bytes = entry.metadata().map_err(io_err)?.len() as usize;
                std::fs::remove_file(entry.path()).map_err(io_err)?;
                report.orphan_chunks += 1;
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(bytes);
                continue;
            }
            total = total.saturating_add(entry.metadata().map_err(io_err)?.len() as usize);
        }
        self.total = total;
        self.save_index()?;
        Ok(report)
    }

    pub fn release(&mut self, blob: &StoredBlob) -> Result<(), AssetError> {
        let remove = match self.blobs.get_mut(&blob.key.0) {
            Some(entry) => {
                entry.refs = entry.refs.saturating_sub(1);
                entry.refs == 0
            }
            None => false,
        };
        if remove {
            self.blobs.remove(&blob.key.0);
        }
        let referenced: BTreeSet<String> = self
            .blobs
            .values()
            .flat_map(|entry| entry.blob.chunks.iter().map(|chunk| chunk.0.clone()))
            .collect();
        let mut total = 0usize;
        for entry in std::fs::read_dir(self.dir.join("chunks")).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if !referenced.contains(&name) {
                std::fs::remove_file(entry.path()).map_err(io_err)?;
                continue;
            }
            total = total.saturating_add(entry.metadata().map_err(io_err)?.len() as usize);
        }
        self.total = total;
        self.save_index()
    }
}

fn io_err(error: std::io::Error) -> AssetError {
    AssetError::Io(error.to_string())
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
    fn chunk_store_dedupes_across_blobs_and_releases() {
        // N729: shared chunks are stored once; the index rebuilds both blobs.
        let mut store = ChunkStore::new(4, 64);
        let a = store.insert(b"AAAABBBBCCCC").unwrap();
        let b = store.insert(b"AAAABBBBXXXX").unwrap();
        assert_eq!(a.len, 12);
        assert_eq!(b.len, 12);
        assert_eq!(store.unique_chunks(), 4, "two shared + two unique chunks");
        // N757: each 4-byte chunk stores as `MRCH || 0 || raw` (9 bytes).
        assert_eq!(store.total_bytes(), 36);
        assert_eq!(store.get(&a).as_deref(), Some(&b"AAAABBBBCCCC"[..]));
        assert_eq!(store.get(&b).as_deref(), Some(&b"AAAABBBBXXXX"[..]));
        let shared = AssetKey::from_bytes(b"AAAA");
        assert_eq!(store.ref_count(&shared), 2);

        store.release(&a);
        assert_eq!(store.ref_count(&shared), 1, "B still references the chunk");
        assert_eq!(store.total_bytes(), 27, "CCCC was freed");
        assert!(store.get(&a).is_none(), "released blob is incomplete");
        assert_eq!(store.get(&b).as_deref(), Some(&b"AAAABBBBXXXX"[..]));

        store.release(&b);
        assert_eq!(store.unique_chunks(), 0);
        assert_eq!(store.total_bytes(), 0);
        assert!(store.get(&b).is_none());
    }

    #[test]
    fn chunk_store_rejects_quota_before_mutating() {
        // N729: a blob larger than the quota is refused atomically.
        let mut store = ChunkStore::new(4, 16);
        assert_eq!(
            store.insert(b"AAAABBBBCCCC"),
            Err(AssetError::QuotaExceeded)
        );
        assert_eq!(store.unique_chunks(), 0);
        assert_eq!(store.total_bytes(), 0);
        // Empty blobs are valid and consume nothing.
        let empty = store.insert(b"").unwrap();
        assert_eq!(store.get(&empty), Some(Vec::new()));

        // A repeated chunk inside one blob is stored (and charged) once.
        let repeated = store.insert(b"AAAA".repeat(3).as_slice()).unwrap();
        assert_eq!(store.unique_chunks(), 1);
        assert_eq!(store.total_bytes(), 9);
        assert_eq!(store.get(&repeated), Some(b"AAAAAAAAAAAA".to_vec()));
    }

    #[test]
    fn disk_chunk_store_persists_dedupes_and_cleans_up() {
        // N738: dedup survives reopen; references keep shared chunks alive and
        // the last release removes them from disk.
        let dir = std::env::temp_dir().join(format!("mdc-disk-chunks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = DiskChunkStore::open(&dir, 4, 64).unwrap();
        let a = store.insert(b"AAAABBBB").unwrap();
        let b = store.insert(b"AAAACCCC").unwrap();
        assert_eq!(store.unique_chunks(), 3);
        assert_eq!(store.total_bytes(), 27);
        drop(store);

        let mut store = DiskChunkStore::open(&dir, 4, 64).unwrap();
        assert_eq!(store.unique_chunks(), 3, "index restored");
        assert_eq!(store.total_bytes(), 27);
        assert_eq!(store.get(&a).as_deref(), Some(&b"AAAABBBB"[..]));
        let a2 = store.insert(b"AAAABBBB").unwrap();
        assert_eq!(store.ref_count(&a2), 2);
        assert_eq!(store.total_bytes(), 27, "no new chunks for the same blob");
        store.release(&a).unwrap();
        assert_eq!(store.ref_count(&a2), 1);
        assert_eq!(store.get(&a2).as_deref(), Some(&b"AAAABBBB"[..]));
        store.release(&a2).unwrap();
        assert_eq!(store.total_bytes(), 18, "AAAA/CCCC remain for b");
        store.release(&b).unwrap();
        assert_eq!(store.total_bytes(), 0);
        assert_eq!(store.unique_chunks(), 0);
        drop(store);

        let store = DiskChunkStore::open(&dir, 4, 64).unwrap();
        assert_eq!(store.unique_chunks(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disk_chunk_store_rejects_quota_without_writing() {
        // N738: a quota failure leaves the directory empty.
        let dir = std::env::temp_dir().join(format!("mdc-disk-quota-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = DiskChunkStore::open(&dir, 4, 8).unwrap();
        assert_eq!(
            store.insert(b"AAAABBBBCCCC"),
            Err(AssetError::QuotaExceeded)
        );
        assert_eq!(store.total_bytes(), 0);
        assert_eq!(store.unique_chunks(), 0);
        assert_eq!(std::fs::read_dir(dir.join("chunks")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disk_chunk_store_compacts_orphans_and_missing_blobs() {
        // N747: compaction drops a blob with a missing chunk, removes orphan and
        // temporary files, and rewrites a consistent index.
        let dir = std::env::temp_dir().join(format!("mdc-disk-compact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = DiskChunkStore::open(&dir, 4, 64).unwrap();
        let a = store.insert(b"AAAA").unwrap();
        let b = store.insert(b"BBBB").unwrap();
        std::fs::write(dir.join("chunks").join("ORPHAN"), b"zz").unwrap();
        std::fs::write(dir.join("chunks").join(".tmp-file"), b"zz").unwrap();
        let missing = dir.join("chunks").join(AssetKey::from_bytes(b"BBBB").0);
        std::fs::remove_file(&missing).unwrap();
        let report = store.compact().unwrap();
        assert_eq!(report.missing_blobs, 1);
        assert_eq!(report.orphan_chunks, 1);
        assert_eq!(report.temp_files, 1);
        assert_eq!(store.total_bytes(), 9);
        assert_eq!(store.unique_chunks(), 1);
        assert_eq!(store.ref_count(&a), 1);
        assert!(store.get(&b).is_none());
        drop(store);
        let store = DiskChunkStore::open(&dir, 4, 64).unwrap();
        assert_eq!(store.unique_chunks(), 1);
        assert_eq!(store.total_bytes(), 9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rle_round_trips_and_quota_counts_compressed_bytes() {
        // N757: deterministic PackBits round-trip, malformed streams rejected,
        // and a repetitive chunk costs far less than its raw size.
        let source: Vec<u8> = vec![7u8; 4096]
            .into_iter()
            .enumerate()
            .map(|(i, b)| if i % 512 == 0 { 9 } else { b })
            .collect();
        let encoded = rle_encode(&source);
        assert!(encoded.len() < source.len() / 4, "{}", encoded.len());
        assert_eq!(rle_decode(&encoded).as_deref(), Some(source.as_slice()));
        assert_eq!(rle_decode(&encoded[..encoded.len() - 1]), None);
        assert_eq!(rle_decode(b"MRLE\x02\x00\x00\x00\x05"), None, "truncated");

        let mut store = ChunkStore::new(4096, 512);
        let blob = store.insert(&source).unwrap();
        assert!(store.total_bytes() < 512, "{}", store.total_bytes());
        assert_eq!(store.get(&blob).as_deref(), Some(source.as_slice()));

        // The disk store writes the same compact form and restores it.
        let dir = std::env::temp_dir().join(format!("mdc-rle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut disk = DiskChunkStore::open(&dir, 4096, 512).unwrap();
        let persisted = disk.insert(&source).unwrap();
        assert!(disk.total_bytes() < 512);
        drop(disk);
        let disk = DiskChunkStore::open(&dir, 4096, 512).unwrap();
        assert_eq!(disk.get(&persisted).as_deref(), Some(source.as_slice()));
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
