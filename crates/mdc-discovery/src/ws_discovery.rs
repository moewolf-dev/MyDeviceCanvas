//! WebSocket / mDNS-style network candidate discovery (testable mocks).
//! Real mDNS browsing is not required for sim-verified acceptance; HELLO still
//! confirms identity for every candidate.

use crate::{Candidate, CandidateSet, DiscoveryProvider, Endpoint, SerialDiscovery};
use std::collections::BTreeSet;

/// Deterministic WS endpoint provider (stand-in for `_mdc._tcp` browse results).
#[derive(Debug, Clone, Default)]
pub struct WsDiscovery {
    addresses: Vec<String>,
    cancelled: bool,
}

impl WsDiscovery {
    pub fn new(addresses: Vec<String>) -> Self {
        Self {
            addresses,
            cancelled: false,
        }
    }

    /// Default lab / sim addresses used by CLI `--sim discover`.
    pub fn with_sim_defaults() -> Self {
        Self::new(vec![
            "ws://127.0.0.1:8765/mdc".into(),
            "ws://[::1]:8765/mdc".into(),
        ])
    }

    pub fn push(&mut self, address: impl Into<String>) {
        self.addresses.push(address.into());
    }
}

impl DiscoveryProvider for WsDiscovery {
    fn discover(&mut self) -> Vec<Candidate> {
        if self.cancelled {
            return Vec::new();
        }
        let mut set = CandidateSet::default();
        let mut seen = BTreeSet::new();
        for address in &self.addresses {
            if !seen.insert(address.clone()) {
                continue;
            }
            set.insert(Candidate {
                device_id: None,
                endpoints: vec![Endpoint::WebSocket {
                    address: address.clone(),
                }],
            });
        }
        set.into_vec()
    }

    fn cancel(&mut self) {
        self.cancelled = true;
    }
}

/// Fan-in serial + WS (+ optional seeded) providers; cancel stops all.
pub struct CombinedDiscovery {
    serial: SerialDiscovery,
    ws: WsDiscovery,
    extra: Vec<Box<dyn DiscoveryProvider + Send>>,
    cancelled: bool,
}

impl CombinedDiscovery {
    pub fn new(serial: SerialDiscovery, ws: WsDiscovery) -> Self {
        Self {
            serial,
            ws,
            extra: Vec::new(),
            cancelled: false,
        }
    }

    pub fn sim_defaults() -> Self {
        Self::new(SerialDiscovery::new(), WsDiscovery::with_sim_defaults())
    }

    pub fn push_provider(&mut self, provider: Box<dyn DiscoveryProvider + Send>) {
        self.extra.push(provider);
    }
}

impl DiscoveryProvider for CombinedDiscovery {
    fn discover(&mut self) -> Vec<Candidate> {
        if self.cancelled {
            return Vec::new();
        }
        let mut set = CandidateSet::default();
        for c in self.serial.discover() {
            set.insert(c);
        }
        for c in self.ws.discover() {
            set.insert(c);
        }
        for p in &mut self.extra {
            for c in p.discover() {
                set.insert(c);
            }
        }
        set.into_vec()
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.serial.cancel();
        self.ws.cancel();
        for p in &mut self.extra {
            p.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockMdnsProvider;

    #[test]
    fn ws_discovery_dedupes_and_stays_anonymous() {
        let mut ws = WsDiscovery::new(vec![
            "ws://127.0.0.1:1/mdc".into(),
            "ws://127.0.0.1:1/mdc".into(),
            "ws://127.0.0.1:2/mdc".into(),
        ]);
        let found = ws.discover();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|c| c.device_id.is_none()));
    }

    #[test]
    fn combined_merges_serial_ws_and_mdns() {
        let serial = SerialDiscovery::with_injected_paths(vec!["/dev/cu.usbmodem1".into()]);
        let ws = WsDiscovery::new(vec!["ws://127.0.0.1:9/mdc".into()]);
        let mut combined = CombinedDiscovery::new(serial, ws);
        combined.push_provider(Box::new(MockMdnsProvider::with_defaults()));
        let found = combined.discover();
        assert!(found.len() >= 3);
        combined.cancel();
        assert!(combined.discover().is_empty());
    }
}
