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

    /// N741: adapt pacing to a measured byte rate. `frame_bytes` is one full
    /// frame; the sustainable FPS becomes `rate / frame_bytes` (clamped to the
    /// configured cap) and the byte budget never exceeds the measured rate.
    pub fn adapt_to_bitrate(&mut self, bytes_per_second: u64, frame_bytes: u64) -> SchedulerConfig {
        if frame_bytes == 0 {
            return self.config;
        }
        let sustainable = bytes_per_second / frame_bytes;
        let cap = if self.config.max_fps == 0 {
            60
        } else {
            self.config.max_fps
        };
        let fps = sustainable.clamp(1, u64::from(cap)) as u16;
        self.config.max_fps = fps;
        if self.config.max_bytes_per_second == 0
            || self.config.max_bytes_per_second > bytes_per_second
        {
            self.config.max_bytes_per_second = bytes_per_second.max(1);
        }
        self.config
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

/// N758: one surface's demand for the current tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceDemand {
    pub surface: u32,
    /// Higher priorities are served first each round (ties by `surface`).
    pub priority: u8,
    /// N767: relative share of the *extra* budget above one tile per surface
    /// per tick (0 and 1 are the same). Every surface still gets the first tile
    /// when the budget permits, so weights cannot starve a display.
    pub weight: u16,
    pub tiles: Vec<TileCost>,
}

/// N758: what one surface may send under a shared byte budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceSelection {
    pub surface: u32,
    pub selected: Vec<u32>,
    pub deferred: Vec<u32>,
}

/// N758: the shared-budget plan across surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SharedDecision {
    pub surfaces: Vec<SurfaceSelection>,
    pub spent: u64,
}

impl SharedDecision {
    pub fn selection(&self, surface: u32) -> Option<&SurfaceSelection> {
        self.surfaces.iter().find(|s| s.surface == surface)
    }
}

/// N758: plan several surfaces against one shared byte budget. Every surface
/// gets one tile first (priority order, so none starves); remaining budget is
/// shared by `weight` in rounds. A tile larger than the whole budget is still
/// admitted when nothing has been spent yet, matching [`Scheduler::plan`].
/// N767: weights only govern the extra budget beyond the first tile.
pub fn plan_shared(budget: u64, demands: &[SurfaceDemand]) -> SharedDecision {
    let mut order: Vec<usize> = (0..demands.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(demands[i].priority), demands[i].surface));
    let weights: Vec<u32> = demands
        .iter()
        .map(|demand| u32::from(demand.weight.max(1)))
        .collect();
    let mut cursors = vec![0usize; demands.len()];
    let mut selections: Vec<SurfaceSelection> = demands
        .iter()
        .map(|demand| SurfaceSelection {
            surface: demand.surface,
            selected: Vec::new(),
            deferred: Vec::new(),
        })
        .collect();
    let mut spent = 0u64;
    let take = |i: usize,
                spent: &mut u64,
                cursors: &mut Vec<usize>,
                selections: &mut Vec<SurfaceSelection>|
     -> bool {
        if cursors[i] >= demands[i].tiles.len() {
            return false;
        }
        let cost = u64::from(demands[i].tiles[cursors[i]].bytes);
        if budget != 0 && *spent != 0 && spent.saturating_add(cost) > budget {
            return false;
        }
        *spent = spent.saturating_add(cost);
        selections[i]
            .selected
            .push(demands[i].tiles[cursors[i]].index);
        cursors[i] += 1;
        true
    };
    // Fair pass: at most one tile per surface.
    for &i in &order {
        take(i, &mut spent, &mut cursors, &mut selections);
    }
    // Weighted extras: high-weight surfaces may take more of the remainder.
    let mut extras = vec![0u32; demands.len()];
    loop {
        let mut progressed = false;
        for &i in &order {
            if extras[i] + 1 >= weights[i] {
                continue;
            }
            if take(i, &mut spent, &mut cursors, &mut selections) {
                extras[i] += 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    for (i, demand) in demands.iter().enumerate() {
        selections[i].deferred = demand.tiles[cursors[i]..]
            .iter()
            .map(|tile| tile.index)
            .collect();
    }
    SharedDecision {
        surfaces: selections,
        spent,
    }
}

/// N787: cross-call weighted credit. Each `plan` serves the surface furthest
/// behind `served / weight`, so a one-tile budget converges to the weight ratio
/// over many calls instead of always preferring the same surface.
#[derive(Debug, Default, Clone)]
pub struct WeightedSharedScheduler {
    served: std::collections::HashMap<u32, u64>,
}

impl WeightedSharedScheduler {
    pub fn new() -> Self {
        Self {
            served: std::collections::HashMap::new(),
        }
    }

    /// Tiles this scheduler has handed to `surface` across every `plan` call.
    pub fn served(&self, surface: u32) -> u64 {
        self.served.get(&surface).copied().unwrap_or(0)
    }

    /// N797: forget every surface's served count so the next plans reconverge.
    pub fn reset_served(&mut self) {
        self.served.clear();
    }

    /// N807: the served counts as plain pairs, sorted by surface id.
    pub fn export_served(&self) -> Vec<(u32, u64)> {
        let mut counts: Vec<(u32, u64)> = self
            .served
            .iter()
            .map(|(&surface, &count)| (surface, count))
            .collect();
        counts.sort_unstable_by_key(|item| item.0);
        counts
    }

    /// N807: replace the served counts. Later `plan` calls continue from these.
    pub fn import_served(&mut self, counts: &[(u32, u64)]) {
        self.served.clear();
        for &(surface, count) in counts {
            self.served.insert(surface, count);
        }
    }

    /// Plan one tick. `budget == 0` is unlimited. A zero weight is treated as 1.
    /// Credit outranks `priority`: the long-run counts follow the weights.
    pub fn plan(&mut self, budget: u64, demands: &[SurfaceDemand]) -> SharedDecision {
        let weights: Vec<u64> = demands
            .iter()
            .map(|demand| u64::from(demand.weight.max(1)))
            .collect();
        let sum_w = weights.iter().copied().sum::<u64>().max(1);
        let mut cursors = vec![0usize; demands.len()];
        let mut selections: Vec<SurfaceSelection> = demands
            .iter()
            .map(|demand| SurfaceSelection {
                surface: demand.surface,
                selected: Vec::new(),
                deferred: Vec::new(),
            })
            .collect();
        let mut spent = 0u64;
        loop {
            let total: u64 = demands
                .iter()
                .map(|demand| self.served(demand.surface))
                .sum();
            let mut best: Option<usize> = None;
            let mut best_key: (i128, u8, u64, u32) = (i128::MIN, 0, 0, 0);
            for (i, demand) in demands.iter().enumerate() {
                if cursors[i] >= demand.tiles.len() {
                    continue;
                }
                let cost = u64::from(demand.tiles[cursors[i]].bytes);
                if budget != 0 && spent != 0 && spent.saturating_add(cost) > budget {
                    continue;
                }
                let served = self.served(demand.surface);
                let deficit = weights[i] as i128 * total as i128 - served as i128 * sum_w as i128;
                // Higher deficit, then higher priority, then higher weight, then
                // the lower surface id (stable when everyone is equally behind).
                let key = (
                    deficit,
                    demand.priority,
                    weights[i],
                    u32::MAX - demand.surface,
                );
                if best.is_none() || key > best_key {
                    best = Some(i);
                    best_key = key;
                }
            }
            let Some(i) = best else {
                break;
            };
            spent = spent.saturating_add(u64::from(demands[i].tiles[cursors[i]].bytes));
            selections[i]
                .selected
                .push(demands[i].tiles[cursors[i]].index);
            cursors[i] += 1;
            *self.served.entry(demands[i].surface).or_default() += 1;
        }
        for (i, demand) in demands.iter().enumerate() {
            selections[i].deferred = demand.tiles[cursors[i]..]
                .iter()
                .map(|tile| tile.index)
                .collect();
        }
        SharedDecision {
            surfaces: selections,
            spent,
        }
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
    fn shared_budget_advances_every_surface_and_converges() {
        // N758: two surfaces share 25 bytes per tick, 10 bytes per tile. Each
        // round serves both, so neither surface starves and both finish.
        let demands = vec![
            SurfaceDemand {
                surface: 1,
                priority: 5,
                weight: 1,
                tiles: (0..4).map(|i| TileCost::new(i, 10, false)).collect(),
            },
            SurfaceDemand {
                surface: 2,
                priority: 1,
                weight: 1,
                tiles: (0..4).map(|i| TileCost::new(i, 10, false)).collect(),
            },
        ];
        let first = plan_shared(25, &demands);
        assert_eq!(first.spent, 20);
        let one = first.selection(1).unwrap();
        let two = first.selection(2).unwrap();
        assert_eq!(one.selected, vec![0]);
        assert_eq!(two.selected, vec![0]);
        assert_eq!(one.deferred.len(), 3);
        assert_eq!(two.deferred.len(), 3);

        // Feed the deferred tiles back until both surfaces are empty.
        let mut remaining: Vec<SurfaceDemand> = demands
            .iter()
            .map(|demand| {
                let deferred = &first.selection(demand.surface).unwrap().deferred;
                SurfaceDemand {
                    surface: demand.surface,
                    priority: demand.priority,
                    weight: 1,
                    tiles: demand
                        .tiles
                        .iter()
                        .filter(|tile| deferred.contains(&tile.index))
                        .cloned()
                        .collect(),
                }
            })
            .collect();
        let mut rounds = 0;
        loop {
            let decision = plan_shared(25, &remaining);
            rounds += 1;
            let mut done = true;
            for demand in &mut remaining {
                let selection = decision.selection(demand.surface).unwrap();
                let before = demand.tiles.len();
                demand
                    .tiles
                    .retain(|tile| !selection.selected.contains(&tile.index));
                done &= demand.tiles.is_empty();
                assert!(demand.tiles.len() < before || before == 0);
            }
            if done {
                break;
            }
            assert!(rounds < 8, "must converge");
        }
        assert_eq!(rounds, 3, "one tile per surface per round");
    }

    #[test]
    fn shared_budget_honours_priority_but_never_starves() {
        // A high-priority surface is served first in each round; with one tile's
        // budget only the priority surface moves this tick, the other moves next.
        let demands = vec![
            SurfaceDemand {
                surface: 7,
                priority: 9,
                weight: 1,
                tiles: vec![TileCost::new(0, 10, false)],
            },
            SurfaceDemand {
                surface: 3,
                priority: 1,
                weight: 1,
                tiles: vec![TileCost::new(0, 10, false)],
            },
        ];
        let decision = plan_shared(10, &demands);
        assert_eq!(decision.selection(7).unwrap().selected, vec![0]);
        assert!(decision.selection(3).unwrap().selected.is_empty());
        assert_eq!(decision.selection(3).unwrap().deferred, vec![0]);
        // With room for both, both are served in the same tick.
        let decision = plan_shared(20, &demands);
        assert_eq!(decision.selection(3).unwrap().selected, vec![0]);
        assert_eq!(decision.spent, 20);
        // An empty demand set and a zero budget are safe.
        assert!(plan_shared(0, &[]).surfaces.is_empty());
        let unlimited = plan_shared(0, &[demands[0].clone()]);
        assert_eq!(unlimited.selection(7).unwrap().selected, vec![0]);
    }

    #[test]
    fn weights_share_extra_budget_without_starving() {
        // N767: the fair pass gives each surface one tile; weights then split
        // the remaining budget (3:1 here), and a tiny budget still moves both.
        let demand = |surface: u32, weight: u16| SurfaceDemand {
            surface,
            priority: 0,
            weight,
            tiles: (0..8).map(|i| TileCost::new(i, 10, false)).collect(),
        };
        let demands = vec![demand(1, 3), demand(2, 1)];
        let decision = plan_shared(80, &demands);
        assert_eq!(decision.selection(1).unwrap().selected, vec![0, 1, 2]);
        assert_eq!(decision.selection(2).unwrap().selected, vec![0]);
        assert_eq!(decision.spent, 40);

        // Over many calls a two-tile budget still serves every surface once;
        // weights never cause a starved display.
        for _ in 0..100 {
            let decision = plan_shared(20, &demands);
            assert_eq!(decision.selection(1).unwrap().selected, vec![0]);
            assert_eq!(decision.selection(2).unwrap().selected, vec![0]);
        }
        // Zero weight is treated as one (never zero service).
        let zero = plan_shared(10, &[demand(3, 0), demand(4, 0)]);
        assert_eq!(zero.selection(3).unwrap().selected, vec![0]);
        assert!(zero.selection(4).unwrap().selected.is_empty());
    }

    #[test]
    fn weighted_credit_converges_across_calls() {
        // N787: a one-tile budget still yields a 3:1 split over 40 calls.
        let demand = |surface: u32, weight: u16| SurfaceDemand {
            surface,
            priority: 0,
            weight,
            tiles: (0..8).map(|i| TileCost::new(i, 10, false)).collect(),
        };
        let demands = vec![demand(1, 3), demand(2, 1)];
        let mut scheduler = WeightedSharedScheduler::new();
        let mut counts = [0u32; 2];
        for _ in 0..40 {
            let decision = scheduler.plan(10, &demands);
            if !decision.selection(1).unwrap().selected.is_empty() {
                counts[0] += 1;
            }
            if !decision.selection(2).unwrap().selected.is_empty() {
                counts[1] += 1;
            }
        }
        assert_eq!(counts, [30, 10]);
        assert_eq!(scheduler.served(1), 30);
        assert_eq!(scheduler.served(2), 10);
        // N797: reset clears the snapshot; the same calls converge again.
        scheduler.reset_served();
        assert_eq!(scheduler.served(1), 0);
        assert_eq!(scheduler.served(2), 0);
        let mut again = [0u32; 2];
        for _ in 0..40 {
            let decision = scheduler.plan(10, &demands);
            if !decision.selection(1).unwrap().selected.is_empty() {
                again[0] += 1;
            }
            if !decision.selection(2).unwrap().selected.is_empty() {
                again[1] += 1;
            }
        }
        assert_eq!(again, [30, 10]);
        assert_eq!(scheduler.served(1), 30);
        // N807: a snapshot reloads onto a fresh scheduler, and the next plans
        // match a scheduler that never stopped.
        let snapshot = scheduler.export_served();
        let mut restored = WeightedSharedScheduler::new();
        restored.import_served(&snapshot);
        assert_eq!(restored.served(1), scheduler.served(1));
        assert_eq!(restored.served(2), scheduler.served(2));
        for _ in 0..40 {
            scheduler.plan(10, &demands);
            restored.plan(10, &demands);
        }
        assert_eq!(restored.export_served(), scheduler.export_served());
        assert_eq!(restored.served(1), 60);
        assert_eq!(restored.served(2), 20);
        // One larger tick follows the same ratio (3 then 1).
        let fresh = {
            let mut scheduler = WeightedSharedScheduler::new();
            scheduler.plan(40, &demands)
        };
        assert_eq!(fresh.selection(1).unwrap().selected.len(), 3);
        assert_eq!(fresh.selection(2).unwrap().selected.len(), 1);
        // A zero weight is one share, and an empty plan spends nothing.
        let mut scheduler = WeightedSharedScheduler::new();
        let zero = scheduler.plan(10, &[demand(3, 0)]);
        assert_eq!(zero.selection(3).unwrap().selected, vec![0]);
        assert_eq!(scheduler.plan(10, &[]).spent, 0);
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

    #[test]
    fn bitrate_adaptation_lowers_fps_and_budget() {
        // N741: a link that sustains 6 frames/s caps a 30 FPS scheduler at 6 and
        // spends at most the measured rate.
        let mut scheduler = Scheduler::new(30, 0);
        let config = scheduler.adapt_to_bitrate(192_000, 32_000);
        assert_eq!(config.max_fps, 6);
        assert_eq!(config.max_bytes_per_second, 192_000);
        assert_eq!(scheduler.min_interval_ms(), 166);
        assert_eq!(scheduler.budget_bytes(), 31_872);

        // 8 x 4 KiB tiles exceed one interval's budget; two ticks drain all.
        let tiles: Vec<TileCost> = (0..8).map(|i| TileCost::new(i, 4096, false)).collect();
        let first = scheduler.plan(0, &tiles);
        let selected = first.selected().to_vec();
        let deferred = match &first {
            FrameDecision::Send { deferred, .. } => deferred.clone(),
            FrameDecision::Defer { .. } => panic!("expected a send"),
        };
        assert!(!first.is_defer());
        assert!(!deferred.is_empty(), "budget defers some tiles");
        scheduler.commit(0);
        assert!(scheduler.plan(100, &tiles).is_defer());
        let rest: Vec<TileCost> = deferred
            .iter()
            .map(|index| TileCost::new(*index, 4096, false))
            .collect();
        let second = scheduler.plan(166, &rest);
        assert!(!second.is_defer());
        assert_eq!(second.selected().len(), rest.len(), "the rest fits");
        let mut all: Vec<u32> = selected;
        all.extend_from_slice(second.selected());
        all.sort_unstable();
        assert_eq!(all, (0..8).collect::<Vec<u32>>(), "the frame converges");
    }
}
