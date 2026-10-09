//! Pairing credentials for network endpoints (H01).
//! Serial / Memory links do not require pairing; WebSocket (and future TCP labels)
//! require a stored credential before display / control / upgrade.

use crate::Endpoint;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingCredential {
    pub device_id: String,
    /// Opaque host-held secret; never log Wi-Fi passwords here.
    pub token: String,
    pub created_unix_ms: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PairingError {
    #[error("device is not paired")]
    NotPaired,
    #[error("pairing token mismatch")]
    TokenMismatch,
    #[error("empty device id or token")]
    Invalid,
    #[error("store I/O: {0}")]
    Io(String),
}

#[derive(Debug, Default, Clone)]
pub struct PairingStore {
    by_id: BTreeMap<String, PairingCredential>,
}

impl PairingStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pair(&mut self, device_id: impl Into<String>, token: impl Into<String>) -> Result<(), PairingError> {
        let device_id = device_id.into();
        let token = token.into();
        if device_id.is_empty() || token.is_empty() {
            return Err(PairingError::Invalid);
        }
        self.by_id.insert(
            device_id.clone(),
            PairingCredential {
                device_id,
                token,
                created_unix_ms: now_ms(),
            },
        );
        Ok(())
    }

    pub fn revoke(&mut self, device_id: &str) -> bool {
        self.by_id.remove(device_id).is_some()
    }

    pub fn is_paired(&self, device_id: &str) -> bool {
        self.by_id.contains_key(device_id)
    }

    pub fn verify(&self, device_id: &str, token: &str) -> Result<(), PairingError> {
        let cred = self.by_id.get(device_id).ok_or(PairingError::NotPaired)?;
        if cred.token != token {
            return Err(PairingError::TokenMismatch);
        }
        Ok(())
    }

    /// Network endpoints require pairing; USB serial and in-process memory do not.
    pub fn endpoint_requires_pairing(endpoint: &Endpoint) -> bool {
        matches!(endpoint, Endpoint::WebSocket { .. })
    }

    /// Gate display/control/upgrade for a known device on an endpoint.
    pub fn authorize(&self, device_id: &str, endpoint: &Endpoint) -> Result<(), PairingError> {
        if !Self::endpoint_requires_pairing(endpoint) {
            return Ok(());
        }
        if self.is_paired(device_id) {
            Ok(())
        } else {
            Err(PairingError::NotPaired)
        }
    }

    pub fn list(&self) -> Vec<&PairingCredential> {
        self.by_id.values().collect()
    }

    pub fn save_json(&self, path: &Path) -> Result<(), PairingError> {
        let list: Vec<&PairingCredential> = self.by_id.values().collect();
        let text = serde_json::to_string_pretty(&list).map_err(|e| PairingError::Io(e.to_string()))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PairingError::Io(e.to_string()))?;
        }
        std::fs::write(path, text).map_err(|e| PairingError::Io(e.to_string()))
    }

    pub fn load_json(path: &Path) -> Result<Self, PairingError> {
        let text = std::fs::read_to_string(path).map_err(|e| PairingError::Io(e.to_string()))?;
        let list: Vec<PairingCredential> =
            serde_json::from_str(&text).map_err(|e| PairingError::Io(e.to_string()))?;
        let mut store = Self::new();
        for cred in list {
            store.by_id.insert(cred.device_id.clone(), cred);
        }
        Ok(store)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_requires_pair_serial_does_not() {
        let mut store = PairingStore::new();
        let ws = Endpoint::WebSocket {
            address: "ws://1.2.3.4/mdc".into(),
        };
        let serial = Endpoint::Serial {
            port: "/dev/cu.usbmodem1".into(),
        };
        assert_eq!(
            store.authorize("dev", &ws),
            Err(PairingError::NotPaired)
        );
        assert!(store.authorize("dev", &serial).is_ok());
        store.pair("dev", "secret").unwrap();
        assert!(store.authorize("dev", &ws).is_ok());
        assert!(store.verify("dev", "secret").is_ok());
        assert_eq!(
            store.verify("dev", "wrong"),
            Err(PairingError::TokenMismatch)
        );
        assert!(store.revoke("dev"));
        assert_eq!(
            store.authorize("dev", &ws),
            Err(PairingError::NotPaired)
        );
    }

    #[test]
    fn json_round_trip() {
        let mut store = PairingStore::new();
        store.pair("a", "t1").unwrap();
        let path = std::env::temp_dir().join(format!("mdc-pair-{}.json", std::process::id()));
        store.save_json(&path).unwrap();
        let loaded = PairingStore::load_json(&path).unwrap();
        assert!(loaded.is_paired("a"));
        let _ = std::fs::remove_file(path);
    }
}
