mod serial_scan;
pub use serial_scan::SerialDiscovery;

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Endpoint {
    Serial { port: String },
    WebSocket { address: String },
    /// In-process / simulator endpoint label (not a network URL).
    Memory { label: String },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub device_id: Option<String>,
    pub endpoints: Vec<Endpoint>,
}
#[derive(Default)]
pub struct CandidateSet {
    by_id: BTreeMap<String, Candidate>,
    anonymous: Vec<Candidate>,
}
impl CandidateSet {
    pub fn insert(&mut self, candidate: Candidate) {
        if let Some(id) = candidate.device_id.clone() {
            self.by_id
                .entry(id)
                .and_modify(|old| {
                    for endpoint in &candidate.endpoints {
                        if !old.endpoints.contains(endpoint) {
                            old.endpoints.push(endpoint.clone());
                        }
                    }
                })
                .or_insert(candidate);
        } else if !candidate.endpoints.iter().any(|endpoint| {
            self.anonymous
                .iter()
                .any(|old| old.endpoints.contains(endpoint))
        }) {
            self.anonymous.push(candidate);
        }
    }
    pub fn into_vec(self) -> Vec<Candidate> {
        self.by_id.into_values().chain(self.anonymous).collect()
    }
}
pub trait DiscoveryProvider {
    fn discover(&mut self) -> Vec<Candidate>;
    fn cancel(&mut self);
}
#[derive(Default)]
pub struct StaticDiscovery {
    candidates: Vec<Candidate>,
    cancelled: bool,
}
impl StaticDiscovery {
    pub fn new(candidates: Vec<Candidate>) -> Self {
        Self {
            candidates,
            cancelled: false,
        }
    }
}
impl DiscoveryProvider for StaticDiscovery {
    fn discover(&mut self) -> Vec<Candidate> {
        if self.cancelled {
            return Vec::new();
        }
        let mut set = CandidateSet::default();
        for candidate in self.candidates.drain(..) {
            set.insert(candidate);
        }
        set.into_vec()
    }
    fn cancel(&mut self) {
        self.cancelled = true;
        self.candidates.clear();
    }
}

/// Deterministic mDNS stand-in for simulator / host tests (no network).
#[derive(Debug, Clone)]
pub struct MockMdnsProvider {
    pub service: String,
    seeded: Vec<Candidate>,
    cancelled: bool,
}
impl Default for MockMdnsProvider {
    fn default() -> Self {
        Self {
            service: "_mdc._tcp.local".into(),
            seeded: vec![Candidate {
                device_id: Some("sim-esp32-jc3248w535".into()),
                endpoints: vec![
                    Endpoint::Memory {
                        label: "fake-device".into(),
                    },
                    Endpoint::WebSocket {
                        address: "ws://127.0.0.1:9/mdc".into(),
                    },
                ],
            }],
            cancelled: false,
        }
    }
}
impl MockMdnsProvider {
    pub fn new(service: impl Into<String>, candidates: Vec<Candidate>) -> Self {
        Self {
            service: service.into(),
            seeded: candidates,
            cancelled: false,
        }
    }
    pub fn with_defaults() -> Self {
        Self::default()
    }
}
impl DiscoveryProvider for MockMdnsProvider {
    fn discover(&mut self) -> Vec<Candidate> {
        if self.cancelled {
            return Vec::new();
        }
        let mut set = CandidateSet::default();
        for candidate in self.seeded.clone() {
            set.insert(candidate);
        }
        set.into_vec()
    }
    fn cancel(&mut self) {
        self.cancelled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ws(a: &str) -> Endpoint {
        Endpoint::WebSocket { address: a.into() }
    }
    #[test]
    fn merges_same_identity() {
        let mut set = CandidateSet::default();
        set.insert(Candidate {
            device_id: Some("device".into()),
            endpoints: vec![ws("a")],
        });
        set.insert(Candidate {
            device_id: Some("device".into()),
            endpoints: vec![ws("b"), ws("a")],
        });
        let result = set.into_vec();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].endpoints.len(), 2);
    }
    #[test]
    fn cancellation_is_empty() {
        let mut provider = StaticDiscovery::new(vec![Candidate {
            device_id: None,
            endpoints: vec![ws("a")],
        }]);
        provider.cancel();
        assert!(provider.discover().is_empty());
    }
    #[test]
    fn mock_mdns_returns_seeded_candidates() {
        let mut mdns = MockMdnsProvider::with_defaults();
        let found = mdns.discover();
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].device_id.as_deref(),
            Some("sim-esp32-jc3248w535")
        );
        assert!(found[0]
            .endpoints
            .iter()
            .any(|e| matches!(e, Endpoint::Memory { .. })));
        mdns.cancel();
        assert!(mdns.discover().is_empty());
    }
}
