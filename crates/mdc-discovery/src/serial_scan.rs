//! Serial port candidate discovery (AgentDeck port patterns, reimplemented).

use crate::{Candidate, DiscoveryProvider, Endpoint};
use mdc_transport::{is_candidate_serial_port, FOREIGN_DENYLIST_COOLDOWN_MS, FOREIGN_MAX_PROBE_FAILURES};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct DenyEntry {
    failures: u32,
    until: Instant,
}

/// Scans OS serial device nodes and yields anonymous Serial endpoints.
/// Identity is never inferred from the path — HELLO must confirm device_id.
#[derive(Debug, Default)]
pub struct SerialDiscovery {
    cancelled: bool,
    /// Injected paths for tests (skip filesystem when Some).
    injected: Option<Vec<String>>,
    denylist: BTreeMap<String, DenyEntry>,
    now: Option<Instant>,
}

impl SerialDiscovery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_injected_paths(paths: Vec<String>) -> Self {
        Self {
            injected: Some(paths),
            ..Self::default()
        }
    }

    fn clock(&self) -> Instant {
        self.now.unwrap_or_else(Instant::now)
    }

    #[cfg(test)]
    fn set_now(&mut self, now: Instant) {
        self.now = Some(now);
    }

    fn list_paths(&self) -> Vec<String> {
        if let Some(paths) = &self.injected {
            return paths.clone();
        }
        let mut out = Vec::new();
        for dir in ["/dev", "/dev/cu.usbmodem*", "/dev/cu.usbserial*"] {
            // Non-glob directories: read /dev entries.
            if dir.contains('*') {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(s) = path.to_str() else { continue };
                if is_candidate_serial_port(s) {
                    out.push(s.to_string());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Record a failed identification probe (foreign / silent device).
    pub fn note_identify_failure(&mut self, port: &str) {
        let now = self.clock();
        let entry = self.denylist.entry(port.into()).or_insert(DenyEntry {
            failures: 0,
            until: now,
        });
        entry.failures = entry.failures.saturating_add(1);
        if entry.failures >= FOREIGN_MAX_PROBE_FAILURES {
            entry.until = now + Duration::from_millis(FOREIGN_DENYLIST_COOLDOWN_MS);
        }
    }

    pub fn clear_denylist(&mut self, port: &str) {
        self.denylist.remove(port);
    }

    fn is_denied(&self, port: &str) -> bool {
        let Some(entry) = self.denylist.get(port) else {
            return false;
        };
        if entry.failures < FOREIGN_MAX_PROBE_FAILURES {
            return false;
        }
        self.clock() < entry.until
    }
}

impl DiscoveryProvider for SerialDiscovery {
    fn discover(&mut self) -> Vec<Candidate> {
        if self.cancelled {
            return Vec::new();
        }
        let mut out = Vec::new();
        for path in self.list_paths() {
            if !is_candidate_serial_port(&path) || self.is_denied(&path) {
                continue;
            }
            out.push(Candidate {
                device_id: None, // HELLO confirms identity
                endpoints: vec![Endpoint::Serial { port: path }],
            });
        }
        out
    }

    fn cancel(&mut self) {
        self.cancelled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn filters_candidates_and_denylists_foreign() {
        let mut scan = SerialDiscovery::with_injected_paths(vec![
            "/dev/cu.usbmodem1".into(),
            "/dev/cu.Bluetooth-Incoming-Port".into(),
            "/dev/ttyUSB0".into(),
        ]);
        let found = scan.discover();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|c| c.device_id.is_none()));

        let now = Instant::now();
        scan.set_now(now);
        for _ in 0..FOREIGN_MAX_PROBE_FAILURES {
            scan.note_identify_failure("/dev/cu.usbmodem1");
        }
        let found = scan.discover();
        assert_eq!(found.len(), 1);
        assert!(matches!(
            &found[0].endpoints[0],
            Endpoint::Serial { port } if port == "/dev/ttyUSB0"
        ));

        scan.set_now(now + Duration::from_millis(FOREIGN_DENYLIST_COOLDOWN_MS + 1));
        assert_eq!(scan.discover().len(), 2);
    }

    #[test]
    fn cancel_empties() {
        let mut scan = SerialDiscovery::with_injected_paths(vec!["/dev/ttyACM0".into()]);
        scan.cancel();
        assert!(scan.discover().is_empty());
    }
}
