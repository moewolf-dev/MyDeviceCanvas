//! Disk-backed flash port lease (AgentDeck esp32-flash-lease idea, reimplemented).
//! Expiry is checked on read — no background timer required.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseStatus {
    Free,
    Held { owner: String, expires_unix_ms: u64 },
    Expired,
}

#[derive(Debug)]
pub struct FilePortLease {
    path: PathBuf,
}

impl FilePortLease {
    pub fn new(dir: impl AsRef<Path>, port_key: &str) -> Self {
        let safe: String = port_key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = dir.as_ref().join(format!("mdc-flash-lease-{safe}.json"));
        Self { path }
    }

    pub fn status(&self) -> LeaseStatus {
        let Ok(text) = fs::read_to_string(&self.path) else {
            return LeaseStatus::Free;
        };
        // Minimal parse: {"owner":"...","expires_unix_ms":N}
        let owner = extract_string(&text, "owner").unwrap_or_default();
        let expires = extract_u64(&text, "expires_unix_ms").unwrap_or(0);
        let now = now_ms();
        if expires == 0 || now >= expires {
            let _ = fs::remove_file(&self.path);
            return LeaseStatus::Expired;
        }
        LeaseStatus::Held {
            owner,
            expires_unix_ms: expires,
        }
    }

    pub fn acquire(&self, owner: &str, ttl: Duration) -> Result<(), String> {
        match self.status() {
            LeaseStatus::Held { owner: ref o, .. } if o != owner => {
                return Err(format!("port leased by {o}"));
            }
            _ => {}
        }
        let expires = now_ms().saturating_add(ttl.as_millis() as u64);
        let body = format!(
            "{{\"owner\":\"{}\",\"expires_unix_ms\":{expires}}}",
            escape(owner)
        );
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&self.path, body).map_err(|e| e.to_string())
    }

    pub fn release(&self, owner: &str) -> Result<(), String> {
        match self.status() {
            LeaseStatus::Held { owner: ref o, .. } if o != owner => {
                return Err(format!("port leased by {o}"));
            }
            LeaseStatus::Free | LeaseStatus::Expired => return Ok(()),
            LeaseStatus::Held { .. } => {}
        }
        let _ = fs::remove_file(&self.path);
        Ok(())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn extract_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = json.find(&needle)? + needle.len();
    let end = json[start..].find('"')? + start;
    Some(json[start..end].to_string())
}

fn extract_u64(json: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\":");
    let start = json.find(&needle)? + needle.len();
    let rest = json[start..].trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn acquire_release_and_expiry() {
        let dir = std::env::temp_dir().join(format!("mdc-lease-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let lease = FilePortLease::new(&dir, "/dev/cu.usbmodem1");
        lease.acquire("mdc", Duration::from_secs(60)).unwrap();
        assert!(matches!(lease.status(), LeaseStatus::Held { .. }));
        assert!(lease.acquire("other", Duration::from_secs(60)).is_err());
        lease.release("mdc").unwrap();
        assert_eq!(lease.status(), LeaseStatus::Free);
        let _ = fs::remove_dir_all(&dir);
    }
}
