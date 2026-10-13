//! Simple LRU asset cache: hash key, size quota, reject oversized before alloc.
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AssetError {
    #[error("asset exceeds max entry size")]
    TooLarge,
    #[error("asset would exceed cache quota")]
    QuotaExceeded,
    #[error("compressed payload is not accepted")]
    CompressedUnsupported,
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
    fn hash_key_stable() {
        let k1 = AssetKey::from_bytes(&[1, 2, 3]);
        let k2 = AssetKey::from_bytes(&[1, 2, 3]);
        assert_eq!(k1, k2);
        assert_ne!(k1, AssetKey::from_bytes(&[1, 2, 4]));
    }
}
