//! Host-side frame/tile pacing for bandwidth-limited devices (N305).
//!
//! The OBS / low-frame-rate report argues that a low-FPS device must *schedule*
//! frames. An 800x480 RGB565 frame is 768,000 bytes; at 60 FPS that is
//! 46,080,000 B/s (~46 MB/s), far beyond a serial link or a small MCU mirror.
//! This crate decides pacing only — how many tiles fit in the current tick and
//! which are deferred — and never reorders image content. `latest-frame-wins`
//! stays in the core queues; a scheduler that has to skip work reports it here.
//!
//! It depends on nothing (no protocol, no GUI) so every consumer shares one
//! pacing implementation.

/// Rolling window the byte budget is expressed over.
pub const WINDOW_MS: u64 = 1_000;

/// One tile the host wants to send this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileCost {
    pub index: u32,
    pub bytes: u32,
    /// A dirty-region tile (form B). Dirty tiles are packed first.
    pub dirty: bool,
}

impl TileCost {
    pub fn new(index: u32, bytes: u32, dirty: bool) -> Self {
        Self {
            index,
            bytes,
            dirty,
        }
    }
}

/// What the scheduler allows at the requested tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameDecision {
    /// Send now. `selected` are the tiles that fit the budget; `deferred`
    /// (dirty-first order preserved) must be retried on a later tick.
    Send {
        selected: Vec<u32>,
        deferred: Vec<u32>,
    },
    /// Too soon since the last emit; retry at `at_ms` (virtual time).
    Defer { at_ms: u64 },
}

impl FrameDecision {
    pub fn selected(&self) -> &[u32] {
        match self {
            Self::Send { selected, .. } => selected,
            Self::Defer { .. } => &[],
        }
    }
    pub fn is_defer(&self) -> bool {
        matches!(self, Self::Defer { .. })
    }
}

/// Pacing limits. `max_fps == 0` means "no fps limit"; a byte budget of 0 means
/// "unlimited bytes" (advisory caps that the device did not advertise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerConfig {
    pub max_fps: u16,
    pub max_bytes_per_second: u64,
}

impl SchedulerConfig {
    pub fn new(max_fps: u16, max_bytes_per_second: u64) -> Self {
        Self {
            max_fps,
            max_bytes_per_second,
        }
    }
    /// Build from the capability fields a device advertises (N03). A missing or
    /// zero fps falls back to a conservative floor rather than "as fast as
    /// possible", and a zero bandwidth stays unlimited (advisory).
    pub fn for_mirror(mirror_fps: u16, mirror_bandwidth: u64) -> Self {
        Self::new(mirror_fps, mirror_bandwidth)
    }
}

/// Deterministic scheduler driven by virtual time (milliseconds).
pub struct Scheduler {
    config: SchedulerConfig,
    last_emit_ms: Option<u64>,
}

impl Scheduler {
    pub fn new(max_fps: u16, max_bytes_per_second: u64) -> Self {
        Self {
            config: SchedulerConfig::new(max_fps, max_bytes_per_second),
            last_emit_ms: None,
        }
    }
    pub fn from_config(config: SchedulerConfig) -> Self {
        Self {
            config,
            last_emit_ms: None,
        }
    }
    /// Build a scheduler from the capability fields a device advertises (N03).
    pub fn for_mirror(mirror_fps: u16, mirror_bandwidth: u64) -> Self {
        Self::from_config(SchedulerConfig::for_mirror(mirror_fps, mirror_bandwidth))
    }
    pub fn config(&self) -> SchedulerConfig {
        self.config
    }
    pub fn last_emit_ms(&self) -> Option<u64> {
        self.last_emit_ms
    }

    /// Smallest interval between two emits for the configured fps (0 = no limit).
    pub fn min_interval_ms(&self) -> u64 {
        match self.config.max_fps {
            0 => 0,
            fps => (WINDOW_MS / u64::from(fps)).max(1),
        }
    }

    /// Bytes the host may spend in one interval. 0 means "unlimited".
    pub fn budget_bytes(&self) -> u64 {
        if self.config.max_bytes_per_second == 0 {
            return 0;
        }
        let interval = self.min_interval_ms().max(1);
        self.config.max_bytes_per_second.saturating_mul(interval) / WINDOW_MS
    }

    /// Decide which tiles fit at `now_ms`. Pure: the caller calls [`commit`]
    /// once the selected tiles are actually queued. Dirty tiles are packed
    /// first so a partial update makes visible progress before clean tiles.
    ///
    /// [`commit`]: Scheduler::commit
    pub fn plan(&self, now_ms: u64, tiles: &[TileCost]) -> FrameDecision {
        let interval = self.min_interval_ms();
        if interval > 0 {
            if let Some(last) = self.last_emit_ms {
                let ready_at = last.saturating_add(interval);
                if now_ms < ready_at {
                    return FrameDecision::Defer { at_ms: ready_at };
                }
            }
        }
        if tiles.is_empty() {
            return FrameDecision::Send {
                selected: Vec::new(),
                deferred: Vec::new(),
            };
        }
        let budget = self.budget_bytes();
        // Preserve the caller's order for the deferred list, but attempt dirty
        // tiles first.
        let mut order: Vec<usize> = (0..tiles.len()).collect();
        order.sort_by_key(|&i| (!tiles[i].dirty, i));
        let mut selected = Vec::new();
        let mut deferred = Vec::new();
        let mut spent: u64 = 0;
        for &i in &order {
            let cost = u64::from(tiles[i].bytes);
            let fits = budget == 0 || selected.is_empty() || spent.saturating_add(cost) <= budget;
            if fits {
                spent = spent.saturating_add(cost);
                selected.push(tiles[i].index);
            } else {
                deferred.push(tiles[i].index);
            }
        }
        FrameDecision::Send { selected, deferred }
    }

    /// Record that the frame planned at `now_ms` was emitted.
    pub fn commit(&mut self, now_ms: u64) {
        self.last_emit_ms = Some(now_ms);
    }
}

/// Merge contiguous tile indices into `[start, end]` ranges (inclusive). Dirty
/// blocks that touch become one transaction instead of many.
pub fn merge_dirty_ranges(tiles: &[TileCost]) -> Vec<(u32, u32)> {
    let mut dirty: Vec<u32> = tiles
        .iter()
        .filter(|tile| tile.dirty)
        .map(|tile| tile.index)
        .collect();
    dirty.sort_unstable();
    dirty.dedup();
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for index in dirty {
        match ranges.last_mut() {
            Some(last) if last.1.saturating_add(1) >= index => {
                last.1 = last.1.max(index);
            }
            _ => ranges.push((index, index)),
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_clamp_defers_until_the_interval_elapses() {
        let mut scheduler = Scheduler::new(8, 0); // 125 ms interval
        assert_eq!(scheduler.min_interval_ms(), 125);
        let tiles = [TileCost::new(0, 100, false), TileCost::new(1, 100, false)];
        let first = scheduler.plan(0, &tiles);
        assert_eq!(
            first,
            FrameDecision::Send {
                selected: vec![0, 1],
                deferred: vec![]
            }
        );
        scheduler.commit(0);
        assert_eq!(
            scheduler.plan(10, &tiles),
            FrameDecision::Defer { at_ms: 125 }
        );
        assert_eq!(
            scheduler.plan(125, &tiles),
            FrameDecision::Send {
                selected: vec![0, 1],
                deferred: vec![]
            }
        );
    }

    #[test]
    fn byte_budget_selects_only_what_fits() {
        // 200 bytes/s at 8 fps -> 25 bytes per 125 ms interval.
        let mut scheduler = Scheduler::new(8, 200);
        assert_eq!(scheduler.budget_bytes(), 25);
        let tiles: Vec<TileCost> = (0..5).map(|i| TileCost::new(i, 10, false)).collect();
        let decision = scheduler.plan(0, &tiles);
        // Two tiles (20 bytes) fit; the rest are deferred.
        assert_eq!(
            decision,
            FrameDecision::Send {
                selected: vec![0, 1],
                deferred: vec![2, 3, 4]
            }
        );
        scheduler.commit(0);
    }

    #[test]
    fn dirty_tiles_are_packed_first() {
        let scheduler = Scheduler::new(8, 200); // 25 bytes/interval
        let tiles = [
            TileCost::new(0, 10, false),
            TileCost::new(1, 10, true),
            TileCost::new(2, 10, true),
            TileCost::new(3, 10, false),
        ];
        // The dirty tiles (1, 2) are attempted before clean tile 0.
        let decision = scheduler.plan(0, &tiles);
        assert_eq!(decision.selected(), &[1, 2]);
        match decision {
            FrameDecision::Send { deferred, .. } => assert_eq!(deferred, vec![0, 3]),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_tile_larger_than_the_budget_still_makes_progress() {
        let scheduler = Scheduler::new(8, 40); // 5 bytes/interval, tile is 100
        let tiles = [TileCost::new(0, 100, true), TileCost::new(1, 100, true)];
        let decision = scheduler.plan(0, &tiles);
        // At least one tile is always selected so the frame cannot stall.
        assert_eq!(decision.selected(), &[0]);
    }

    #[test]
    fn zero_limits_mean_no_clamp() {
        let mut scheduler = Scheduler::new(0, 0);
        assert_eq!(scheduler.min_interval_ms(), 0);
        assert_eq!(scheduler.budget_bytes(), 0);
        let tiles: Vec<TileCost> = (0..40).map(|i| TileCost::new(i, 10_000, false)).collect();
        let decision = scheduler.plan(0, &tiles);
        assert_eq!(decision.selected().len(), 40);
        scheduler.commit(0);
        // No fps clamp: an immediate retry still sends.
        assert!(!scheduler.plan(0, &tiles).is_defer());
    }

    #[test]
    fn dirty_ranges_merge_contiguous_blocks() {
        let tiles = [
            TileCost::new(1, 1, true),
            TileCost::new(2, 1, true),
            TileCost::new(5, 1, true),
            TileCost::new(6, 1, false),
            TileCost::new(7, 1, true),
            TileCost::new(8, 1, true),
        ];
        assert_eq!(merge_dirty_ranges(&tiles), vec![(1, 2), (5, 5), (7, 8)]);
        assert!(merge_dirty_ranges(&[]).is_empty());
    }
}
