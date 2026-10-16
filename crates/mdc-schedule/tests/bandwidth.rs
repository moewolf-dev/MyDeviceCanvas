//! N306: a bandwidth-constrained link must defer tiles and still converge.
use mdc_schedule::{FrameDecision, Scheduler, TileCost};

const WIDTH: u32 = 800;
const HEIGHT: u32 = 480;
const TILE: u32 = 32;

#[test]
fn the_arithmetic_that_requires_a_scheduler() {
    // The reason report 2 insists on a scheduler: a full RGB565 frame at 60 FPS.
    let full_frame_bytes = WIDTH * HEIGHT * 2;
    assert_eq!(full_frame_bytes, 768_000);
    assert_eq!(full_frame_bytes * 60, 46_080_000); // ~46 MB/s
}

#[test]
fn low_bandwidth_defers_tiles_but_the_frame_converges() {
    let cols = WIDTH.div_ceil(TILE);
    let rows = HEIGHT.div_ceil(TILE);
    let tile_bytes = TILE * TILE * 2; // 2048
    let total_tiles = cols * rows;
    assert!(total_tiles > 300);

    // 8 FPS, 200 KB/s: a slow Wi-Fi / serial mirror, not the 46 MB/s upper bound.
    let mut scheduler = Scheduler::new(8, 200_000);
    let interval = scheduler.min_interval_ms();
    let budget = scheduler.budget_bytes() as usize;
    assert_eq!(interval, 125);
    assert!(budget >= tile_bytes as usize && budget < (WIDTH * HEIGHT * 2) as usize);

    let mut pending: Vec<u32> = (0..total_tiles).collect();
    let mut deferred_ticks = 0usize;
    let mut deferred_tiles = 0usize;
    let mut tick = 0u64;
    while !pending.is_empty() && tick < 10_000 {
        let tiles: Vec<TileCost> = pending
            .iter()
            .map(|&index| TileCost::new(index, tile_bytes, false))
            .collect();
        let now = tick * interval;
        match scheduler.plan(now, &tiles) {
            FrameDecision::Send { selected, deferred } => {
                assert!(!selected.is_empty(), "at least one tile must make progress");
                if !deferred.is_empty() {
                    deferred_ticks += 1;
                    deferred_tiles += deferred.len();
                }
                pending.retain(|index| !selected.contains(index));
                scheduler.commit(now);
            }
            FrameDecision::Defer { .. } => {}
        }
        tick += 1;
    }

    assert!(pending.is_empty(), "every tile must eventually be sent");
    assert!(
        deferred_ticks > 0 && deferred_tiles > 0,
        "a constrained link must defer work rather than send it all at once"
    );
    // The link is slower than one frame per interval, so it takes many intervals.
    assert!(tick > 1);
}

#[test]
fn merged_dirty_ranges_reduce_transactions() {
    // Three touching dirty tiles collapse into one range; a clean gap splits.
    let tiles = [
        TileCost::new(10, 4, true),
        TileCost::new(11, 4, true),
        TileCost::new(12, 4, true),
        TileCost::new(13, 4, false),
        TileCost::new(14, 4, true),
    ];
    assert_eq!(
        mdc_schedule::merge_dirty_ranges(&tiles),
        vec![(10, 12), (14, 14)]
    );
}
