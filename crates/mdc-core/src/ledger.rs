//! Host-side allocation counter for simulator soaks. Outstanding bytes must
//! return to zero after the run; this does not measure process RSS.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllocationLedger {
    pub outstanding: usize,
    pub peak: usize,
    pub rejects: usize,
}

impl AllocationLedger {
    pub fn alloc(&mut self, bytes: usize) -> bool {
        let Some(next) = self.outstanding.checked_add(bytes) else {
            self.rejects = self.rejects.saturating_add(1);
            return false;
        };
        self.outstanding = next;
        self.peak = self.peak.max(self.outstanding);
        true
    }

    pub fn free(&mut self, bytes: usize) {
        self.outstanding = self.outstanding.saturating_sub(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_soak_returns_outstanding_to_zero() {
        let iters: usize = std::env::var("MDC_SOAK_ITERS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(64);
        let frame = 480 * 320 * 2;
        let mut ledger = AllocationLedger::default();
        for _ in 0..iters {
            assert!(ledger.alloc(frame));
            assert!(ledger.alloc(frame));
            ledger.free(frame);
            ledger.free(frame);
        }
        assert_eq!(ledger.outstanding, 0);
        assert_eq!(ledger.peak, frame * 2);
        let mut overflow = AllocationLedger::default();
        assert!(overflow.alloc(usize::MAX));
        assert!(!overflow.alloc(1));
        assert_eq!(overflow.rejects, 1);
        overflow.free(usize::MAX);
        assert_eq!(overflow.outstanding, 0);
    }
}
