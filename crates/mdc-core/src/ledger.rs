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

/// Pixel buffer, transport copy, and SDK copy share one host limit.
pub struct HostCopyBudget {
    ledger: AllocationLedger,
    limit: usize,
}

impl HostCopyBudget {
    pub fn new(limit: usize) -> Self {
        Self {
            ledger: AllocationLedger::default(),
            limit,
        }
    }

    pub fn outstanding(&self) -> usize {
        self.ledger.outstanding
    }

    pub fn peak(&self) -> usize {
        self.ledger.peak
    }

    /// Hold three copies. A second full frame that would pass the limit is refused
    /// before another buffer is counted.
    pub fn hold_frame(&mut self, pixels: usize) -> Result<(), &'static str> {
        let Some(total) = pixels.checked_mul(3) else {
            self.ledger.rejects = self.ledger.rejects.saturating_add(1);
            return Err("frame copies overflow");
        };
        if self.ledger.outstanding.saturating_add(total) > self.limit {
            self.ledger.rejects = self.ledger.rejects.saturating_add(1);
            return Err("host copy budget exceeded");
        }
        if !self.ledger.alloc(total) {
            return Err("host copy budget exceeded");
        }
        Ok(())
    }

    pub fn release_frame(&mut self, pixels: usize) {
        let Some(total) = pixels.checked_mul(3) else {
            return;
        };
        self.ledger.free(total);
    }
}

/// Fake internal heap. A framebuffer that would drop free bytes under the reserve is refused.
pub struct SimHeap {
    free_bytes: usize,
    largest_block: usize,
    min_free: usize,
}

impl SimHeap {
    pub fn new(free_bytes: usize, largest_block: usize, min_free: usize) -> Self {
        Self {
            free_bytes,
            largest_block,
            min_free,
        }
    }

    pub fn free_bytes(&self) -> usize {
        self.free_bytes
    }

    pub fn alloc_framebuffer(&mut self, bytes: usize) -> Result<(), &'static str> {
        if bytes > self.largest_block {
            return Err("largest block is below the framebuffer");
        }
        if self.free_bytes.saturating_sub(bytes) < self.min_free {
            return Err("internal reserve would be exhausted");
        }
        self.free_bytes -= bytes;
        self.largest_block = self.largest_block.min(self.free_bytes);
        Ok(())
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

    #[test]
    fn copy_budget_counts_three_buffers_and_returns_to_zero() {
        let pixels = 480 * 320 * 2;
        let mut budget = HostCopyBudget::new(1024 * 1024);
        budget.hold_frame(pixels).unwrap();
        assert_eq!(budget.outstanding(), pixels * 3);
        assert!(budget.hold_frame(pixels).is_err());
        assert_eq!(budget.outstanding(), pixels * 3);
        budget.release_frame(pixels);
        assert_eq!(budget.outstanding(), 0);
    }

    #[test]
    fn heap_reserve_refuses_a_second_framebuffer() {
        let frame = 480 * 320 * 2;
        let reserve = 32 * 1024;
        let mut heap = SimHeap::new(frame + reserve, frame, reserve);
        heap.alloc_framebuffer(frame).unwrap();
        assert_eq!(heap.free_bytes(), reserve);
        assert!(heap.alloc_framebuffer(frame).is_err());
        assert_eq!(heap.free_bytes(), reserve);
    }
}
