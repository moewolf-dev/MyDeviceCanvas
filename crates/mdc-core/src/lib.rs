use mdc_protocol::{
    decode_frame_payload, encode_control, encode_frame_begin, encode_frame_chunk,
    encode_frame_commit, encode_frame_payload, encode_package_begin, encode_package_chunk,
    encode_package_commit, encode_tile_payload, negotiate_mirror, package_checksum, Ack, BootInfo,
    Capabilities, ConfigCommand, ConfigReply, ErrorPayload, FramePayload, Hello, InputEvent,
    MessageType, MetricReport, MirrorCommand, OtaCommand, Packet, Surface, VERSION,
};
mod ledger;
pub use ledger::{AllocationLedger, HostCopyBudget, SimHeap};

use mdc_transport::Transport;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

mod limits;
pub use limits::*;

/// N316: one diagnostic record received on the (separate) diagnostics channel.
/// Kept apart from frame/ack bookkeeping so a reboot or a busy image link cannot
/// mix old results into the current run.
#[derive(Debug, Clone, PartialEq)]
pub enum DiagnosticRecord {
    Metric(MetricReport),
    BootInfo(BootInfo),
    /// N335: structured telemetry event.
    Telemetry(mdc_protocol::TelemetryEvent),
    /// N367: a crash/fault report.
    Crash(mdc_protocol::CrashReport),
}

mod error;
pub use error::CoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Handshaking,
    Ready,
    Reconnecting,
    Closing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaState {
    Unsupported,
    Idle,
    Verifying,
    Installing,
    Rebooting,
    Confirming,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchPoint {
    pub surface_id: u16,
    pub pointer_id: u16,
    pub x: u16,
    pub y: u16,
}

pub fn transform_touch(
    width: u16,
    height: u16,
    rotation: u16,
    x: u16,
    y: u16,
) -> Result<(u16, u16), CoreError> {
    if x >= width || y >= height {
        return Err(CoreError::TileBounds);
    }
    match rotation % 360 {
        0 => Ok((x, y)),
        90 => Ok((height - 1 - y, x)),
        180 => Ok((width - 1 - x, height - 1 - y)),
        270 => Ok((y, width - 1 - x)),
        _ => Err(CoreError::Dimensions),
    }
}

pub struct OtaController {
    pub state: OtaState,
    pub render_frozen: bool,
    has_slot: bool,
}
impl OtaController {
    pub fn new(has_slot: bool) -> Self {
        Self {
            state: if has_slot {
                OtaState::Idle
            } else {
                OtaState::Unsupported
            },
            render_frozen: false,
            has_slot,
        }
    }
    pub fn begin(&mut self) -> Result<(), CoreError> {
        if !self.has_slot {
            return Err(CoreError::Unsupported);
        }
        if self.state != OtaState::Idle {
            return Err(CoreError::Transport("OTA is not idle".into()));
        }
        self.state = OtaState::Verifying;
        Ok(())
    }
    pub fn verified(&mut self) -> Result<(), CoreError> {
        if self.state != OtaState::Verifying {
            return Err(CoreError::Transport("OTA image was not verified".into()));
        }
        self.state = OtaState::Installing;
        self.render_frozen = true;
        Ok(())
    }
    pub fn reboot(&mut self) -> Result<(), CoreError> {
        if self.state != OtaState::Installing {
            return Err(CoreError::Transport("OTA install is not active".into()));
        }
        self.state = OtaState::Rebooting;
        Ok(())
    }
    pub fn confirm(&mut self, success: bool) {
        self.state = if success {
            OtaState::Succeeded
        } else {
            OtaState::Failed
        };
        self.render_frozen = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter_ms: u64,
}
impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 1_000,
            max_delay_ms: 30_000,
            jitter_ms: 0,
        }
    }
}
impl ReconnectPolicy {
    pub fn delay_ms(&self, attempt: u32, jitter: u64) -> Option<u64> {
        if attempt == 0 || attempt > self.max_attempts {
            return None;
        }
        let exponent = attempt.saturating_sub(1).min(5);
        let base = self
            .base_delay_ms
            .saturating_mul(1u64 << exponent)
            .min(self.max_delay_ms);
        Some(
            base.saturating_add(jitter.min(self.jitter_ms))
                .min(self.max_delay_ms),
        )
    }
}

/// Timeouts and reconnect knobs for Session::tick (virtual clock friendly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    pub connect_timeout_ms: u64,
    pub ack_timeout_ms: u64,
    pub heartbeat_ms: u64,
    pub reconnect: bool,
}
impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            connect_timeout_ms: 20_000,
            ack_timeout_ms: 5_000,
            heartbeat_ms: 5_000,
            reconnect: true,
        }
    }
}

/// N464: a recognised pointer gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureKind {
    Tap,
    LongPress,
    Drag,
    /// N725: two pointers changed their distance and/or angle.
    Pinch,
    /// N735: two pointers translated together.
    Pan,
    /// N745: three pointers swiped together.
    Swipe,
    /// N755: a second tap within the multi-tap window.
    DoubleTap,
    /// N755: a third tap within the multi-tap window.
    TripleTap,
}

/// N464: a gesture derived from raw input events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gesture {
    pub kind: GestureKind,
    pub pointer_id: u16,
    pub x: u16,
    pub y: u16,
    /// N493: the hit-tested scene node, when the device reported one.
    pub node: Option<String>,
    /// N725: for `Pinch`, the distance ratio in permille (1000 = unchanged).
    pub scale: u16,
    /// N725: for `Pinch`, the angle change in degrees (signed).
    pub rotation: i16,
    /// N735: for `Pan`/`Swipe`, the midpoint translation in pixels.
    pub dx: i16,
    pub dy: i16,
}

/// N464: thresholds for gesture recognition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GestureConfig {
    /// A press released within this window (and without dragging) is a tap.
    pub tap_max_ms: u64,
    /// A press held this long emits `LongPress`.
    pub long_press_ms: u64,
    /// Movement beyond this many pixels becomes a drag.
    pub drag_threshold: u16,
    /// N755: taps closer together than this form a double/triple tap.
    pub multi_tap_ms: u64,
    /// N774: consecutive taps must stay within this many pixels on both axes.
    pub multi_tap_radius: u16,
}

/// N794: `mdc_session_set_multi_tap_ms` refuses a window above this (milliseconds).
pub const MULTI_TAP_MS_MAX: u64 = 60_000;

impl Default for GestureConfig {
    fn default() -> Self {
        Self {
            tap_max_ms: 250,
            long_press_ms: 600,
            drag_threshold: 6,
            multi_tap_ms: 300,
            multi_tap_radius: 24,
        }
    }
}

#[derive(Debug, Clone)]
struct PointerState {
    down_ms: u64,
    x: u16,
    y: u16,
    dragged: bool,
    long_pressed: bool,
    /// N493: the node under the pointer when it went down.
    node: Option<String>,
    /// N725: this pointer took part in a pinch, so its release is not a tap.
    pinched: bool,
}

/// N725: the initial two-pointer geometry a pinch is measured against.
#[derive(Debug, Clone, Copy)]
struct PinchAnchor {
    distance: f32,
    angle: f32,
    last_scale: u16,
    last_rotation: i16,
    last_mid: (u16, u16),
}

/// N725: minimum change before a pinch reports another event.
const PINCH_SCALE_STEP: u16 = 20;
const PINCH_ROTATE_STEP: i16 = 4;
/// N745: minimum three-finger midpoint movement before a swipe is reported.
const SWIPE_THRESHOLD: i32 = 8;

/// N725: wrap an angle difference into `-180..=180` degrees.
fn normalize_degrees(degrees: f32) -> i16 {
    let mut value = degrees % 360.0;
    if value > 180.0 {
        value -= 360.0;
    } else if value < -180.0 {
        value += 360.0;
    }
    value.round() as i16
}

/// N464: derive tap/long-press/drag gestures from a pointer's input events.
/// N725: two simultaneous pointers additionally derive pinch/rotate.
#[derive(Debug, Clone, Default)]
pub struct GestureTracker {
    config: GestureConfig,
    pointers: std::collections::BTreeMap<u16, PointerState>,
    pinch: Option<PinchAnchor>,
    /// N745: the midpoint anchor for a three-finger swipe.
    swipe: Option<(u16, u16)>,
    /// N755: the last tap's time/position/node and the current streak (0..=3).
    /// N774: taps at different nodes or too far away start a new chain.
    last_tap: Option<(u64, u16, u16, Option<String>)>,
    tap_streak: u8,
}

impl GestureTracker {
    pub fn new(config: GestureConfig) -> Self {
        Self {
            config,
            pointers: std::collections::BTreeMap::new(),
            pinch: None,
            swipe: None,
            last_tap: None,
            tap_streak: 0,
        }
    }

    /// N488: replace the recognition thresholds.
    pub fn set_config(&mut self, config: GestureConfig) {
        self.config = config;
    }

    /// SIM-R04: drop all in-progress pointers and tap chains.
    pub fn reset(&mut self) {
        self.pointers.clear();
        self.pinch = None;
        self.swipe = None;
        self.last_tap = None;
        self.tap_streak = 0;
    }

    /// N488: the current thresholds.
    pub fn config(&self) -> GestureConfig {
        self.config
    }

    /// N764: any non-tap gesture breaks the multi-tap chain.
    fn reset_taps(&mut self) {
        self.last_tap = None;
        self.tap_streak = 0;
    }

    /// Feed one input event; returns a gesture when it completes one.
    pub fn on_event(&mut self, event: &mdc_protocol::InputEvent, now_ms: u64) -> Option<Gesture> {
        // A two-finger move can produce a pan and a pinch in the same event;
        // the single-gesture API reports the pinch (the richer gesture).
        self.on_events(event, now_ms).pop()
    }

    /// N735: like [`on_event`](Self::on_event) but returns every gesture derived
    /// from the event (a two-finger move may both pan and pinch).
    pub fn on_events(&mut self, event: &mdc_protocol::InputEvent, now_ms: u64) -> Vec<Gesture> {
        match event.phase.as_str() {
            "down" => {
                if self.pointers.len() >= MAX_ACTIVE_POINTERS
                    && !self.pointers.contains_key(&event.pointer_id)
                {
                    return Vec::new();
                }
                self.pointers.insert(
                    event.pointer_id,
                    PointerState {
                        down_ms: now_ms,
                        x: event.x,
                        y: event.y,
                        dragged: false,
                        long_pressed: false,
                        node: event.node.clone(),
                        pinched: false,
                    },
                );
                // N725: a second pointer starts a pinch; neither finger can
                // subsequently become a tap/long-press.
                if self.pointers.len() == 2 {
                    self.pinch = self.anchor();
                    // N764: a multi-touch sequence ends any tap chain.
                    self.reset_taps();
                }
                if self.pointers.len() >= 2 {
                    for state in self.pointers.values_mut() {
                        state.pinched = true;
                        state.long_pressed = true;
                    }
                }
                if self.pointers.len() == 3 {
                    self.swipe = self.midpoint_all();
                }
                Vec::new()
            }
            "move" => {
                // N745: three pointers become a swipe (no pan/pinch).
                if self.pointers.len() >= 3 {
                    self.reset_taps();
                    let Some(state) = self.pointers.get_mut(&event.pointer_id) else {
                        return Vec::new();
                    };
                    state.x = event.x;
                    state.y = event.y;
                    state.pinched = true;
                    let Some(anchor) = self.swipe else {
                        return Vec::new();
                    };
                    let Some((x, y)) = self.midpoint_all() else {
                        return Vec::new();
                    };
                    let dx = i32::from(x) - i32::from(anchor.0);
                    let dy = i32::from(y) - i32::from(anchor.1);
                    if dx.abs().max(dy.abs()) < SWIPE_THRESHOLD {
                        return Vec::new();
                    }
                    self.swipe = Some((x, y));
                    return vec![Gesture {
                        kind: GestureKind::Swipe,
                        pointer_id: event.pointer_id,
                        x,
                        y,
                        node: None,
                        scale: 1000,
                        rotation: 0,
                        dx: dx.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
                        dy: dy.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
                    }];
                }
                if self.pointers.len() >= 2 {
                    self.reset_taps();
                    let Some(state) = self.pointers.get_mut(&event.pointer_id) else {
                        return Vec::new();
                    };
                    state.x = event.x;
                    state.y = event.y;
                    state.pinched = true;
                    let Some(anchor) = self.pinch else {
                        return Vec::new();
                    };
                    let mut out = Vec::new();
                    let Some((x, y)) = self.midpoint() else {
                        return out;
                    };
                    // N735: report the midpoint translation whenever it changes.
                    let dx = i32::from(x) - i32::from(anchor.last_mid.0);
                    let dy = i32::from(y) - i32::from(anchor.last_mid.1);
                    if dx != 0 || dy != 0 {
                        out.push(Gesture {
                            kind: GestureKind::Pan,
                            pointer_id: event.pointer_id,
                            x,
                            y,
                            node: None,
                            scale: 1000,
                            rotation: 0,
                            dx: dx.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
                            dy: dy.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
                        });
                        if let Some(anchor) = self.pinch.as_mut() {
                            anchor.last_mid = (x, y);
                        }
                    }
                    let Some((distance, angle)) = self.metrics() else {
                        return out;
                    };
                    let scale = ((distance / anchor.distance) * 1000.0)
                        .round()
                        .clamp(0.0, f32::from(u16::MAX)) as u16;
                    let rotation = normalize_degrees((angle - anchor.angle).to_degrees());
                    let scale_delta =
                        (i32::from(scale) - i32::from(anchor.last_scale)).unsigned_abs();
                    let rotation_delta =
                        (i32::from(rotation) - i32::from(anchor.last_rotation)).unsigned_abs();
                    if scale_delta >= u32::from(PINCH_SCALE_STEP)
                        || rotation_delta >= u32::from(PINCH_ROTATE_STEP.unsigned_abs())
                    {
                        if let Some(anchor) = self.pinch.as_mut() {
                            anchor.last_scale = scale;
                            anchor.last_rotation = rotation;
                        }
                        out.push(Gesture {
                            kind: GestureKind::Pinch,
                            pointer_id: event.pointer_id,
                            x,
                            y,
                            node: None,
                            scale,
                            rotation,
                            dx: 0,
                            dy: 0,
                        });
                    }
                    return out;
                }
                let Some(state) = self.pointers.get_mut(&event.pointer_id) else {
                    return Vec::new();
                };
                let dx = i32::from(event.x) - i32::from(state.x);
                let dy = i32::from(event.y) - i32::from(state.y);
                if !state.dragged && dx.abs().max(dy.abs()) >= i32::from(self.config.drag_threshold)
                {
                    state.dragged = true;
                    // N764: dragging is not part of a double/triple tap.
                    self.reset_taps();
                    return vec![Gesture {
                        kind: GestureKind::Drag,
                        pointer_id: event.pointer_id,
                        x: event.x,
                        y: event.y,
                        node: event.node.clone(),
                        scale: 1000,
                        rotation: 0,
                        dx: 0,
                        dy: 0,
                    }];
                }
                Vec::new()
            }
            "up" | "cancel" => {
                if event.phase == "cancel" {
                    self.reset_taps();
                }
                let Some(state) = self.pointers.remove(&event.pointer_id) else {
                    return Vec::new();
                };
                // N725: ending a pinch suppresses taps for the remaining finger.
                if self.pointers.len() < 2 && self.pinch.take().is_some() {
                    for pointer in self.pointers.values_mut() {
                        pointer.pinched = true;
                    }
                }
                if self.pointers.len() < 3 && self.swipe.take().is_some() {
                    for pointer in self.pointers.values_mut() {
                        pointer.pinched = true;
                    }
                }
                if state.pinched {
                    return Vec::new();
                }
                if state.dragged {
                    return vec![Gesture {
                        kind: GestureKind::Drag,
                        pointer_id: event.pointer_id,
                        x: event.x,
                        y: event.y,
                        node: event.node.clone(),
                        scale: 1000,
                        rotation: 0,
                        dx: 0,
                        dy: 0,
                    }];
                }
                // N764: a cancelled pointer never becomes a tap.
                if event.phase == "cancel" {
                    return Vec::new();
                }
                if state.long_pressed {
                    return Vec::new();
                }
                if now_ms.saturating_sub(state.down_ms) <= self.config.tap_max_ms {
                    // N755: consecutive taps inside the window become a double
                    // (2) or triple (3) tap; the streak then restarts.
                    // N774: a different node or a far-away tap starts a new chain.
                    let node = event.node.clone().or(state.node.clone());
                    let within = self.last_tap.as_ref().is_some_and(|(at, x, y, prior)| {
                        now_ms.saturating_sub(*at) <= self.config.multi_tap_ms
                            && event.x.abs_diff(*x) <= self.config.multi_tap_radius
                            && event.y.abs_diff(*y) <= self.config.multi_tap_radius
                            && *prior == node
                    });
                    self.tap_streak = if within {
                        self.tap_streak.saturating_add(1).min(3)
                    } else {
                        1
                    };
                    self.last_tap = Some((now_ms, event.x, event.y, node.clone()));
                    let kind = match self.tap_streak {
                        2 => GestureKind::DoubleTap,
                        3 => {
                            self.tap_streak = 0;
                            GestureKind::TripleTap
                        }
                        _ => GestureKind::Tap,
                    };
                    vec![Gesture {
                        kind,
                        pointer_id: event.pointer_id,
                        x: event.x,
                        y: event.y,
                        node,
                        scale: 1000,
                        rotation: 0,
                        dx: 0,
                        dy: 0,
                    }]
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    /// N725: the initial distance/angle between the first two pointers.
    fn anchor(&self) -> Option<PinchAnchor> {
        let (distance, angle) = self.metrics()?;
        let (mx, my) = self.midpoint()?;
        Some(PinchAnchor {
            distance,
            angle,
            last_scale: 1000,
            last_rotation: 0,
            last_mid: (mx, my),
        })
    }

    fn metrics(&self) -> Option<(f32, f32)> {
        let mut pointers = self.pointers.values();
        let a = pointers.next()?;
        let b = pointers.next()?;
        let dx = f32::from(b.x) - f32::from(a.x);
        let dy = f32::from(b.y) - f32::from(a.y);
        Some(((dx * dx + dy * dy).sqrt().max(0.5), dy.atan2(dx)))
    }

    fn midpoint(&self) -> Option<(u16, u16)> {
        let mut pointers = self.pointers.values();
        let a = pointers.next()?;
        let b = pointers.next()?;
        Some((
            ((u32::from(a.x) + u32::from(b.x)) / 2) as u16,
            ((u32::from(a.y) + u32::from(b.y)) / 2) as u16,
        ))
    }

    /// N745: the midpoint of every active pointer (three-finger swipe).
    fn midpoint_all(&self) -> Option<(u16, u16)> {
        let count = self.pointers.len() as u32;
        if count == 0 {
            return None;
        }
        let (x, y) = self
            .pointers
            .values()
            .fold((0u32, 0u32), |(x, y), pointer| {
                (x + u32::from(pointer.x), y + u32::from(pointer.y))
            });
        Some(((x / count) as u16, (y / count) as u16))
    }

    /// Advance held pointers; emits `LongPress` once per pointer.
    pub fn tick(&mut self, now_ms: u64) -> Option<Gesture> {
        let mut pressed: Option<Gesture> = None;
        for (pointer_id, state) in self.pointers.iter_mut() {
            if state.pinched {
                continue;
            }
            if !state.long_pressed
                && now_ms.saturating_sub(state.down_ms) >= self.config.long_press_ms
            {
                state.long_pressed = true;
                // N764: a long press breaks the tap chain.
                pressed = Some(Gesture {
                    kind: GestureKind::LongPress,
                    pointer_id: *pointer_id,
                    x: state.x,
                    y: state.y,
                    node: state.node.clone(),
                    scale: 1000,
                    rotation: 0,
                    dx: 0,
                    dy: 0,
                });
                break;
            }
        }
        if pressed.is_some() {
            self.reset_taps();
        }
        pressed
    }
}

/// N750: the result of one joint streaming tuning pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveTuning {
    pub bitrate: u64,
    pub pacing: Option<mdc_schedule::SchedulerConfig>,
    pub dirty_threshold: Option<f64>,
}

/// N398: host-side session counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionStats {
    pub packets_sent: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub acks_received: u64,
    pub frames_sent: u64,
    /// N436: frames the device dropped (reported by the device).
    pub dropped_frames: u64,
    /// N446: packages installed on the device.
    pub packages_sent: u64,
    /// N446: complete mirror frames received.
    pub mirror_frames: u64,
    /// N446: frames refused by the host pacing budget.
    pub pacing_dropped: u64,
    /// N506: partial (tile) updates sent.
    pub tiles_sent: u64,
    /// N506: bytes covered by the tiles sent.
    pub tile_bytes_sent: u64,
    /// N547: tiles the device reports having applied (from its `tiles_received`
    /// metric). 0 until the device reports a count.
    pub device_tiles_received: u64,
    /// N547: tiles the host sent that the device has not acknowledged.
    pub tiles_missing: u64,
}

/// SIM-R09: bounded container occupancies. A soak samples this to prove that
/// completed transactions do not grow history or input/mirror state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContainerCounts {
    pub rx_bytes: usize,
    pub pending_requests: usize,
    pub pending_control: usize,
    pub pending_frames: usize,
    pub request_deadlines: usize,
    pub finished_acks: usize,
    pub displayed_timings: usize,
    pub held_frames: usize,
    pub diagnostics: usize,
    pub config_cache: usize,
    pub pending_configs: usize,
    pub active_pointers: usize,
    pub mirror_inbox_frames: usize,
    pub mirror_base_frames: usize,
    pub frame_transactions: usize,
    pub manager_events: usize,
    pub devices: usize,
}

/// N547: the metric name a device uses to report applied partial-update tiles.
pub const TILES_RECEIVED_METRIC: &str = "tiles_received";

/// N586: the device config key that negotiates how many unacknowledged tiles the
/// host tolerates before forcing a full frame.
pub const TILES_MISSING_THRESHOLD_CONFIG: &str = "tile_missing_threshold";

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
    surface: (u16, u16),
    tile_size: u16,
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

pub struct Device {
    pub capabilities: Capabilities,
    pub state: ConnectionState,
    pub surfaces: Vec<SurfaceQueue>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ManagerEvent {
    Added(String),
    Removed(String),
    Connected(String),
    Disconnected(String),
    /// Active endpoint replaced; old Session ACKs must not affect the new one.
    EndpointSwitched {
        device_id: String,
    },
    Input {
        device_id: String,
        event: InputEvent,
    },
    /// N473: a recognised gesture (tap / long-press / drag).
    Gesture {
        device_id: String,
        gesture: Gesture,
    },
    /// Form B (N24): a complete mirrored frame was stored. Metadata only — the
    /// pixels stay in the session inbox (`take_mirror_frame`) so the event queue
    /// stays small.
    Mirror {
        device_id: String,
        surface_id: String,
        frame_id: u64,
        width: u16,
        height: u16,
        frames_received: u64,
        frames_dropped: u64,
    },
    /// N357: a structured telemetry event (N335). Metadata only; the full record
    /// is in the diagnostics queue (`take_diagnostics`).
    Telemetry {
        device_id: String,
        name: String,
        level: Option<String>,
    },
    Error {
        device_id: String,
        message: String,
    },
}
#[derive(Default)]
pub struct DeviceManager {
    devices: BTreeMap<String, Device>,
    events: std::collections::VecDeque<ManagerEvent>,
    /// Single active session endpoint per device id.
    active_endpoints: BTreeMap<String, String>,
}
impl DeviceManager {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn upsert(&mut self, capabilities: Capabilities) -> Result<(), CoreError> {
        if capabilities.device_id.is_empty() || capabilities.surfaces.is_empty() {
            return Err(CoreError::Protocol(
                "device identity/surfaces missing".into(),
            ));
        }
        let id = capabilities.device_id.clone();
        let event = if self.devices.contains_key(&id) {
            ManagerEvent::Connected(id.clone())
        } else {
            ManagerEvent::Added(id.clone())
        };
        let mut device = Device::new(capabilities);
        device.state = ConnectionState::Ready;
        self.devices.insert(id, device);
        self.push_event(event);
        Ok(())
    }
    pub fn remove(&mut self, device_id: &str) -> bool {
        self.active_endpoints.remove(device_id);
        let removed = self.devices.remove(device_id).is_some();
        if removed {
            self.push_event(ManagerEvent::Removed(device_id.into()));
        }
        removed
    }
    pub fn device(&self, device_id: &str) -> Option<&Device> {
        self.devices.get(device_id)
    }
    pub fn device_mut(&mut self, device_id: &str) -> Option<&mut Device> {
        self.devices.get_mut(device_id)
    }
    pub fn devices(&self) -> impl Iterator<Item = &Device> {
        self.devices.values()
    }
    pub fn drain_events(&mut self) -> impl Iterator<Item = ManagerEvent> + '_ {
        self.events.drain(..)
    }
    /// SIM-R09: queued event count for soak/container assertions.
    pub fn event_count(&self) -> usize {
        self.events.len()
    }
    /// Store the single active session endpoint for `device_id` (replaces any prior).
    /// Returns the previous endpoint if this is a switch.
    pub fn connect_session(
        &mut self,
        device_id: &str,
        endpoint: impl Into<String>,
    ) -> Option<String> {
        let endpoint = endpoint.into();
        let previous = self
            .active_endpoints
            .insert(device_id.to_string(), endpoint);
        if let Some(device) = self.devices.get_mut(device_id) {
            device.state = ConnectionState::Ready;
        }
        if previous.is_some() {
            self.push_event(ManagerEvent::EndpointSwitched {
                device_id: device_id.into(),
            });
        } else {
            self.push_event(ManagerEvent::Connected(device_id.into()));
        }
        previous
    }
    pub fn active_endpoint(&self, device_id: &str) -> Option<&str> {
        self.active_endpoints.get(device_id).map(|s| s.as_str())
    }
    pub fn clear_endpoint(&mut self, device_id: &str) {
        if self.active_endpoints.remove(device_id).is_some() {
            self.push_event(ManagerEvent::Disconnected(device_id.into()));
        }
    }
    fn push_event(&mut self, event: ManagerEvent) {
        if self.events.len() >= MAX_MANAGER_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// Coalesce consecutive moves. A full queue keeps down/up/cancel, reports an error, and cancels that pointer.
    /// N473: queue a recognised gesture.
    pub fn push_gesture(&mut self, device_id: String, gesture: Gesture) {
        self.events
            .push_back(ManagerEvent::Gesture { device_id, gesture });
        if self.events.len() >= MAX_MANAGER_EVENTS {
            self.events.pop_front();
        }
    }

    pub fn push_input(&mut self, device_id: String, event: InputEvent) {
        if event.phase == "move" {
            if let Some(ManagerEvent::Input {
                device_id: queued_id,
                event: queued,
            }) = self.events.back_mut()
            {
                if queued_id == &device_id
                    && queued.pointer_id == event.pointer_id
                    && queued.phase == "move"
                {
                    queued.x = event.x;
                    queued.y = event.y;
                    return;
                }
            }
        }
        let critical = matches!(event.phase.as_str(), "down" | "up" | "cancel");
        if self.events.len() >= MAX_MANAGER_EVENTS && critical {
            let slots = if event.phase == "cancel" { 2 } else { 3 };
            self.evict_until(MAX_MANAGER_EVENTS - slots);
            self.events.push_back(ManagerEvent::Error {
                device_id: device_id.clone(),
                message: "input queue overflow".into(),
            });
            if event.phase != "cancel" {
                self.events.push_back(ManagerEvent::Input {
                    device_id: device_id.clone(),
                    event: InputEvent {
                        phase: "cancel".into(),
                        ..event.clone()
                    },
                });
            }
            self.events
                .push_back(ManagerEvent::Input { device_id, event });
            return;
        }
        if self.events.len() >= MAX_MANAGER_EVENTS {
            self.evict_until(MAX_MANAGER_EVENTS - 1);
        }
        self.events
            .push_back(ManagerEvent::Input { device_id, event });
    }

    /// N24: record that the session stored a mirrored frame. The bounded event
    /// queue drops the oldest event when full (mirror metadata is not critical).
    #[allow(clippy::too_many_arguments)]
    pub fn push_mirror(
        &mut self,
        device_id: String,
        surface_id: String,
        frame_id: u64,
        width: u16,
        height: u16,
        frames_received: u64,
        frames_dropped: u64,
    ) {
        self.push_event(ManagerEvent::Mirror {
            device_id,
            surface_id,
            frame_id,
            width,
            height,
            frames_received,
            frames_dropped,
        });
    }

    /// N357: record a telemetry event (metadata only) on the bounded queue.
    pub fn push_telemetry(&mut self, device_id: String, name: String, level: Option<String>) {
        self.push_event(ManagerEvent::Telemetry {
            device_id,
            name,
            level,
        });
    }

    fn evict_until(&mut self, limit: usize) {
        while self.events.len() > limit {
            if let Some(index) = self.events.iter().position(
                |event| matches!(event, ManagerEvent::Input { event, .. } if event.phase == "move"),
            ) {
                self.events.remove(index);
            } else {
                self.events.pop_front();
            }
        }
    }
}

pub struct DisplayTiming {
    pub transport_us: u128,
    pub render_us: u128,
    pub e2e_us: u128,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameTxStage {
    Begin,
    Chunks,
    Commit,
    Done,
}

/// SIM-R03: a frame transaction that can resume after a short write at any
/// BEGIN/CHUNK/COMMIT boundary. The pixels are retained until the transaction
/// finishes; the request id stays in the normal pending set.
struct FrameTransaction {
    surface_id: String,
    frame_id: u64,
    width: u16,
    height: u16,
    pixels: Vec<u8>,
    max_chunk: usize,
    request_id: u32,
    stage: FrameTxStage,
    next_offset: usize,
}

pub struct Session<T: Transport> {
    transport: T,
    pub state: ConnectionState,
    pub device: Option<Device>,
    rx: Vec<u8>,
    next_request_id: u32,
    max_message: usize,
    max_chunk: usize,
    pending_requests: std::collections::BTreeSet<u32>,
    /// SIM-R04: per-request virtual deadline (milliseconds).
    request_deadlines: BTreeMap<u32, u64>,
    /// Control messages only. Frame and Tile stay on the surface queue.
    pending_control: std::collections::BTreeSet<u32>,
    frame_sent_at: BTreeMap<u32, std::time::Instant>,
    frame_received_at: BTreeMap<u32, std::time::Instant>,
    displayed_timings: Vec<DisplayTiming>,
    dropped_frames: u64,
    /// request_id -> (surface_id, frame_id) for in-flight Frame/Tile acks.
    pending_frames: BTreeMap<u32, (String, u64)>,
    /// ACKs and errors below this id belong to a previous link.
    request_floor: u32,
    /// Displayed acknowledgements already applied. A replay must not launch again.
    finished_acks: std::collections::BTreeSet<u32>,
    /// Tail of a packet that a short write did not accept.
    unsent: Option<Vec<u8>>,
    /// SIM-R04: request id of the current HELLO; only its Capabilities reply may
    /// move the session to Ready.
    hello_request: Option<u32>,
    /// SIM-R03: resumable frame transaction.
    frame_tx: Option<FrameTransaction>,
    copy_limit: usize,
    copy_budget: HostCopyBudget,
    /// (surface id, frame id) -> pixel bytes held in the copy budget.
    held_frames: BTreeMap<(String, u64), usize>,
    pub reconnect_policy: ReconnectPolicy,
    reconnect_attempt: u32,
    reconnect_enabled: bool,
    pub config: SessionConfig,
    virtual_ms: u64,
    connect_started_ms: u64,
    last_send_ms: u64,
    last_heartbeat_ms: u64,
    /// Ping sent and not yet answered. Three misses start reconnect.
    missed_heartbeats: u32,
    heartbeat_outstanding: bool,
    /// SIM-R04: request id of the outstanding Ping; only that Pong counts.
    heartbeat_request: Option<u32>,
    reconnect_ready_ms: Option<u64>,
    /// After reconnect backoff, host must supply a fresh transport via
    /// [`provide_transport`](Self::provide_transport) before HELLO is resent.
    pub awaiting_transport: bool,
    /// After reconnect / endpoint switch, tiles are blocked until a full frame is ACKed.
    pub needs_full_frame: bool,
    manager: Option<Arc<Mutex<DeviceManager>>>,
    /// Latest complete mirror frame. Bounded to one; a newer frame replaces it.
    mirror_inbox: Option<MirrorFrame>,
    /// In-progress chunked mirror frame: (surface, w, h, assembler).
    mirror_assembler: Option<(String, u16, u16, FrameAssembler)>,
    /// N264: the negotiated mirror mode ("full-frame" | "dirty-region").
    mirror_mode: String,
    /// N264: the last complete mirror frame, patched by dirty-region updates.
    mirror_last: Option<(String, u16, u16, Vec<u8>)>,
    mirror_frames: u64,
    mirror_dropped: u64,
    mirror_last_frame_id: u64,
    packages_sent: u64,
    /// N40: configuration values learned from device replies.
    config_cache: BTreeMap<String, String>,
    /// SIM-R04: key -> request id for config replies that were actually asked for.
    pending_configs: BTreeMap<String, u32>,
    /// N304: host boot identity advertised in Hello (0 = unset).
    host_boot_id: u64,
    /// N304: the peer's last reported boot identity (None before the first
    /// Capabilities reply). A change means the device rebooted.
    device_boot_id: Option<u64>,
    /// N304: the peer's last reported run id.
    device_run_id: u64,
    /// N316: bounded diagnostics received on the separate channel.
    diagnostics: std::collections::VecDeque<DiagnosticRecord>,
    /// N329: opt-in host frame pacing (fps clamp + byte budget).
    pacing: Option<mdc_schedule::Scheduler>,
    /// N329: frames refused by the pacing budget.
    pacing_dropped: u64,
    /// N398: host-side session counters.
    stats: SessionStats,
    /// N547: the last `tiles_received` count reported by the device.
    device_tiles_received: u64,
    /// N419: the highest OTA phase sent (0 none, 1 begin, 2 chunk, 3 confirm).
    ota_phase: u8,
    /// N732: `(virtual_ms, bytes_sent)` baseline for the measured bitrate.
    throughput_mark: (u64, u64),
    /// N732: the last measured bitrate (reused when no new sample exists).
    last_bitrate: Option<u64>,
    /// N473: derives gestures from incoming input events.
    gestures: GestureTracker,
}
impl<T: Transport> Session<T> {
    pub fn new(transport: T) -> Self {
        Self::with_config(transport, SessionConfig::default())
    }
    pub fn with_config(transport: T, config: SessionConfig) -> Self {
        Self {
            transport,
            state: ConnectionState::Disconnected,
            device: None,
            rx: Vec::new(),
            next_request_id: 1,
            max_message: MAX_FRAME_BYTES,
            max_chunk: mdc_protocol::DEFAULT_MAX_CHUNK,
            pending_requests: std::collections::BTreeSet::new(),
            request_deadlines: BTreeMap::new(),
            pending_control: std::collections::BTreeSet::new(),
            frame_sent_at: BTreeMap::new(),
            frame_received_at: BTreeMap::new(),
            displayed_timings: Vec::new(),
            dropped_frames: 0,
            pending_frames: BTreeMap::new(),
            request_floor: 0,
            finished_acks: std::collections::BTreeSet::new(),
            unsent: None,
            hello_request: None,
            frame_tx: None,
            copy_limit: MAX_FRAME_BYTES * 3,
            copy_budget: HostCopyBudget::new(MAX_FRAME_BYTES * 3),
            held_frames: BTreeMap::new(),
            reconnect_policy: ReconnectPolicy::default(),
            reconnect_attempt: 0,
            reconnect_enabled: config.reconnect,
            config,
            virtual_ms: 0,
            connect_started_ms: 0,
            last_send_ms: 0,
            last_heartbeat_ms: 0,
            missed_heartbeats: 0,
            heartbeat_outstanding: false,
            heartbeat_request: None,
            reconnect_ready_ms: None,
            awaiting_transport: false,
            needs_full_frame: false,
            manager: None,
            mirror_inbox: None,
            mirror_assembler: None,
            mirror_mode: String::new(),
            mirror_last: None,
            mirror_frames: 0,
            mirror_dropped: 0,
            mirror_last_frame_id: 0,
            packages_sent: 0,
            config_cache: BTreeMap::new(),
            pending_configs: BTreeMap::new(),
            host_boot_id: 0,
            device_boot_id: None,
            device_run_id: 0,
            diagnostics: std::collections::VecDeque::new(),
            pacing: None,
            pacing_dropped: 0,
            stats: SessionStats::default(),
            device_tiles_received: 0,
            ota_phase: 0,
            throughput_mark: (0, 0),
            last_bitrate: None,
            gestures: GestureTracker::default(),
        }
    }
    pub fn set_manager(&mut self, manager: Arc<Mutex<DeviceManager>>) {
        self.manager = Some(manager);
    }
    /// N329: pace `send_frame` with an fps clamp and a byte budget. `0` for either
    /// limit disables that limit (`max_fps = 0` = no clamp, `bytes_per_second = 0`
    /// = unlimited). A paced frame that does not fit returns [`CoreError::Throttled`].
    pub fn set_frame_pacing(&mut self, max_fps: u16, bytes_per_second: u64) {
        self.pacing = Some(mdc_schedule::Scheduler::new(max_fps, bytes_per_second));
    }
    pub fn clear_frame_pacing(&mut self) {
        self.pacing = None;
    }
    /// N329: frames refused by the pacing budget since the last reset.
    pub fn pacing_dropped(&self) -> u64 {
        self.pacing_dropped
    }
    /// N304: set the host boot identity advertised in Hello.
    pub fn set_host_boot_id(&mut self, boot_id: u64) {
        self.host_boot_id = boot_id;
    }
    pub fn host_boot_id(&self) -> u64 {
        self.host_boot_id
    }
    /// N304: the peer boot identity from the last Capabilities reply.
    pub fn device_boot_id(&self) -> Option<u64> {
        self.device_boot_id
    }
    pub fn device_run_id(&self) -> u64 {
        self.device_run_id
    }
    /// N304: adopt a Capabilities reply's run identity. Returns true when the
    /// device reported a new boot id (the previous run's pending ACKs are dropped).
    fn adopt_device_identity(&mut self, caps: &Capabilities) -> bool {
        self.note_run_identity(caps.boot_id, caps.run_id)
    }
    /// N304/N316: fold a peer-reported `(boot_id, run_id)` into the run
    /// bookkeeping. A changed boot id discards the previous run.
    fn note_run_identity(&mut self, boot_id: u64, run_id: u64) -> bool {
        if boot_id == 0 && run_id == 0 {
            return false;
        }
        let restarted = matches!(
            (self.device_boot_id, boot_id),
            (Some(old), new) if new != 0 && old != new
        );
        if boot_id != 0 {
            self.device_boot_id = Some(boot_id);
        }
        if run_id != 0 {
            self.device_run_id = run_id;
        }
        if restarted {
            self.discard_run();
        }
        restarted
    }
    /// N316: drain the bounded diagnostic queue (metrics + boot info). This is a
    /// separate channel: it never mixes with frame/ack results.
    pub fn take_diagnostics(&mut self) -> Vec<DiagnosticRecord> {
        self.diagnostics.drain(..).collect()
    }
    pub fn diagnostic_count(&self) -> usize {
        self.diagnostics.len()
    }
    fn push_diagnostic(&mut self, record: DiagnosticRecord) {
        while self.diagnostics.len() >= MAX_DIAGNOSTICS {
            self.diagnostics.pop_front();
        }
        self.diagnostics.push_back(record);
    }
    /// SIM-R06: every path that abandons a link clears the same state. Keeping
    /// this in one place prevents a reconnect from inheriting a stale request
    /// deadline, frame transaction, mirror base or gesture chain.
    fn clear_link_state(&mut self) {
        self.rx.clear();
        self.pending_requests.clear();
        self.request_deadlines.clear();
        self.pending_control.clear();
        self.frame_sent_at.clear();
        self.frame_received_at.clear();
        self.pending_frames.clear();
        self.unsent = None;
        self.frame_tx = None;
        self.hello_request = None;
        self.finished_acks.clear();
        self.mirror_inbox = None;
        self.mirror_assembler = None;
        self.mirror_last = None;
        self.mirror_last_frame_id = 0;
        self.config_cache.clear();
        self.pending_configs.clear();
        self.gestures.reset();
        self.missed_heartbeats = 0;
        self.heartbeat_outstanding = false;
        self.heartbeat_request = None;
        self.copy_budget = HostCopyBudget::new(self.copy_limit);
    }

    /// Drop every outstanding request/held frame from the previous device run.
    fn discard_run(&mut self) {
        self.held_frames.clear();
        self.clear_link_state();
        self.request_floor = self.next_request_id;
        self.missed_heartbeats = 0;
        self.heartbeat_outstanding = false;
        self.needs_full_frame = true;
    }
    pub fn connect(&mut self) -> Result<(), CoreError> {
        if self.state != ConnectionState::Disconnected
            && self.state != ConnectionState::Connecting
            && self.state != ConnectionState::Reconnecting
        {
            return Err(CoreError::Transport("session already active".into()));
        }
        self.state = ConnectionState::Connecting;
        self.connect_started_ms = self.virtual_ms;
        self.state = ConnectionState::Handshaking;
        let hello = encode_control(&Hello {
            major: VERSION.major,
            minor: VERSION.minor,
            max_message: MAX_FRAME_BYTES as u32,
            max_chunk: mdc_protocol::DEFAULT_MAX_CHUNK as u32,
            boot_id: self.host_boot_id,
        })
        .map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.hello_request = Some(self.write_packet(MessageType::Hello, hello)?);
        Ok(())
    }
    /// N398/N436: the host-side session counters.
    pub fn stats(&self) -> SessionStats {
        let mut stats = SessionStats {
            dropped_frames: self.dropped_frames,
            packages_sent: self.packages_sent,
            mirror_frames: self.mirror_frames,
            pacing_dropped: self.pacing_dropped,
            // N547: the device's reported applied-tile count rides along, and the
            // outstanding gap is derived in one place.
            device_tiles_received: self.device_tiles_received,
            ..self.stats
        };
        stats.tiles_missing = stats.tiles_sent.saturating_sub(stats.device_tiles_received);
        stats
    }

    /// N732: measure the outbound bitrate since the previous call using the
    /// virtual clock. Returns `None` when no virtual time has elapsed.
    pub fn measure_bitrate(&mut self) -> Option<u64> {
        let now = self.virtual_ms;
        let bytes = self.stats.bytes_sent;
        let (last_ms, last_bytes) = self.throughput_mark;
        self.throughput_mark = (now, bytes);
        let elapsed = now.saturating_sub(last_ms);
        if elapsed == 0 {
            return None;
        }
        let rate = bytes.saturating_sub(last_bytes).saturating_mul(1000) / elapsed;
        self.last_bitrate = Some(rate);
        Some(rate)
    }

    /// N732: adapt a dirty tracker to the measured outbound bitrate. `frame_bytes`
    /// is one full RGB565 frame; a link below ~1 FPS forces full frames for any
    /// meaningful dirty area.
    pub fn adapt_dirty_threshold(
        &mut self,
        tracker: &mut DirtyTracker,
        frame_bytes: u64,
    ) -> Option<f64> {
        let rate = self.measure_bitrate().or(self.last_bitrate)?;
        Some(tracker.adapt_to_bandwidth(rate, frame_bytes))
    }

    /// N750: one tuning pass for both streaming knobs. Measures the outbound
    /// rate once, adapts the active scheduler (if any) and the dirty tracker.
    pub fn tune_streaming(
        &mut self,
        tracker: &mut DirtyTracker,
        frame_bytes: u64,
    ) -> Option<AdaptiveTuning> {
        let bitrate = self.measure_bitrate().or(self.last_bitrate)?;
        let pacing = self
            .pacing
            .as_mut()
            .map(|scheduler| scheduler.adapt_to_bitrate(bitrate, frame_bytes));
        let dirty_threshold = Some(tracker.adapt_to_bandwidth(bitrate, frame_bytes));
        Some(AdaptiveTuning {
            bitrate,
            pacing,
            dirty_threshold,
        })
    }

    /// N758: plan every surface against one shared budget (from the active
    /// pacing scheduler, or unlimited when pacing is off). Each tile is billed at
    /// 1 KiB, matching `send_frame`, and surfaces take turns so a low-bandwidth
    /// link still advances each display.
    pub fn plan_shared_surface_tiles(&self) -> Option<mdc_schedule::SharedDecision> {
        let device = self.device.as_ref()?;
        let budget = self
            .pacing
            .as_ref()
            .map(|scheduler| scheduler.budget_bytes())
            .unwrap_or(0);
        Some(device.plan_shared_tiles(budget))
    }

    /// N767: plan with per-surface weights (missing entries mean 1). The first
    /// tile of every surface is still served when the budget allows.
    pub fn plan_shared_surface_tiles_weighted(
        &self,
        weights: &[u16],
    ) -> Option<mdc_schedule::SharedDecision> {
        let device = self.device.as_ref()?;
        let budget = self
            .pacing
            .as_ref()
            .map(|scheduler| scheduler.budget_bytes())
            .unwrap_or(0);
        Some(device.plan_shared_tiles_weighted(budget, weights))
    }

    /// N777: the device's advertised surface weights (`surface_weights` config),
    /// or `None` when unset/invalid.
    pub fn surface_weights(&self) -> Option<Vec<u16>> {
        self.config_cache
            .get("surface_weights")
            .and_then(|value| parse_surface_weights(value))
    }

    /// N777: plan with the device's configured weights (falling back to 1 each).
    pub fn plan_shared_surface_tiles_from_config(&self) -> Option<mdc_schedule::SharedDecision> {
        let weights = self.surface_weights().unwrap_or_default();
        self.plan_shared_surface_tiles_weighted(&weights)
    }

    /// N741: adapt an active frame-pacing scheduler to the measured outbound
    /// bitrate. Returns the new configuration (None without a scheduler/rate).
    pub fn adapt_frame_pacing(
        &mut self,
        frame_bytes: u64,
    ) -> Option<mdc_schedule::SchedulerConfig> {
        let rate = self.measure_bitrate().or(self.last_bitrate)?;
        let scheduler = self.pacing.as_mut()?;
        Some(scheduler.adapt_to_bitrate(rate, frame_bytes))
    }

    /// N547: reconcile the host's sent tiles against the device's reported count.
    pub fn tile_reconciliation(&self) -> TileReconciliation {
        reconcile_tiles(&self.stats(), self.device_tiles_received)
    }

    /// N557: if the device has not acknowledged more than `max_missing` tiles,
    /// set `needs_full_frame` so the next update resynchronises. Returns the
    /// reconciliation that was evaluated.
    pub fn enforce_tile_reconciliation(&mut self, max_missing: u64) -> TileReconciliation {
        let report = self.tile_reconciliation();
        if report.needs_full_frame(max_missing) {
            self.needs_full_frame = true;
        }
        report
    }

    /// N616: the factory default a device advertised for `key` in its
    /// `Capabilities.config.defaults`, if any.
    pub fn advertised_config_default(&self, key: &str) -> Option<&str> {
        self.device
            .as_ref()
            .and_then(|device| device.capabilities.config.default_for(key))
    }

    /// N586/N616: the `tile_missing_threshold`. A value read via `get_config`
    /// wins; otherwise the device's advertised factory default is used. `None`
    /// means neither is known.
    pub fn tile_missing_threshold(&self) -> Option<u64> {
        self.config(TILES_MISSING_THRESHOLD_CONFIG)
            .and_then(|value| value.trim().parse().ok())
            .or_else(|| {
                self.advertised_config_default(TILES_MISSING_THRESHOLD_CONFIG)
                    .and_then(|value| value.trim().parse().ok())
            })
    }

    /// N586: enforce the reconciled threshold from the device configuration. With
    /// no advertised key the check is a no-op (returns the report unchanged).
    pub fn enforce_tile_reconciliation_from_config(&mut self) -> TileReconciliation {
        match self.tile_missing_threshold() {
            Some(threshold) => self.enforce_tile_reconciliation(threshold),
            None => self.tile_reconciliation(),
        }
    }

    pub fn poll(&mut self) -> Result<ConnectionState, CoreError> {
        // SIM-R07: bound both the read and the parse per call. A peer that
        // floods data must not make one poll unbounded or delay cancellation.
        let mut read_bytes = 0usize;
        while read_bytes < MAX_POLL_BYTES && self.rx.len() < MAX_RX_BYTES {
            // 已经进缓冲区的包先处理。对端先写下 cancel 再关闭时，不能让关闭错误吞掉这些包。
            match self.transport.read() {
                Ok(Some(bytes)) => {
                    // N436: count inbound bytes.
                    read_bytes = read_bytes.saturating_add(bytes.len());
                    self.stats.bytes_received =
                        self.stats.bytes_received.saturating_add(bytes.len() as u64);
                    self.rx.extend(bytes)
                }
                Ok(None) => break,
                Err(error) => {
                    if self.rx.is_empty() {
                        let _ = self.mark_transport_failure();
                        return Err(CoreError::Transport(error.to_string()));
                    }
                    break;
                }
            }
        }
        let mut packets = 0usize;
        let mut parsed_bytes = 0usize;
        while packets < MAX_POLL_PACKETS && parsed_bytes < MAX_POLL_BYTES {
            match mdc_protocol::decode_stream(&self.rx, self.max_message) {
                mdc_protocol::StreamStep::Packet(packet, consumed) => {
                    self.rx.drain(..consumed);
                    parsed_bytes = parsed_bytes.saturating_add(consumed);
                    packets = packets.saturating_add(1);
                    self.handle_packet(packet)?;
                }
                mdc_protocol::StreamStep::NeedMore { discard } => {
                    if discard > 0 {
                        self.rx.drain(..discard.min(self.rx.len()));
                    }
                    break;
                }
                mdc_protocol::StreamStep::Rejected { error, discard } => {
                    if discard > 0 {
                        self.rx.drain(..discard.min(self.rx.len()));
                    }
                    return Err(CoreError::Protocol(error.to_string()));
                }
            }
        }
        Ok(self.state)
    }
    /// Advance virtual clock; drives heartbeat, ack timeout, and reconnect edges.
    pub fn tick(&mut self, virtual_ms: u64) -> Result<ConnectionState, CoreError> {
        self.virtual_ms = virtual_ms;
        // N473: held pointers may become long-presses on the virtual clock.
        while let Some(gesture) = self.gestures.tick(virtual_ms) {
            self.emit_gesture(gesture);
        }
        match self.state {
            ConnectionState::Connecting | ConnectionState::Handshaking => {
                if self.config.connect_timeout_ms > 0
                    && virtual_ms.saturating_sub(self.connect_started_ms)
                        >= self.config.connect_timeout_ms
                {
                    let _ = self.mark_transport_failure();
                    return Err(CoreError::ConnectTimeout);
                }
            }
            ConnectionState::Ready => {
                if self.config.ack_timeout_ms > 0
                    && self
                        .request_deadlines
                        .values()
                        .any(|deadline| *deadline <= virtual_ms)
                {
                    let _ = self.mark_transport_failure();
                    return Err(CoreError::AckTimeout);
                }
                if self.config.heartbeat_ms > 0
                    && virtual_ms.saturating_sub(self.last_heartbeat_ms) >= self.config.heartbeat_ms
                {
                    if self.heartbeat_outstanding {
                        self.missed_heartbeats = self.missed_heartbeats.saturating_add(1);
                        if self.missed_heartbeats >= 3 {
                            let _ = self.mark_transport_failure();
                            return Err(CoreError::HeartbeatMissed);
                        }
                    }
                    let id = self.alloc_request_id()?;
                    self.write_packet_with_id(MessageType::Ping, Vec::new(), id, false)?;
                    self.heartbeat_outstanding = true;
                    self.heartbeat_request = Some(id);
                    self.last_heartbeat_ms = virtual_ms;
                }
            }
            ConnectionState::Reconnecting => {
                if let Some(ready_at) = self.reconnect_ready_ms {
                    if virtual_ms >= ready_at {
                        self.prepare_transport_replace();
                    }
                }
            }
            _ => {}
        }
        Ok(self.state)
    }

    /// Close the dead link and wait for [`provide_transport`](Self::provide_transport).
    fn prepare_transport_replace(&mut self) {
        self.transport.close();
        self.rx.clear();
        self.pending_requests.clear();
        self.pending_control.clear();
        self.frame_sent_at.clear();
        self.frame_received_at.clear();
        self.pending_frames.clear();
        self.unsent = None;
        self.frame_tx = None;
        self.hello_request = None;
        self.finished_acks.clear();
        self.held_frames.clear();
        self.copy_budget = HostCopyBudget::new(self.copy_limit);
        self.mirror_assembler = None;
        self.mirror_last = None;
        self.mirror_last_frame_id = 0;
        self.config_cache.clear();
        self.gestures.reset();
        self.device = None;
        self.max_message = MAX_FRAME_BYTES;
        self.max_chunk = mdc_protocol::DEFAULT_MAX_CHUNK;
        self.reconnect_ready_ms = None;
        self.awaiting_transport = true;
        self.state = ConnectionState::Connecting;
    }

    /// Supply a new transport after reconnect backoff and resend HELLO.
    pub fn provide_transport(&mut self, transport: T) -> Result<(), CoreError> {
        if !self.awaiting_transport
            && self.state != ConnectionState::Disconnected
            && self.state != ConnectionState::Connecting
        {
            return Err(CoreError::Transport(
                "session is not awaiting a replacement transport".into(),
            ));
        }
        self.transport = transport;
        self.awaiting_transport = false;
        self.rx.clear();
        self.pending_requests.clear();
        self.pending_control.clear();
        self.frame_sent_at.clear();
        self.frame_received_at.clear();
        self.pending_frames.clear();
        self.unsent = None;
        self.finished_acks.clear();
        self.held_frames.clear();
        self.copy_budget = HostCopyBudget::new(self.copy_limit);
        self.request_floor = self.next_request_id;
        self.device = None;
        self.needs_full_frame = true;
        self.missed_heartbeats = 0;
        self.heartbeat_outstanding = false;
        self.state = ConnectionState::Disconnected;
        self.connect()
    }

    /// Cancel the current Session and attach a different endpoint (I03).
    /// Old ACKs are dropped; after handshake the host must send a full frame
    /// before any Tile (`needs_full_frame`).
    pub fn switch_transport(&mut self, transport: T) -> Result<(), CoreError> {
        let reconnect = self.reconnect_enabled;
        self.transport.close();
        self.held_frames.clear();
        self.clear_link_state();
        self.request_floor = self.next_request_id;
        self.device = None;
        self.gestures.reset();
        self.awaiting_transport = false;
        self.needs_full_frame = true;
        self.reconnect_ready_ms = None;
        self.state = ConnectionState::Disconnected;
        self.reconnect_enabled = reconnect;
        self.transport = transport;
        self.connect()
    }
    pub fn disconnect(&mut self) {
        self.reconnect_enabled = false;
        self.awaiting_transport = false;
        self.state = ConnectionState::Closing;
        self.transport.close();
        self.state = ConnectionState::Disconnected;
        self.device = None;
        self.held_frames.clear();
        self.clear_link_state();
    }
    pub fn enable_reconnect(&mut self, enabled: bool) {
        self.reconnect_enabled = enabled;
        self.config.reconnect = enabled;
    }
    pub fn mark_transport_failure(&mut self) -> Option<u64> {
        if !self.reconnect_enabled {
            self.state = ConnectionState::Disconnected;
            self.transport.close();
            self.device = None;
            self.held_frames.clear();
            self.clear_link_state();
            return None;
        }
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        self.state = ConnectionState::Reconnecting;
        self.device = None;
        let delay = self.reconnect_policy.delay_ms(self.reconnect_attempt, 0)?;
        self.reconnect_ready_ms = Some(self.virtual_ms.saturating_add(delay));
        Some(delay)
    }
    pub fn reconnect_attempt(&self) -> u32 {
        self.reconnect_attempt
    }
    pub fn pending_request_count(&self) -> usize {
        self.pending_requests.len()
    }
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }
    pub fn copy_outstanding(&self) -> usize {
        self.copy_budget.outstanding()
    }
    pub fn copy_peak(&self) -> usize {
        self.copy_budget.peak()
    }
    /// Test and board profiles can tighten the host copy limit. Outstanding bytes are cleared.
    pub fn set_copy_limit(&mut self, limit: usize) {
        self.copy_limit = limit;
        self.held_frames.clear();
        self.copy_budget = HostCopyBudget::new(limit);
    }
    pub fn displayed_timings(&self) -> &[DisplayTiming] {
        &self.displayed_timings
    }
    /// SIM-R09: take a cheap snapshot of every growing container. Locks the
    /// manager only for the event/device counts.
    pub fn container_counts(&self) -> ContainerCounts {
        let (manager_events, devices) = self
            .manager
            .as_ref()
            .and_then(|manager| manager.lock().ok())
            .map(|manager| (manager.event_count(), manager.devices().count()))
            .unwrap_or((0, 0));
        ContainerCounts {
            rx_bytes: self.rx.len(),
            pending_requests: self.pending_requests.len(),
            pending_control: self.pending_control.len(),
            pending_frames: self.pending_frames.len(),
            request_deadlines: self.request_deadlines.len(),
            finished_acks: self.finished_acks.len(),
            displayed_timings: self.displayed_timings.len(),
            held_frames: self.held_frames.len(),
            diagnostics: self.diagnostics.len(),
            config_cache: self.config_cache.len(),
            pending_configs: self.pending_configs.len(),
            active_pointers: self.gestures.pointers.len(),
            mirror_inbox_frames: usize::from(self.mirror_inbox.is_some()),
            mirror_base_frames: usize::from(self.mirror_last.is_some()),
            frame_transactions: usize::from(self.frame_tx.is_some()),
            manager_events,
            devices,
        }
    }
    /// SIM-R07: remember a displayed ACK in a fixed-size window. An evicted id
    /// is harmless: its request and frame were already released, so a replay
    /// cannot launch anything again.
    fn record_finished_ack(&mut self, id: u32) {
        if self.finished_acks.len() >= MAX_FINISHED_ACKS {
            self.finished_acks.pop_first();
        }
        self.finished_acks.insert(id);
    }
    fn release_request(&mut self, id: u32) {
        self.pending_requests.remove(&id);
        self.request_deadlines.remove(&id);
        self.pending_control.remove(&id);
    }
    fn release_held(&mut self, surface_id: &str, frame_id: u64) {
        if let Some(pixels) = self.held_frames.remove(&(surface_id.to_string(), frame_id)) {
            self.copy_budget.release_frame(pixels);
        }
    }
    fn note_displayed(&mut self, id: u32) {
        let Some(sent) = self.frame_sent_at.remove(&id) else {
            return;
        };
        let now = std::time::Instant::now();
        let received = self.frame_received_at.remove(&id).unwrap_or(now);
        let transport = received.saturating_duration_since(sent);
        let e2e = now.saturating_duration_since(sent);
        let render = e2e.saturating_sub(transport);
        if self.displayed_timings.len() >= MAX_DISPLAY_TIMINGS {
            self.displayed_timings.remove(0);
        }
        self.displayed_timings.push(DisplayTiming {
            transport_us: transport.as_micros(),
            render_us: render.as_micros(),
            e2e_us: e2e.as_micros(),
        });
    }
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }
    pub fn resume_write(&mut self) -> Result<(), CoreError> {
        if let Some(rest) = self.unsent.take() {
            self.write_bytes(&rest)?;
        }
        if self.frame_tx.is_some() {
            self.drive_frame_tx()?;
        }
        Ok(())
    }

    fn drive_frame_tx(&mut self) -> Result<(), CoreError> {
        let Some(mut tx) = self.frame_tx.take() else {
            return Ok(());
        };
        loop {
            match tx.stage {
                FrameTxStage::Begin => {
                    let begin = encode_frame_begin(
                        &tx.surface_id,
                        tx.frame_id,
                        tx.width,
                        tx.height,
                        tx.pixels.len() as u32,
                    )
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                    // Mark the stage before writing: a short write leaves the
                    // BEGIN tail in `unsent`, so resume must continue with chunks.
                    tx.stage = FrameTxStage::Chunks;
                    tx.next_offset = 0;
                    match self.write_packet_with_id(MessageType::Frame, begin, tx.request_id, true)
                    {
                        Ok(()) => {}
                        Err(CoreError::ShortWrite) => {
                            self.frame_tx = Some(tx);
                            return Err(CoreError::ShortWrite);
                        }
                        Err(error) => {
                            self.pending_requests.remove(&tx.request_id);
                            self.pending_frames.remove(&tx.request_id);
                            return Err(error);
                        }
                    }
                }
                FrameTxStage::Chunks => {
                    if tx.next_offset >= tx.pixels.len() {
                        tx.stage = FrameTxStage::Commit;
                        continue;
                    }
                    let end = (tx.next_offset + tx.max_chunk).min(tx.pixels.len());
                    let chunk = encode_frame_chunk(
                        tx.frame_id,
                        tx.next_offset as u32,
                        &tx.pixels[tx.next_offset..end],
                    );
                    // Advance before writing so a short chunk is not re-sent.
                    tx.next_offset = end;
                    match self.write_packet_with_id(MessageType::Frame, chunk, tx.request_id, false)
                    {
                        Ok(()) => {}
                        Err(CoreError::ShortWrite) => {
                            self.frame_tx = Some(tx);
                            return Err(CoreError::ShortWrite);
                        }
                        Err(error) => {
                            self.pending_requests.remove(&tx.request_id);
                            self.pending_frames.remove(&tx.request_id);
                            return Err(error);
                        }
                    }
                }
                FrameTxStage::Commit => {
                    let commit = encode_frame_commit(tx.frame_id);
                    tx.stage = FrameTxStage::Done;
                    match self.write_packet_with_id(
                        MessageType::Frame,
                        commit,
                        tx.request_id,
                        false,
                    ) {
                        Ok(()) => return Ok(()),
                        Err(CoreError::ShortWrite) => {
                            self.frame_tx = Some(tx);
                            return Err(CoreError::ShortWrite);
                        }
                        Err(error) => {
                            self.pending_requests.remove(&tx.request_id);
                            self.pending_frames.remove(&tx.request_id);
                            return Err(error);
                        }
                    }
                }
                FrameTxStage::Done => return Ok(()),
            }
        }
    }
    pub fn send_frame(&mut self, frame: Frame) -> Result<u32, CoreError> {
        self.stats.frames_sent = self.stats.frames_sent.saturating_add(1);
        if self.unsent.is_some() {
            return Err(CoreError::ShortWrite);
        }
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        {
            let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
            device.validate_frame(&frame)?;
        }
        // N329: opt-in pacing refuses a frame that is too soon or over budget.
        if self.pacing.is_some() {
            let cost_tile = 1024u32;
            let count = frame.bytes.len().div_ceil(cost_tile as usize) as u32;
            let tiles: Vec<mdc_schedule::TileCost> = (0..count)
                .map(|i| mdc_schedule::TileCost::new(i, cost_tile, false))
                .collect();
            let now = self.virtual_ms;
            let decision = self
                .pacing
                .as_ref()
                .map(|scheduler| scheduler.plan(now, &tiles));
            match decision {
                Some(mdc_schedule::FrameDecision::Defer { .. }) => {
                    return Err(CoreError::Throttled);
                }
                Some(mdc_schedule::FrameDecision::Send { deferred, .. })
                    if !deferred.is_empty() =>
                {
                    self.pacing_dropped = self.pacing_dropped.saturating_add(1);
                    return Err(CoreError::Throttled);
                }
                Some(mdc_schedule::FrameDecision::Send { .. }) => {
                    if let Some(scheduler) = self.pacing.as_mut() {
                        scheduler.commit(now);
                    }
                }
                None => {}
            }
        }
        let device_max_chunk = self
            .device
            .as_ref()
            .ok_or(CoreError::NotReady)?
            .capabilities
            .max_chunk as usize;
        let max_chunk = self.max_chunk.min(device_max_chunk).max(1);
        let surface_id = frame.surface_id.clone();
        let width = frame.width;
        let height = frame.height;
        let pixels = frame.bytes.clone();
        let pixel_len = pixels.len();
        self.copy_budget
            .hold_frame(pixel_len)
            .map_err(|_| CoreError::CopyBudget)?;
        let queued = (|| {
            let device = self.device.as_mut().ok_or(CoreError::NotReady)?;
            let surface = device
                .surfaces
                .iter_mut()
                .find(|s| s.info.id == surface_id)
                .ok_or(CoreError::Dimensions)?;
            let old_pending = surface.pending_frame_id().zip(surface.pending_pixels());
            let before = surface.replaced_pending();
            let id = surface.enqueue_frame(frame)?;
            Ok::<_, CoreError>((
                id,
                surface.replaced_pending().saturating_sub(before),
                old_pending,
            ))
        })();
        let (frame_id, replaced, old_pending) = match queued {
            Ok(value) => value,
            Err(error) => {
                self.copy_budget.release_frame(pixel_len);
                return Err(error);
            }
        };
        if replaced > 0 {
            if let Some((old_id, old_len)) = old_pending {
                self.held_frames.remove(&(surface_id.clone(), old_id));
                self.copy_budget.release_frame(old_len);
            }
        }
        self.held_frames
            .insert((surface_id.clone(), frame_id), pixel_len);
        self.dropped_frames = self.dropped_frames.saturating_add(replaced);
        let send_now = self
            .device
            .as_ref()
            .and_then(|device| device.surfaces.iter().find(|s| s.info.id == surface_id))
            .map(|surface| surface.in_flight() == Some(frame_id))
            .unwrap_or(false);
        if !send_now {
            // 0 表示这一帧只占着待发槽，还没有请求号。在途帧 ACK 之后才会写到链路上。
            return Ok(0);
        }
        self.write_frame_bytes(&surface_id, frame_id, width, height, &pixels, max_chunk)
    }
    fn write_frame_bytes(
        &mut self,
        surface_id: &str,
        frame_id: u64,
        width: u16,
        height: u16,
        pixels: &[u8],
        max_chunk: usize,
    ) -> Result<u32, CoreError> {
        let legacy = encode_frame_payload(surface_id, frame_id, width, height, pixels)
            .map_err(|e| CoreError::Protocol(e.to_string()))?;
        let request_id = self.alloc_request_id()?;
        let write_result = if legacy.len() <= max_chunk {
            self.write_packet_with_id(MessageType::Frame, legacy, request_id, true)
        } else {
            self.pending_frames
                .insert(request_id, (surface_id.to_string(), frame_id));
            self.frame_sent_at
                .insert(request_id, std::time::Instant::now());
            self.pending_requests.insert(request_id);
            if self.config.ack_timeout_ms > 0 {
                self.request_deadlines.insert(
                    request_id,
                    self.virtual_ms.saturating_add(self.config.ack_timeout_ms),
                );
            }
            self.frame_tx = Some(FrameTransaction {
                surface_id: surface_id.to_string(),
                frame_id,
                width,
                height,
                pixels: pixels.to_vec(),
                max_chunk,
                request_id,
                stage: FrameTxStage::Begin,
                next_offset: 0,
            });
            self.drive_frame_tx()
        };
        if matches!(write_result, Ok(()) | Err(CoreError::ShortWrite)) {
            // Legacy path: chunked path already recorded the transaction.
            self.pending_frames
                .entry(request_id)
                .or_insert_with(|| (surface_id.to_string(), frame_id));
            self.frame_sent_at
                .entry(request_id)
                .or_insert_with(std::time::Instant::now);
        }
        write_result?;
        Ok(request_id)
    }
    fn launch_pending_frame(&mut self, surface_id: &str, frame_id: u64) -> Result<(), CoreError> {
        let follow = {
            let device = self.device.as_mut().ok_or(CoreError::NotReady)?;
            let surface = device
                .surfaces
                .iter_mut()
                .find(|surface| surface.info.id == surface_id);
            match surface {
                Some(surface) => surface.ack(frame_id),
                None => None,
            }
        };
        let Some((next_id, frame)) = follow else {
            return Ok(());
        };
        let max_chunk = self.negotiated_max_chunk();
        self.write_frame_bytes(
            &frame.surface_id,
            next_id,
            frame.width,
            frame.height,
            &frame.bytes,
            max_chunk,
        )?;
        Ok(())
    }
    fn negotiated_max_chunk(&self) -> usize {
        let device_max = self
            .device
            .as_ref()
            .map(|device| device.capabilities.max_chunk as usize)
            .unwrap_or(self.max_chunk);
        self.max_chunk.min(device_max).max(1)
    }
    pub fn send_tile(&mut self, tile: Tile) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        if self.needs_full_frame {
            return Err(CoreError::NeedFullFrame);
        }
        let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
        if !device.capabilities.tile {
            return Err(CoreError::Unsupported);
        }
        device.send_tile(&tile)?;
        if device
            .surfaces
            .iter()
            .find(|surface| surface.info.id == tile.surface_id)
            .map(|surface| surface.rejects_tile(tile.base_frame_id))
            .unwrap_or(false)
        {
            return Err(CoreError::QueueFull);
        }
        let payload = encode_tile_payload(
            &tile.surface_id,
            tile.base_frame_id,
            tile.x,
            tile.y,
            tile.width,
            tile.height,
            &tile.bytes,
        )
        .map_err(|e| CoreError::Protocol(e.to_string()))?;
        let request_id = self.write_packet(MessageType::Tile, payload)?;
        // N506: count the partial update and its payload bytes.
        self.stats.tiles_sent = self.stats.tiles_sent.saturating_add(1);
        self.stats.tile_bytes_sent = self
            .stats
            .tile_bytes_sent
            .saturating_add(tile.bytes.len() as u64);
        self.pending_frames
            .insert(request_id, (tile.surface_id, tile.base_frame_id));
        Ok(request_id)
    }
    /// N516: send only the tiles covering `rects` of a tightly packed RGB565
    /// frame, and return the number of tiles sent. Duplicate/overlapping rects
    /// collapse into one tile.
    #[allow(clippy::too_many_arguments)]
    pub fn send_dirty_tiles(
        &mut self,
        surface_id: &str,
        base_frame_id: u64,
        frame: &[u8],
        frame_size: (u16, u16),
        tile_size: u16,
        rects: &[DirtyRect],
    ) -> Result<usize, CoreError> {
        let tiles = plan_dirty_tiles(frame_size, tile_size, rects);
        for tile in &tiles {
            let (x, y, w, h) = *tile;
            self.send_tile(Tile {
                surface_id: surface_id.to_string(),
                base_frame_id,
                x,
                y,
                width: w,
                height: h,
                bytes: extract_tile(frame, frame_size, *tile),
            })?;
        }
        Ok(tiles.len())
    }

    /// N526: send the accumulated dirty regions of `frame`, or a full frame when
    /// the tracker decides the partial update is not worth it. The frame must be
    /// tightly packed RGB565 at `tracker`'s surface size.
    pub fn send_dirty_update(
        &mut self,
        surface_id: &str,
        base_frame_id: u64,
        frame: &[u8],
        tracker: &mut DirtyTracker,
    ) -> Result<UpdateDecision, CoreError> {
        let decision = tracker.decide();
        let tiles = tracker.tiles();
        let (surface, tile_size) = (tracker.surface, tracker.tile_size);
        tracker.take();
        match decision {
            UpdateDecision::Full => {
                self.send_frame(Frame {
                    surface_id: surface_id.to_string(),
                    width: surface.0,
                    height: surface.1,
                    bytes: frame.to_vec(),
                })?;
                Ok(UpdateDecision::Full)
            }
            UpdateDecision::Partial(_) => {
                let sent = self.send_dirty_tiles(
                    surface_id,
                    base_frame_id,
                    frame,
                    surface,
                    tile_size,
                    &tiles,
                )?;
                Ok(UpdateDecision::Partial(sent))
            }
        }
    }

    /// Send an OTA control command; gated by device `ota` capability.
    pub fn send_ota(&mut self, command: OtaCommand) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
        device.ota()?;
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        let request_id = self.write_packet(MessageType::Ota, payload)?;
        // N419: checkpoint the phase so a driver can resume after a reconnect.
        self.ota_phase = match command.action.as_str() {
            "begin" => 1,
            "chunk" => 2,
            "confirm" => 3,
            "abort" => 0,
            _ => self.ota_phase,
        };
        Ok(request_id)
    }

    /// N473: push a recognised gesture to the manager (if attached).
    fn emit_gesture(&mut self, gesture: Gesture) {
        let device_id = self
            .device
            .as_ref()
            .map(|device| device.capabilities.device_id.clone())
            .unwrap_or_default();
        if let Some(manager) = &self.manager {
            if let Ok(mut manager) = manager.lock() {
                manager.push_gesture(device_id, gesture);
            }
        }
    }

    /// N488: tune gesture recognition (tap/long-press/drag thresholds).
    pub fn set_gesture_config(&mut self, config: GestureConfig) {
        self.gestures.set_config(config);
    }

    /// N488: the current gesture thresholds.
    pub fn gesture_config(&self) -> GestureConfig {
        self.gestures.config()
    }

    /// N419: the highest OTA phase sent (0 none, 1 begin, 2 chunk, 3 confirm).
    pub fn ota_phase(&self) -> u8 {
        self.ota_phase
    }

    /// N728: the OTA commands still required after a reconnect, derived from the
    /// checkpoint. The device keeps its OTA state across a transport reconnect,
    /// so begin/chunk that already reached it are not resent; phase 3 means the
    /// update is already confirmed and the plan is empty.
    pub fn ota_resume_commands(
        &self,
        version: &str,
        size: Option<u64>,
        sha256: Option<String>,
    ) -> Vec<mdc_protocol::OtaCommand> {
        let begin = mdc_protocol::OtaCommand {
            action: "begin".into(),
            version: Some(version.to_string()),
            size,
            sha256: None,
            offset: None,
            len: None,
        };
        let chunk = mdc_protocol::OtaCommand {
            action: "chunk".into(),
            version: None,
            size: None,
            sha256,
            offset: None,
            len: None,
        };
        let confirm = mdc_protocol::OtaCommand {
            action: "confirm".into(),
            version: None,
            size: None,
            sha256: None,
            offset: None,
            len: None,
        };
        match self.ota_phase {
            0 => vec![begin, chunk, confirm],
            1 => vec![chunk, confirm],
            2 => vec![confirm],
            _ => Vec::new(),
        }
    }

    /// N737: the chunk ranges of an image (`chunk_size` clamped to at least 1),
    /// with the final range shortened to the image length.
    pub fn ota_chunk_ranges(total_bytes: u64, chunk_size: u32) -> Vec<(u32, u32)> {
        let total = total_bytes.min(u64::from(u32::MAX)) as u32;
        let chunk = chunk_size.max(1);
        let mut out = Vec::new();
        let mut offset = 0u32;
        while offset < total {
            let len = chunk.min(total - offset);
            out.push((offset, len));
            offset = offset.saturating_add(len);
        }
        out
    }

    /// N737: the chunk ranges not fully covered by `acknowledged`.
    pub fn ota_missing_chunks(
        total_bytes: u64,
        chunk_size: u32,
        acknowledged: &[(u32, u32)],
    ) -> Vec<(u32, u32)> {
        Self::ota_chunk_ranges(total_bytes, chunk_size)
            .into_iter()
            .filter(|(offset, len)| {
                !acknowledged.iter().any(|(ack_offset, ack_len)| {
                    *ack_offset <= *offset
                        && u64::from(*ack_offset) + u64::from(*ack_len)
                            >= u64::from(*offset) + u64::from(*len)
                })
            })
            .collect()
    }

    /// N737: like [`Self::ota_resume_commands`] but chunk-aware. The caller
    /// supplies the image size, chunk size and the ranges the device confirmed;
    /// only missing chunks are emitted (begin is skipped once phase >= 1).
    pub fn ota_resume_commands_chunked(
        &self,
        version: &str,
        total_bytes: u64,
        chunk_size: u32,
        acknowledged: &[(u32, u32)],
    ) -> Vec<mdc_protocol::OtaCommand> {
        if self.ota_phase >= 3 {
            return Vec::new();
        }
        let mut commands = Vec::new();
        if self.ota_phase == 0 {
            commands.push(mdc_protocol::OtaCommand {
                action: "begin".into(),
                version: Some(version.to_string()),
                size: Some(total_bytes),
                sha256: None,
                offset: None,
                len: None,
            });
        }
        for (offset, len) in Self::ota_missing_chunks(total_bytes, chunk_size, acknowledged) {
            commands.push(mdc_protocol::OtaCommand {
                action: "chunk".into(),
                version: None,
                size: None,
                sha256: None,
                offset: Some(offset),
                len: Some(len),
            });
        }
        commands.push(mdc_protocol::OtaCommand {
            action: "confirm".into(),
            version: None,
            size: None,
            sha256: None,
            offset: None,
            len: None,
        });
        commands
    }

    /// N419: clear the OTA checkpoint (a fresh update or an abort).
    pub fn reset_ota(&mut self) {
        self.ota_phase = 0;
    }
    /// 在同一条连接上再发 HELLO，用对端当前 Capabilities 覆盖缓存。
    /// OTA 失败后用它读回固件版本；会重置 surface 队列，调用前不要留在途帧。
    pub fn refresh_capabilities(&mut self) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let hello = encode_control(&Hello {
            major: VERSION.major,
            minor: VERSION.minor,
            max_message: MAX_FRAME_BYTES as u32,
            max_chunk: mdc_protocol::DEFAULT_MAX_CHUNK as u32,
            boot_id: self.host_boot_id,
        })
        .map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.write_packet(MessageType::Hello, hello)
    }
    /// v1 raw Scene payload is not accepted; use [`Session::set_scene`] with a
    /// structured node. Kept for ABI stability.
    pub fn send_scene(&mut self, _payload: &[u8]) -> Result<u32, CoreError> {
        let _ = (self.state, _payload);
        Err(CoreError::Unsupported)
    }
    /// N400: send a pointer/touch event to the device, gated on its `touch`
    /// capability.
    pub fn send_input(&mut self, event: mdc_protocol::InputEvent) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::Unsupported);
        }
        let touch = self
            .device
            .as_ref()
            .is_some_and(|device| device.capabilities.touch);
        if !touch {
            return Err(CoreError::Unsupported);
        }
        let payload =
            mdc_protocol::encode_control(&event).map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.write_packet(MessageType::Input, payload)
    }
    /// N54: set a vector scene, gated on the device's advertised scene limits.
    pub fn set_scene(&mut self, scene: mdc_protocol::SceneNode) -> Result<u32, CoreError> {
        let caps = self.scene_gate()?;
        if scene.count() > caps.max_nodes
            || scene.depth() > caps.max_depth
            || scene.anim_keys() > caps.max_anim_keys
        {
            return Err(CoreError::TooLarge);
        }
        let command = mdc_protocol::SceneCommand {
            action: "set".into(),
            scene: Some(scene),
        };
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.write_packet(MessageType::Scene, payload)
    }
    /// N54: clear the vector scene.
    pub fn clear_scene(&mut self) -> Result<u32, CoreError> {
        self.scene_gate()?;
        let command = mdc_protocol::SceneCommand {
            action: "clear".into(),
            scene: None,
        };
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.write_packet(MessageType::Scene, payload)
    }
    fn scene_gate(&self) -> Result<mdc_protocol::SceneCapability, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
        if device.capabilities.scene.is_disabled() {
            return Err(CoreError::Unsupported);
        }
        Ok(device.capabilities.scene.clone())
    }
    /// Form B: true when the negotiated device advertises a usable mirror mode.
    pub fn supports_mirror(&self) -> bool {
        self.device
            .as_ref()
            .map(|device| device.capabilities.mirror.supports())
            .unwrap_or(false)
    }
    /// Form B: ask the device to start streaming mirrored frames.
    /// Rejected with `Unsupported` when the device did not advertise mirror.
    pub fn start_mirror(
        &mut self,
        surface_id: &str,
        mode: &str,
        max_fps: u16,
    ) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let command = {
            let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
            negotiate_mirror(&device.capabilities, surface_id, mode, max_fps, "RGB565")
                .ok_or(CoreError::Unsupported)?
        };
        // N264: remember the negotiated mode so dirty-region frames are patched.
        self.mirror_mode = command.mode.clone();
        self.mirror_last = None;
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.write_packet(MessageType::MirrorCtrl, payload)
    }
    /// Form B: ask the device to stop streaming mirrored frames.
    pub fn stop_mirror(&mut self, surface_id: &str) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
        if !device.capabilities.mirror.supports() {
            return Err(CoreError::Unsupported);
        }
        let command = MirrorCommand {
            action: "stop".into(),
            surface_id: surface_id.to_string(),
            mode: "full-frame".into(),
            max_fps: 0,
            format: "RGB565".into(),
        };
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.mirror_mode.clear();
        self.mirror_last = None;
        self.write_packet(MessageType::MirrorCtrl, payload)
    }
    /// Form B: upload a compiled runtime package to the device (N23). The bytes
    /// are chunked into PACKAGE begin/chunk/commit messages. Rejected with
    /// `Unsupported` when the device advertised no renderer capability.
    pub fn install_package(&mut self, bytes: &[u8]) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let renderer_ready = self
            .device
            .as_ref()
            .map(|device| device.capabilities.renderer.is_some())
            .unwrap_or(false);
        if !renderer_ready {
            return Err(CoreError::Unsupported);
        }
        if bytes.is_empty() {
            return Err(CoreError::Protocol("empty package".into()));
        }
        const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;
        if bytes.len() > MAX_PACKAGE_BYTES {
            return Err(CoreError::TooLarge);
        }
        // Check the window before BEGIN: bailing out after streaming chunks
        // would leave a half-written package in the device's staging buffer.
        if self.pending_control.len() >= MAX_CONTROL_IN_FLIGHT {
            return Err(CoreError::ControlQueueFull);
        }
        let request_id = self.alloc_request_id()?;
        if let Err(error) = self.send_package_bytes(bytes, request_id) {
            // A package is atomic. A short write or I/O error leaves an unknown
            // written prefix, so abandon the link instead of retrying chunks
            // against a device that may already hold part of the package.
            self.unsent = None;
            let _ = self.mark_transport_failure();
            return Err(error);
        }
        self.pending_control.insert(request_id);
        self.last_send_ms = self.virtual_ms;
        self.packages_sent = self.packages_sent.saturating_add(1);
        Ok(request_id)
    }

    fn send_package_bytes(&mut self, bytes: &[u8], request_id: u32) -> Result<(), CoreError> {
        let total = u32::try_from(bytes.len()).map_err(|_| CoreError::TooLarge)?;
        let checksum = package_checksum(bytes);
        // SIM-R03: BEGIN/chunks are not individual control requests. Only the
        // final COMMIT occupies the bounded control window; the device ACKs the
        // package after commit, not every chunk.
        self.write_packet_with_id(
            MessageType::Package,
            encode_package_begin(total, checksum),
            request_id,
            false,
        )?;
        let chunk_size = self
            .negotiated_max_chunk()
            .min(self.max_message.saturating_sub(32))
            .max(1);
        let mut offset: u32 = 0;
        for chunk in bytes.chunks(chunk_size) {
            self.write_packet_with_id(
                MessageType::Package,
                encode_package_chunk(offset, chunk),
                request_id,
                false,
            )?;
            offset = offset
                .checked_add(chunk.len() as u32)
                .ok_or(CoreError::TooLarge)?;
        }
        self.write_packet_with_id(
            MessageType::Package,
            encode_package_commit(),
            request_id,
            true,
        )
    }
    pub fn packages_sent(&self) -> u64 {
        self.packages_sent
    }

    /// N40: set a configuration value. Refused when the device did not advertise
    /// the key or the value is too large.
    pub fn set_config(&mut self, key: &str, value: &str) -> Result<u32, CoreError> {
        self.config_gate(key)?;
        if value.len() > MAX_CONFIG_VALUE_BYTES {
            return Err(CoreError::TooLarge);
        }
        let command = ConfigCommand {
            action: "set".into(),
            key: key.to_string(),
            value: Some(value.to_string()),
        };
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        let request_id = self.write_packet(MessageType::Config, payload)?;
        self.pending_configs.insert(key.to_string(), request_id);
        Ok(request_id)
    }

    /// N40: request a configuration value; the reply lands in `config`.
    pub fn get_config(&mut self, key: &str) -> Result<u32, CoreError> {
        self.config_gate(key)?;
        let command = ConfigCommand {
            action: "get".into(),
            key: key.to_string(),
            value: None,
        };
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        let request_id = self.write_packet(MessageType::Config, payload)?;
        self.pending_configs.insert(key.to_string(), request_id);
        Ok(request_id)
    }

    /// N40: a configuration value learned from the device, if any.
    pub fn config(&self, key: &str) -> Option<&str> {
        self.config_cache.get(key).map(String::as_str)
    }

    pub fn config_values(&self) -> &BTreeMap<String, String> {
        &self.config_cache
    }

    /// N73: play a baked-atlas clip on the device (via the `animation` setting).
    pub fn set_animation(&mut self, clip: &str) -> Result<u32, CoreError> {
        self.set_config("animation", clip)
    }

    /// N73: stop the device-side animation.
    pub fn stop_animation(&mut self) -> Result<u32, CoreError> {
        self.set_config("animation", "off")
    }

    /// N73: the clip the device reported as active, if any.
    pub fn active_animation(&self) -> Option<&str> {
        self.config("animation").filter(|value| *value != "off")
    }

    /// N833: blend up to four clips (`"idle:700,wave:300"`). The device replaces
    /// the single clip; the cache follows the accepted reply.
    pub fn set_animation_blend(&mut self, blend: &str) -> Result<u32, CoreError> {
        self.set_config("animation_blend", blend)
    }

    /// N833: stop blending and leave the device with no running clip.
    pub fn stop_animation_blend(&mut self) -> Result<u32, CoreError> {
        self.set_config("animation_blend", "off")
    }

    /// N833: the blend string the device reported, if any (`off` is hidden).
    pub fn active_animation_blend(&self) -> Option<&str> {
        self.config("animation_blend")
            .filter(|value| *value != "off")
    }

    /// N83: stop any running animation so the device presents its scene (if one
    /// was set). The scene itself is unchanged.
    pub fn show_scene(&mut self) -> Result<u32, CoreError> {
        self.stop_animation()
    }

    fn config_gate(&self, key: &str) -> Result<(), CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
        if !device.capabilities.config.supports(key) {
            return Err(CoreError::Unsupported);
        }
        Ok(())
    }

    fn handle_config(&mut self, packet: &Packet) -> Result<(), CoreError> {
        if self.state != ConnectionState::Ready {
            return Ok(());
        }
        let reply: ConfigReply = match mdc_protocol::decode_control(&packet.payload) {
            Ok(reply) => reply,
            Err(_) => return Ok(()),
        };
        // SIM-R04: the device replies with request_id 0, so bind the reply to
        // the pending key instead of trusting any Config packet.
        let Some(request_id) = self.pending_configs.remove(&reply.key) else {
            return Ok(());
        };
        self.release_request(request_id);
        if reply.ok {
            if let Some(value) = reply.value {
                // N833: a successful animation reply means the device replaced
                // the other selection; mirror that in the cache. A rejected set
                // produces no reply here, so the old value survives.
                if reply.key == "animation" {
                    self.config_cache
                        .insert("animation_blend".to_string(), "off".to_string());
                } else if reply.key == "animation_blend" {
                    self.config_cache
                        .insert("animation".to_string(), "off".to_string());
                }
                self.config_cache.insert(reply.key, value);
            }
        }
        Ok(())
    }
    /// Take the latest complete mirrored frame, if any. The inbox holds at most
    /// one frame; a newer frame replaces an unread one (latest-frame-wins).
    pub fn take_mirror_frame(&mut self) -> Option<MirrorFrame> {
        self.mirror_inbox.take()
    }
    /// N559: the negotiated mirror mode (`"full-frame"` | `"dirty-region"`; empty
    /// before the device confirms a `MIRRORCTRL start`).
    pub fn mirror_mode(&self) -> &str {
        &self.mirror_mode
    }
    /// N24: mirror counters for a host/inspector without draining the inbox.
    pub fn mirror_stats(&self) -> MirrorStats {
        MirrorStats {
            frames_received: self.mirror_frames,
            frames_dropped: self.mirror_dropped,
            last_frame_id: self.mirror_last_frame_id,
        }
    }
    pub fn mirror_frames_received(&self) -> u64 {
        self.mirror_frames
    }
    pub fn mirror_frames_dropped(&self) -> u64 {
        self.mirror_dropped
    }
    fn handle_mirror_frame(&mut self, packet: Packet) -> Result<(), CoreError> {
        if self.state != ConnectionState::Ready {
            return Ok(());
        }
        let decoded = match decode_frame_payload(&packet.payload) {
            Ok(value) => value,
            Err(_) => {
                self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                return Ok(());
            }
        };
        match decoded {
            FramePayload::Legacy {
                surface_id,
                frame_id,
                width,
                height,
                bytes,
            } => {
                self.store_mirror_frame(surface_id, frame_id, width, height, bytes)?;
            }
            FramePayload::Begin {
                surface_id,
                frame_id,
                width,
                height,
                total_bytes,
            } => {
                let max_chunk = self.negotiated_max_chunk();
                match FrameAssembler::begin(frame_id, total_bytes as usize, max_chunk) {
                    Ok(assembler) => {
                        self.mirror_assembler = Some((surface_id, width, height, assembler));
                    }
                    Err(_) => {
                        self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                        self.mirror_assembler = None;
                    }
                }
            }
            FramePayload::Chunk {
                frame_id,
                offset,
                data,
            } => {
                let ok = match self.mirror_assembler.as_mut() {
                    Some((_, _, _, assembler)) if assembler.frame_id == frame_id => {
                        assembler.chunk(offset as usize, &data).is_ok()
                    }
                    _ => false,
                };
                if !ok {
                    self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                }
            }
            FramePayload::Commit { frame_id } => {
                let taken = match self.mirror_assembler.take() {
                    Some((surface_id, width, height, assembler))
                        if assembler.frame_id == frame_id =>
                    {
                        Some((surface_id, width, height, assembler))
                    }
                    _ => None,
                };
                match taken {
                    Some((surface_id, width, height, assembler)) => {
                        let expected = assembler.expected_len;
                        let (ranges, staging) = assembler.into_ranges();
                        let complete = ranges.len() == 1 && ranges[0] == (0, expected);
                        if self.mirror_mode == "dirty-region" {
                            let base = self.mirror_last.as_ref().filter(|(s, lw, lh, last)| {
                                *s == surface_id
                                    && *lw == width
                                    && *lh == height
                                    && last.len() == expected
                            });
                            match base {
                                Some((_, _, _, last)) => {
                                    let mut buffer = last.clone();
                                    for (start, end) in &ranges {
                                        if *end <= buffer.len() && *end <= staging.len() {
                                            buffer[*start..*end]
                                                .copy_from_slice(&staging[*start..*end]);
                                        }
                                    }
                                    self.store_mirror_frame(
                                        surface_id, frame_id, width, height, buffer,
                                    )?;
                                }
                                None if complete => {
                                    self.store_mirror_frame(
                                        surface_id, frame_id, width, height, staging,
                                    )?;
                                }
                                None => {
                                    self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                                }
                            }
                        } else if complete {
                            self.store_mirror_frame(surface_id, frame_id, width, height, staging)?;
                        } else {
                            self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                        }
                    }
                    None => {
                        self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                    }
                }
            }
        }
        Ok(())
    }
    fn store_mirror_frame(
        &mut self,
        surface_id: String,
        frame_id: u64,
        width: u16,
        height: u16,
        bytes: Vec<u8>,
    ) -> Result<(), CoreError> {
        let expected = match mdc_protocol::rgb565_bytes(width, height) {
            Ok(value) => value,
            Err(_) => {
                self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                return Ok(());
            }
        };
        let known_surface = self
            .device
            .as_ref()
            .map(|device| {
                device
                    .capabilities
                    .surfaces
                    .iter()
                    .any(|s| s.id == surface_id && s.width == width && s.height == height)
            })
            .unwrap_or(false);
        if bytes.len() != expected || expected > MAX_FRAME_BYTES || !known_surface {
            self.mirror_dropped = self.mirror_dropped.saturating_add(1);
            return Ok(());
        }
        if self.mirror_inbox.is_some() {
            // Latest-frame-wins: an unread older frame is dropped, not queued.
            self.mirror_dropped = self.mirror_dropped.saturating_add(1);
        }
        self.mirror_frames = self.mirror_frames.saturating_add(1);
        self.mirror_last_frame_id = frame_id;
        // N264: keep the frame so a later dirty-region update can patch it.
        self.mirror_last = Some((surface_id.clone(), width, height, bytes.clone()));
        let notify_surface = surface_id.clone();
        self.mirror_inbox = Some(MirrorFrame {
            surface_id,
            frame_id,
            width,
            height,
            bytes,
        });
        if let Some(manager) = &self.manager {
            let device_id = self
                .device
                .as_ref()
                .map(|device| device.capabilities.device_id.clone())
                .unwrap_or_default();
            if let Ok(mut manager) = manager.lock() {
                manager.push_mirror(
                    device_id,
                    notify_surface,
                    frame_id,
                    width,
                    height,
                    self.mirror_frames,
                    self.mirror_dropped,
                );
            }
        }
        Ok(())
    }
    fn alloc_request_id(&mut self) -> Result<u32, CoreError> {
        let id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(CoreError::Protocol("request ID exhausted".into()))?;
        Ok(id)
    }
    fn write_packet(&mut self, kind: MessageType, payload: Vec<u8>) -> Result<u32, CoreError> {
        if self.pending_control.len() >= MAX_CONTROL_IN_FLIGHT {
            return Err(CoreError::ControlQueueFull);
        }
        let id = self.alloc_request_id()?;
        self.write_packet_with_id(kind, payload, id, true)?;
        self.pending_control.insert(id);
        Ok(id)
    }
    fn write_packet_with_id(
        &mut self,
        kind: MessageType,
        payload: Vec<u8>,
        id: u32,
        track: bool,
    ) -> Result<(), CoreError> {
        let packet = Packet {
            version: VERSION,
            kind,
            flags: 0,
            request_id: id,
            payload,
        };
        let bytes = packet
            .encode(self.max_message)
            .map_err(|e| CoreError::Protocol(e.to_string()))?;
        // N398: count outbound packets/bytes.
        self.stats.packets_sent = self.stats.packets_sent.saturating_add(1);
        self.stats.bytes_sent = self.stats.bytes_sent.saturating_add(bytes.len() as u64);
        let write_result = self.write_bytes(&bytes);
        if matches!(write_result, Ok(()) | Err(CoreError::ShortWrite)) && track {
            self.pending_requests.insert(id);
            if self.config.ack_timeout_ms > 0 {
                self.request_deadlines.insert(
                    id,
                    self.virtual_ms.saturating_add(self.config.ack_timeout_ms),
                );
            }
            self.last_send_ms = self.virtual_ms;
        }
        write_result
    }
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), CoreError> {
        match self.transport.write(bytes) {
            Ok(()) => Ok(()),
            Err(mdc_transport::TransportError::ShortWrite { accepted }) => {
                let accepted = accepted.min(bytes.len());
                if accepted < bytes.len() {
                    self.unsent = Some(bytes[accepted..].to_vec());
                }
                Err(CoreError::ShortWrite)
            }
            Err(error) => Err(CoreError::Transport(error.to_string())),
        }
    }
    fn handle_packet(&mut self, packet: Packet) -> Result<(), CoreError> {
        match packet.kind {
            MessageType::Capabilities => {
                if self.state == ConnectionState::Handshaking {
                    if self.hello_request != Some(packet.request_id) {
                        return Err(CoreError::Protocol("unexpected capabilities reply".into()));
                    }
                } else if self.state == ConnectionState::Ready {
                    // A device may refresh capabilities after the handshake; a
                    // stale run's packet cannot update the current session.
                    if packet.request_id < self.request_floor {
                        return Ok(());
                    }
                } else {
                    return Err(CoreError::Protocol("capabilities before handshake".into()));
                }
                self.release_request(packet.request_id);
                self.hello_request = None;
                let caps: Capabilities = mdc_protocol::decode_control(&packet.payload)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                if caps.max_message == 0 || caps.max_message as usize > MAX_FRAME_BYTES {
                    return Err(CoreError::Protocol(
                        "invalid negotiated message limit".into(),
                    ));
                }
                self.max_message = self.max_message.min(caps.max_message as usize);
                if caps.max_chunk > 0 {
                    self.max_chunk = self.max_chunk.min(caps.max_chunk as usize);
                }
                self.missed_heartbeats = 0;
                self.heartbeat_outstanding = false;
                self.heartbeat_request = None;
                // N304: a changed device boot id discards the previous run's
                // pending ACKs before the new run is adopted.
                self.adopt_device_identity(&caps);
                if let Some(mgr) = &self.manager {
                    mgr.lock()
                        .map_err(|e| CoreError::Transport(e.to_string()))?
                        .upsert(caps.clone())?;
                }
                let mut device = Device::new(caps);
                device.state = ConnectionState::Ready;
                self.device = Some(device);
                self.state = ConnectionState::Ready;
                self.reconnect_attempt = 0;
                self.last_heartbeat_ms = self.virtual_ms;
                Ok(())
            }
            MessageType::Pong => {
                if packet.request_id < self.request_floor {
                    return Ok(());
                }
                // SIM-R04: an unsolicited Pong must not prove a dead peer alive.
                if !self.heartbeat_outstanding || self.heartbeat_request != Some(packet.request_id)
                {
                    return Ok(());
                }
                self.heartbeat_outstanding = false;
                self.heartbeat_request = None;
                self.missed_heartbeats = 0;
                Ok(())
            }
            MessageType::Ack => {
                if packet.request_id < self.request_floor
                    || self.finished_acks.contains(&packet.request_id)
                {
                    return Ok(());
                }
                let ack: Ack = mdc_protocol::decode_control(&packet.payload)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                // N304: ignore an ACK stamped with a previous run id.
                if ack.run_id != 0 && self.device_run_id != 0 && ack.run_id != self.device_run_id {
                    return Ok(());
                }
                if self.pending_requests.contains(&packet.request_id)
                    && (ack.received || ack.displayed)
                {
                    let waiting_frame = self.pending_frames.contains_key(&packet.request_id);
                    if waiting_frame && !ack.displayed {
                        // received 只表示对端收过。待发帧和 needs_full_frame 都要等 displayed。
                        self.frame_received_at
                            .entry(packet.request_id)
                            .or_insert_with(std::time::Instant::now);
                        return Ok(());
                    }
                    self.record_finished_ack(packet.request_id);
                    self.stats.acks_received = self.stats.acks_received.saturating_add(1);
                    self.note_displayed(packet.request_id);
                    self.release_request(packet.request_id);
                    let was_frame = self.pending_frames.contains_key(&packet.request_id);
                    if let Some((surface_id, frame_id)) =
                        self.pending_frames.remove(&packet.request_id)
                    {
                        self.release_held(&surface_id, frame_id);
                        self.launch_pending_frame(&surface_id, frame_id)?;
                    } else if let Some(device) = self.device.as_mut() {
                        let ids: Vec<_> = device
                            .surfaces
                            .iter()
                            .map(|surface| surface.info.id.clone())
                            .collect();
                        for surface_id in ids {
                            self.launch_pending_frame(&surface_id, ack.frame_id)?;
                        }
                    }
                    if was_frame && ack.displayed {
                        self.needs_full_frame = false;
                    }
                }
                Ok(())
            }
            MessageType::Error => {
                if packet.request_id < self.request_floor {
                    return Ok(());
                }
                self.release_request(packet.request_id);
                if let Some((surface_id, frame_id)) = self.pending_frames.remove(&packet.request_id)
                {
                    self.release_held(&surface_id, frame_id);
                    self.launch_pending_frame(&surface_id, frame_id)?;
                }
                let detail = mdc_protocol::decode_control::<ErrorPayload>(&packet.payload)
                    .map(|err| format!("{}: {}", err.code, err.message))
                    .unwrap_or_else(|_| "device returned error".into());
                Err(CoreError::Protocol(detail))
            }
            MessageType::Input => {
                if self.state != ConnectionState::Ready {
                    return Ok(());
                }
                if packet.request_id != 0 && packet.request_id < self.request_floor {
                    return Ok(());
                }
                let event: InputEvent = mdc_protocol::decode_control(&packet.payload)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                let device_id = self
                    .device
                    .as_ref()
                    .map(|d| d.capabilities.device_id.clone())
                    .unwrap_or_default();
                // N473/N735: feed the gesture tracker before forwarding the raw
                // event; a two-finger move may emit both a pan and a pinch.
                let gestures = self.gestures.on_events(&event, self.virtual_ms);
                if let Some(mgr) = &self.manager {
                    mgr.lock()
                        .map_err(|e| CoreError::Transport(e.to_string()))?
                        .push_input(device_id, event);
                }
                for gesture in gestures {
                    self.emit_gesture(gesture);
                }
                Ok(())
            }
            MessageType::Ota => {
                // Device-originated OTA status replies are acknowledged silently;
                // host-initiated OTA uses send_ota + Ack/Error.
                Ok(())
            }
            MessageType::MirrorFrame => self.handle_mirror_frame(packet),
            MessageType::Config => self.handle_config(&packet),
            MessageType::Metric => {
                // N316: a metric is a diagnostic record, never an ACK/error for a
                // frame transaction.
                let metric: MetricReport = mdc_protocol::decode_control(&packet.payload)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                self.note_run_identity(metric.boot_id, metric.run_id);
                // N547: fold the device's applied-tile counter into the snapshot.
                if metric.name == TILES_RECEIVED_METRIC {
                    self.device_tiles_received = metric.value.max(0.0) as u64;
                }
                self.push_diagnostic(DiagnosticRecord::Metric(metric));
                Ok(())
            }
            MessageType::BootInfo => {
                let boot: BootInfo = mdc_protocol::decode_control(&packet.payload)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                self.note_run_identity(boot.boot_id, boot.run_id);
                self.push_diagnostic(DiagnosticRecord::BootInfo(boot));
                Ok(())
            }
            MessageType::Telemetry => {
                // N335: the reserved telemetry type now carries a structured event.
                let event: mdc_protocol::TelemetryEvent =
                    mdc_protocol::decode_control(&packet.payload)
                        .map_err(|e| CoreError::Protocol(e.to_string()))?;
                self.note_run_identity(event.boot_id, event.run_id);
                // N357: also surface a metadata-only manager event.
                if let Some(mgr) = &self.manager {
                    let device_id = self
                        .device
                        .as_ref()
                        .map(|d| d.capabilities.device_id.clone())
                        .unwrap_or_default();
                    if let Ok(mut manager) = mgr.lock() {
                        manager.push_telemetry(device_id, event.name.clone(), event.level.clone());
                    }
                }
                self.push_diagnostic(DiagnosticRecord::Telemetry(event));
                Ok(())
            }
            MessageType::Crash => {
                // N367: the reserved crash type now carries a fault report.
                let report: mdc_protocol::CrashReport =
                    mdc_protocol::decode_control(&packet.payload)
                        .map_err(|e| CoreError::Protocol(e.to_string()))?;
                self.note_run_identity(report.boot_id, report.run_id);
                self.push_diagnostic(DiagnosticRecord::Crash(report));
                Ok(())
            }
            MessageType::MirrorCtrl => Ok(()),
            // Reserved diagnostics without a payload (N303) are ignored.
            _ => Ok(()),
        }
    }
}
impl Device {
    pub fn new(c: Capabilities) -> Self {
        let surfaces = c.surfaces.iter().cloned().map(SurfaceQueue::new).collect();
        Self {
            capabilities: c,
            state: ConnectionState::Disconnected,
            surfaces,
        }
    }
    /// N308: the surface's advertised display profile.
    pub fn surface(&self, surface_id: &str) -> Option<&Surface> {
        self.capabilities
            .surfaces
            .iter()
            .find(|s| s.id == surface_id)
    }
    /// N308: tile cut the host should use for this surface, adapting to the
    /// device-advertised preferred tile size (falls back to the default).
    pub fn plan_surface_tiles(&self, surface_id: &str) -> Vec<(u16, u16, u16, u16)> {
        match self.surface(surface_id) {
            Some(surface) => plan_tiles(surface.width, surface.height, surface.preferred_tile_size),
            None => Vec::new(),
        }
    }
    /// N758: cut every surface into tiles and plan them against one shared byte
    /// budget. Surfaces are equal priority; the caller can reorder the result.
    pub fn plan_shared_tiles(&self, budget: u64) -> mdc_schedule::SharedDecision {
        self.plan_shared_tiles_weighted(budget, &[])
    }

    /// N767: like [`plan_shared_tiles`](Self::plan_shared_tiles) with a relative
    /// weight per surface (missing entries mean 1). The fair pass always serves
    /// each surface once; weights only share the extra budget.
    pub fn plan_shared_tiles_weighted(
        &self,
        budget: u64,
        weights: &[u16],
    ) -> mdc_schedule::SharedDecision {
        let demands: Vec<mdc_schedule::SurfaceDemand> = self
            .capabilities
            .surfaces
            .iter()
            .enumerate()
            .map(|(index, surface)| mdc_schedule::SurfaceDemand {
                surface: index as u32,
                priority: 0,
                weight: weights.get(index).copied().unwrap_or(1),
                tiles: plan_tiles(surface.width, surface.height, surface.preferred_tile_size)
                    .into_iter()
                    .enumerate()
                    .map(|(tile, _)| mdc_schedule::TileCost::new(tile as u32, 1024, false))
                    .collect(),
            })
            .collect();
        mdc_schedule::plan_shared(budget, &demands)
    }

    pub fn validate_frame(&self, f: &Frame) -> Result<(), CoreError> {
        let surface = self
            .capabilities
            .surfaces
            .iter()
            .find(|s| s.id == f.surface_id)
            .ok_or(CoreError::Dimensions)?;
        if f.width != surface.width || f.height != surface.height {
            return Err(CoreError::Dimensions);
        }
        if !surface.pixel_format.is_implemented() {
            return Err(CoreError::Format(surface.pixel_format.to_string()));
        }
        let expected = usize::from(f.width)
            .checked_mul(usize::from(f.height))
            .and_then(|n| n.checked_mul(2))
            .ok_or(CoreError::TooLarge)?;
        if f.bytes.len() != expected || expected > MAX_FRAME_BYTES {
            return Err(CoreError::TooLarge);
        }
        Ok(())
    }
    pub fn send_tile(&self, t: &Tile) -> Result<(), CoreError> {
        let s = self
            .capabilities
            .surfaces
            .iter()
            .find(|s| s.id == t.surface_id)
            .ok_or(CoreError::TileBounds)?;
        let x = t.x as u32 + t.width as u32;
        let y = t.y as u32 + t.height as u32;
        if x > s.width as u32 || y > s.height as u32 {
            return Err(CoreError::TileBounds);
        }
        let expected = (t.width as usize)
            .checked_mul(t.height as usize)
            .and_then(|x| x.checked_mul(2))
            .ok_or(CoreError::TooLarge)?;
        if t.bytes.len() != expected {
            return Err(CoreError::TooLarge);
        }
        Ok(())
    }
    pub fn touch(&self) -> Result<(), CoreError> {
        if self.capabilities.touch {
            Ok(())
        } else {
            Err(CoreError::Unsupported)
        }
    }
    pub fn ota(&self) -> Result<(), CoreError> {
        if self.capabilities.ota {
            Ok(())
        } else {
            Err(CoreError::Unsupported)
        }
    }

    /// Apply runtime-detected surface geometry (FrameOS display_detect idea).
    /// Returns Ok(true) when values changed, Ok(false) when unchanged.
    pub fn update_surface_geometry(
        &mut self,
        surface_id: &str,
        width: u16,
        height: u16,
    ) -> Result<bool, CoreError> {
        if width == 0 || height == 0 {
            return Err(CoreError::Dimensions);
        }
        let _ = mdc_protocol::rgb565_bytes(width, height).map_err(|_| CoreError::TooLarge)?;
        let surface = self
            .capabilities
            .surfaces
            .iter_mut()
            .find(|s| s.id == surface_id)
            .ok_or(CoreError::Dimensions)?;
        if surface.width == width && surface.height == height {
            return Ok(false);
        }
        surface.width = width;
        surface.height = height;
        surface.stride = u32::from(width) * 2;
        if let Some(queue) = self.surfaces.iter_mut().find(|q| q.info.id == surface_id) {
            queue.info.width = width;
            queue.info.height = height;
            queue.info.stride = u32::from(width) * 2;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
