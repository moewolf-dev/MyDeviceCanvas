//! Frame/tile staging, assemblers and per-surface queues.
//!
//! Owns bounded transactions and coverage tracking; no transport or session
//! state machine lives here.
use super::*;

#[derive(Debug, Clone)]
pub struct Frame {
    pub surface_id: String,
    pub width: u16,
    pub height: u16,
    pub bytes: Vec<u8>,
}
/// N516: a dirty rectangle in surface pixels (x, y, width, height).
pub type DirtyRect = (u16, u16, u16, u16);

/// N516: snap dirty rectangles to a tile grid and merge them into the minimal
/// set of tiles covering the same area (duplicates and overlaps collapse). This
/// is the dirty-region counterpart of [`plan_tiles`], which covers a whole
/// surface.
pub fn plan_dirty_tiles(
    surface: (u16, u16),
    tile_size: u16,
    rects: &[DirtyRect],
) -> Vec<DirtyRect> {
    let tile = tile_size.max(1);
    let cols = surface.0.div_ceil(tile).max(1);
    let rows = surface.1.div_ceil(tile).max(1);
    let mut marked = std::collections::BTreeSet::new();
    for &(x, y, w, h) in rects {
        if w == 0 || h == 0 {
            continue;
        }
        let x1 = x.saturating_add(w - 1);
        let y1 = y.saturating_add(h - 1);
        let tx0 = x / tile;
        let ty0 = y / tile;
        let tx1 = (x1 / tile).min(cols.saturating_sub(1));
        let ty1 = (y1 / tile).min(rows.saturating_sub(1));
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                marked.insert((tx, ty));
            }
        }
    }
    marked
        .into_iter()
        .map(|(tx, ty)| {
            let x = tx * tile;
            let y = ty * tile;
            (
                x,
                y,
                tile.min(surface.0.saturating_sub(x)),
                tile.min(surface.1.saturating_sub(y)),
            )
        })
        .collect()
}

/// N516: copy one tile out of a tightly packed RGB565 frame.
pub fn extract_tile(frame: &[u8], frame_size: (u16, u16), tile: DirtyRect) -> Vec<u8> {
    let (x, y, w, h) = tile;
    let stride = usize::from(frame_size.0) * 2;
    let mut out = vec![0u8; usize::from(w) * usize::from(h) * 2];
    for row in 0..usize::from(h) {
        let src = (usize::from(y) + row) * stride + usize::from(x) * 2;
        let dst = row * usize::from(w) * 2;
        let len = usize::from(w) * 2;
        if let (Some(from), Some(to)) = (frame.get(src..src + len), out.get_mut(dst..dst + len)) {
            to.copy_from_slice(from);
        }
    }
    out
}

/// N536: the host's partial-update counters reconciled against the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileReconciliation {
    /// Tiles the host wrote to the wire.
    pub sent: u64,
    /// Tiles the device reports having applied.
    pub received: u64,
    /// Bytes the host covered with those tiles.
    pub bytes_sent: u64,
    /// True when the device acknowledged every tile the host sent.
    pub consistent: bool,
}

impl TileReconciliation {
    /// Tiles the device did not acknowledge (0 when consistent).
    pub fn missing(&self) -> u64 {
        self.sent.saturating_sub(self.received)
    }

    /// N557: whether the device has fallen behind by more than `max_missing`
    /// tiles, so the next update should resynchronise with a full frame.
    pub fn needs_full_frame(&self, max_missing: u64) -> bool {
        self.missing() > max_missing
    }
}

/// N536: compare the host's tile counters with the device's reported count, so a
/// partial update can be checked end to end instead of only at the wire.
pub fn reconcile_tiles(stats: &SessionStats, device_tiles_received: u64) -> TileReconciliation {
    TileReconciliation {
        sent: stats.tiles_sent,
        received: device_tiles_received,
        bytes_sent: stats.tile_bytes_sent,
        consistent: stats.tiles_sent == device_tiles_received,
    }
}

/// N526: the outcome of a dirty-region update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDecision {
    /// A whole frame was sent (the accumulated area was not worth tiling).
    Full,
    /// Partial tiles were sent (`usize` = tile count; 0 = nothing was dirty).
    Partial(usize),
}

/// N526: accumulate dirty rectangles across frames and decide when a full frame
/// is cheaper than a partial update.
#[derive(Debug, Clone)]
pub struct DirtyTracker {
    pub(crate) surface: (u16, u16),
    pub(crate) tile_size: u16,
    /// Tile coverage above this fraction switches to a full frame (0..=1).
    full_threshold: f64,
    pending: Vec<DirtyRect>,
    updates: u64,
}

/// N541: the most dirty rectangles a tracker holds before coalescing them into
/// one bounding rectangle, so a caller that forgets to `take` cannot grow it
/// without bound.
pub const MAX_PENDING_RECTS: usize = 256;

impl DirtyTracker {
    pub fn new(surface: (u16, u16), tile_size: u16, full_threshold: f64) -> Self {
        Self {
            surface,
            tile_size: tile_size.max(1),
            full_threshold: full_threshold.clamp(0.0, 1.0),
            pending: Vec::new(),
            updates: 0,
        }
    }

    /// Accumulate one dirty rectangle. N541: past [`MAX_PENDING_RECTS`] the
    /// pending list is coalesced into one bounding rectangle so it stays bounded.
    pub fn mark(&mut self, rect: DirtyRect) {
        if rect.2 == 0 || rect.3 == 0 {
            return;
        }
        if self.pending.len() >= MAX_PENDING_RECTS {
            self.coalesce();
        }
        self.pending.push(rect);
    }

    /// N541: replace the pending list with its bounding rectangle.
    pub fn coalesce(&mut self) {
        let Some(first) = self.pending.first().copied() else {
            return;
        };
        let mut x0 = first.0;
        let mut y0 = first.1;
        let mut x1 = first.0.saturating_add(first.2);
        let mut y1 = first.1.saturating_add(first.3);
        for &(x, y, w, h) in &self.pending {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x.saturating_add(w));
            y1 = y1.max(y.saturating_add(h));
        }
        self.pending.clear();
        self.pending
            .push((x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)));
    }

    /// Mark the whole surface dirty.
    pub fn mark_all(&mut self) {
        self.mark((0, 0, self.surface.0, self.surface.1));
    }

    pub fn pending(&self) -> &[DirtyRect] {
        &self.pending
    }

    /// N732: the current full-frame threshold.
    pub fn full_threshold(&self) -> f64 {
        self.full_threshold
    }

    /// N732: replace the full-frame threshold (clamped to 0..=1).
    pub fn set_full_threshold(&mut self, threshold: f64) {
        self.full_threshold = threshold.clamp(0.0, 1.0);
    }

    /// N732: adapt the threshold to a measured byte rate. A device that cannot
    /// sustain even one full frame per second switches to a full frame for any
    /// meaningful dirty area (low threshold); a fast link tolerates mostly-dirty
    /// frames as tiles (high threshold). Returns the new threshold.
    pub fn adapt_to_bandwidth(&mut self, bytes_per_second: u64, frame_bytes: u64) -> f64 {
        if frame_bytes == 0 {
            return self.full_threshold;
        }
        let fps = bytes_per_second as f64 / frame_bytes as f64;
        let threshold = if fps <= 1.0 {
            0.05
        } else if fps >= 8.0 {
            0.9
        } else {
            0.05 + (fps - 1.0) / 7.0 * 0.85
        };
        self.full_threshold = threshold.clamp(0.05, 0.9);
        self.full_threshold
    }

    /// The fraction of the surface's tiles the pending rectangles cover.
    pub fn coverage(&self) -> f64 {
        let total_tiles = u64::from(self.surface.0.div_ceil(self.tile_size).max(1))
            * u64::from(self.surface.1.div_ceil(self.tile_size).max(1));
        let covered = plan_dirty_tiles(self.surface, self.tile_size, &self.pending).len() as u64;
        if total_tiles == 0 {
            return 0.0;
        }
        covered as f64 / total_tiles as f64
    }

    /// Completed updates.
    pub fn updates(&self) -> u64 {
        self.updates
    }

    /// Decide what to send for the accumulated rectangles without clearing them.
    pub fn decide(&self) -> UpdateDecision {
        if self.pending.is_empty() {
            return UpdateDecision::Partial(0);
        }
        let tiles = plan_dirty_tiles(self.surface, self.tile_size, &self.pending);
        if self.coverage() > self.full_threshold {
            UpdateDecision::Full
        } else {
            UpdateDecision::Partial(tiles.len())
        }
    }

    /// Decide and clear the accumulated rectangles.
    pub fn take(&mut self) -> UpdateDecision {
        let decision = self.decide();
        if decision != UpdateDecision::Partial(0) {
            self.updates = self.updates.saturating_add(1);
        }
        self.pending.clear();
        decision
    }

    /// The tiles for the pending rectangles (empty when a full frame is chosen).
    pub fn tiles(&self) -> Vec<DirtyRect> {
        match self.decide() {
            UpdateDecision::Full | UpdateDecision::Partial(0) => Vec::new(),
            UpdateDecision::Partial(_) => {
                plan_dirty_tiles(self.surface, self.tile_size, &self.pending)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tile {
    pub surface_id: String,
    pub base_frame_id: u64,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub bytes: Vec<u8>,
}
/// One complete device -> host mirrored frame (Form B / N03).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorFrame {
    pub surface_id: String,
    pub frame_id: u64,
    pub width: u16,
    pub height: u16,
    pub bytes: Vec<u8>,
}
/// N24: bounded mirror counters for a host / inspector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MirrorStats {
    pub frames_received: u64,
    pub frames_dropped: u64,
    pub last_frame_id: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionAck {
    Received,
    Displayed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionNack {
    InvalidRange,
    ConflictingDuplicate,
    Incomplete,
    BaseFrameMismatch,
    Expired,
    /// SIM-R07: the chunk would create more disjoint ranges than allowed.
    TooManyRanges,
}

#[derive(Debug)]
pub struct FrameAssembler {
    pub frame_id: u64,
    pub expected_len: usize,
    staging: Vec<u8>,
    ranges: Vec<(usize, usize)>,
    max_chunk: usize,
}
impl FrameAssembler {
    pub fn begin(frame_id: u64, expected_len: usize, max_chunk: usize) -> Result<Self, CoreError> {
        if expected_len == 0
            || expected_len > MAX_FRAME_BYTES
            || max_chunk == 0
            || max_chunk > mdc_protocol::DEFAULT_MAX_CHUNK
        {
            return Err(CoreError::TooLarge);
        }
        Ok(Self {
            frame_id,
            expected_len,
            staging: vec![0; expected_len],
            ranges: Vec::new(),
            max_chunk,
        })
    }
    pub fn chunk(&mut self, offset: usize, bytes: &[u8]) -> Result<(), TransactionNack> {
        let end = offset
            .checked_add(bytes.len())
            .ok_or(TransactionNack::InvalidRange)?;
        if bytes.is_empty() || bytes.len() > self.max_chunk || end > self.expected_len {
            return Err(TransactionNack::InvalidRange);
        }
        for &(start, finish) in &self.ranges {
            if offset < finish && end > start {
                if offset >= start && end <= finish && self.staging[offset..end] == *bytes {
                    return Ok(());
                }
                return Err(TransactionNack::ConflictingDuplicate);
            }
        }
        if self.ranges.len() >= MAX_FRAME_RANGES {
            return Err(TransactionNack::TooManyRanges);
        }
        self.staging[offset..end].copy_from_slice(bytes);
        self.ranges.push((offset, end));
        self.ranges.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(self.ranges.len());
        for (start, finish) in self.ranges.drain(..) {
            if let Some(last) = merged.last_mut() {
                if start <= last.1 {
                    last.1 = last.1.max(finish);
                    continue;
                }
            }
            merged.push((start, finish));
        }
        self.ranges = merged;
        Ok(())
    }
    pub fn commit(self) -> Result<Vec<u8>, TransactionNack> {
        if self.ranges.len() == 1 && self.ranges[0] == (0, self.expected_len) {
            Ok(self.staging)
        } else {
            Err(TransactionNack::Incomplete)
        }
    }
    /// N264: the received byte ranges (for a dirty-region patch).
    pub fn ranges(&self) -> &[(usize, usize)] {
        &self.ranges
    }
    /// N264: the received ranges and the staging buffer without requiring full
    /// coverage. Returns the bitmap of received ranges plus the staging bytes.
    pub fn into_ranges(self) -> (Vec<(usize, usize)>, Vec<u8>) {
        (self.ranges, self.staging)
    }
}
pub fn validate_tile_base(tile: &Tile, current_frame_id: u64) -> Result<(), TransactionNack> {
    if tile.base_frame_id == current_frame_id {
        Ok(())
    } else {
        Err(TransactionNack::BaseFrameMismatch)
    }
}

/// N308: tile edge the host uses when a device does not announce a preference.
pub const DEFAULT_TILE_SIZE: u16 = 32;

/// N777: the most surface weights a device config may carry.
pub const MAX_SURFACE_WEIGHTS: usize = 4;
/// N777: the largest relative weight accepted from a device config.
pub const MAX_SURFACE_WEIGHT: u16 = 16;

/// N777: parse a `surface_weights` config value (`"3,1"`). Empty, zero, more
/// than four entries, non-numeric or out-of-range values are rejected so the
/// host keeps its previous weights instead of silently re-balancing.
pub fn parse_surface_weights(value: &str) -> Option<Vec<u16>> {
    let weights: Vec<u16> = value
        .split(',')
        .map(str::trim)
        .map(str::parse::<u16>)
        .collect::<Result<_, _>>()
        .ok()?;
    if weights.is_empty()
        || weights.len() > MAX_SURFACE_WEIGHTS
        || weights.iter().any(|w| *w == 0 || *w > MAX_SURFACE_WEIGHT)
    {
        return None;
    }
    Some(weights)
}

/// N833: maximum number of clips in one `animation_blend` value.
pub const MAX_ANIMATION_BLEND_CLIPS: usize = 4;
/// N833: maximum weight for one blended clip (1000 = authored scale).
pub const MAX_ANIMATION_WEIGHT: u16 = 1000;
/// N833: maximum clip-name length in an `animation_blend` value.
pub const MAX_ANIMATION_NAME: usize = 32;

/// N833: parse `"clip:weight,clip:weight"` into bounded weights. `off` and an
/// empty string mean "no blend" and return an empty list. Names are exact and
/// unique; weights are `1..=1000`; at most [`MAX_ANIMATION_BLEND_CLIPS`].
pub fn parse_animation_blend(value: &str) -> Option<Vec<(String, u16)>> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("off") {
        return Some(Vec::new());
    }
    let mut entries: Vec<(String, u16)> = Vec::new();
    for token in value.split(',') {
        let (name, weight) = token.trim().split_once(':')?;
        let name = name.trim();
        if name.is_empty() || name.len() > MAX_ANIMATION_NAME || name.eq_ignore_ascii_case("off") {
            return None;
        }
        let weight: u16 = weight.trim().parse().ok()?;
        if weight == 0 || weight > MAX_ANIMATION_WEIGHT {
            return None;
        }
        if entries.iter().any(|(existing, _)| existing == name) {
            return None;
        }
        entries.push((name.to_string(), weight));
        if entries.len() > MAX_ANIMATION_BLEND_CLIPS {
            return None;
        }
    }
    (!entries.is_empty()).then_some(entries)
}

/// N833: format a blend list back to the canonical `clip:weight` string.
pub fn format_animation_blend(entries: &[(String, u16)]) -> String {
    if entries.is_empty() {
        return "off".to_string();
    }
    entries
        .iter()
        .map(|(name, weight)| format!("{name}:{weight}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// N308: host-side tile cut for a surface. Uses the device-advertised preferred
/// tile size when non-zero, otherwise [`DEFAULT_TILE_SIZE`]. Never overruns the
/// surface and never returns a zero-area tile.
pub fn plan_tiles(width: u16, height: u16, tile_size: u16) -> Vec<(u16, u16, u16, u16)> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let tile = if tile_size == 0 {
        DEFAULT_TILE_SIZE
    } else {
        tile_size
    };
    let mut tiles = Vec::new();
    let mut y = 0u16;
    while y < height {
        let h = tile.min(height - y);
        let mut x = 0u16;
        while x < width {
            let w = tile.min(width - x);
            tiles.push((x, y, w, h));
            x += w;
        }
        y += h;
    }
    tiles
}

pub struct SurfaceQueue {
    pub info: Surface,
    pending: Option<(u64, Frame)>,
    in_flight: Option<u64>,
    next_id: u64,
    replaced_pending: u64,
}
impl SurfaceQueue {
    pub fn new(info: Surface) -> Self {
        Self {
            info,
            pending: None,
            in_flight: None,
            next_id: 1,
            replaced_pending: 0,
        }
    }
    pub fn replaced_pending(&self) -> u64 {
        self.replaced_pending
    }
    pub fn in_flight(&self) -> Option<u64> {
        self.in_flight
    }
    pub fn enqueue_frame(&mut self, f: Frame) -> Result<u64, CoreError> {
        if f.surface_id != self.info.id
            || f.width != self.info.width
            || f.height != self.info.height
        {
            return Err(CoreError::Dimensions);
        }
        if !self.info.pixel_format.is_implemented() {
            return Err(CoreError::Format(self.info.pixel_format.to_string()));
        }
        let expected = (f.width as usize)
            .checked_mul(f.height as usize)
            .and_then(|x| x.checked_mul(2))
            .ok_or(CoreError::TooLarge)?;
        if f.bytes.len() != expected || expected > MAX_FRAME_BYTES {
            return Err(CoreError::TooLarge);
        }
        let id = self.next_id;
        self.next_id += 1;
        if self.in_flight.is_none() {
            self.in_flight = Some(id);
        } else {
            // 在途帧保持不动。尚未发完的那一帧被新的全帧替换，队列不会再长出第三帧。
            if self.pending.is_some() {
                self.replaced_pending = self.replaced_pending.saturating_add(1);
            }
            self.pending = Some((id, f));
        }
        Ok(id)
    }
    pub fn pending_frame_id(&self) -> Option<u64> {
        self.pending.as_ref().map(|(id, _)| *id)
    }
    pub fn pending_pixels(&self) -> Option<usize> {
        self.pending.as_ref().map(|(_, frame)| frame.bytes.len())
    }
    /// 在途帧和待发帧都占用时，新的 Tile 不能再插进来。
    /// base 指向尚未显示的帧时同样拒绝，避免把没画上的帧当成 Tile 的底图。
    pub fn rejects_tile(&self, base_frame_id: u64) -> bool {
        if self.in_flight.is_some() && self.pending.is_some() {
            return true;
        }
        self.in_flight == Some(base_frame_id)
            || self.pending.as_ref().map(|(id, _)| *id) == Some(base_frame_id)
    }
    /// 在途帧被确认后，返回应当写到链路上的待发帧。没有待发帧时槽位清空。
    pub fn ack(&mut self, id: u64) -> Option<(u64, Frame)> {
        if self.in_flight != Some(id) {
            return None;
        }
        match self.pending.take() {
            Some((pending_id, frame)) => {
                self.in_flight = Some(pending_id);
                Some((pending_id, frame))
            }
            None => {
                self.in_flight = None;
                None
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameTxStage {
    Begin,
    Chunks,
    Commit,
    Done,
}

/// SIM-R03: a frame transaction that can resume after a short write at any
/// BEGIN/CHUNK/COMMIT boundary. The pixels are retained until the transaction
/// finishes; the request id stays in the normal pending set.
pub(crate) struct FrameTransaction {
    pub(crate) surface_id: String,
    pub(crate) frame_id: u64,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) pixels: Vec<u8>,
    pub(crate) max_chunk: usize,
    pub(crate) request_id: u32,
    pub(crate) stage: FrameTxStage,
    pub(crate) next_offset: usize,
}
