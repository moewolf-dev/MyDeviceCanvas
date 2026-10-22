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
use thiserror::Error;

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_MANAGER_EVENTS: usize = 128;
pub const MAX_CONTROL_IN_FLIGHT: usize = 32;
/// N40: longest configuration value the host will send (bytes).
pub const MAX_CONFIG_VALUE_BYTES: usize = 256;
/// N316: bounded host-side diagnostic queue (metrics + boot info).
pub const MAX_DIAGNOSTICS: usize = 64;

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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    #[error("unsupported pixel format: {0}")]
    Format(String),
    #[error("frame dimensions do not match surface")]
    Dimensions,
    #[error("frame is too large")]
    TooLarge,
    #[error("host copy budget exceeded")]
    CopyBudget,
    #[error("tile is outside surface")]
    TileBounds,
    #[error("queue is full")]
    QueueFull,
    #[error("control queue is full")]
    ControlQueueFull,
    #[error("short write; resume the same frame before sending another")]
    ShortWrite,
    #[error("feature is not supported by device")]
    Unsupported,
    #[error("transport error: {0}")]
    Transport(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("device major version is incompatible")]
    VersionMismatch,
    #[error("session is not ready")]
    NotReady,
    #[error("ack timeout")]
    AckTimeout,
    #[error("heartbeat missed")]
    HeartbeatMissed,
    #[error("connect timeout")]
    ConnectTimeout,
    #[error("network endpoint is not paired")]
    NotPaired,
    #[error("full frame required after reconnect or endpoint switch before tiles")]
    NeedFullFrame,
    /// N329: the frame-pacing budget refused this frame; retry on a later tick.
    #[error("frame throttled by the pacing budget")]
    Throttled,
}

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
            connect_timeout_ms: 5_000,
            ack_timeout_ms: 3_000,
            heartbeat_ms: 1_000,
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

pub struct Session<T: Transport> {
    transport: T,
    pub state: ConnectionState,
    pub device: Option<Device>,
    rx: Vec<u8>,
    next_request_id: u32,
    max_message: usize,
    max_chunk: usize,
    pending_requests: std::collections::BTreeSet<u32>,
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
            pending_control: std::collections::BTreeSet::new(),
            frame_sent_at: BTreeMap::new(),
            frame_received_at: BTreeMap::new(),
            displayed_timings: Vec::new(),
            dropped_frames: 0,
            pending_frames: BTreeMap::new(),
            request_floor: 0,
            finished_acks: std::collections::BTreeSet::new(),
            unsent: None,
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
    /// Drop every outstanding request/held frame from the previous device run.
    fn discard_run(&mut self) {
        let held: Vec<usize> = self.held_frames.values().copied().collect();
        for pixels in held {
            self.copy_budget.release_frame(pixels);
        }
        self.held_frames.clear();
        self.request_floor = self.next_request_id;
        self.pending_requests.clear();
        self.pending_control.clear();
        self.pending_frames.clear();
        self.finished_acks.clear();
        self.frame_sent_at.clear();
        self.frame_received_at.clear();
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
        self.write_packet(MessageType::Hello, hello)?;
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
        // 已经进缓冲区的包先处理。对端先写下 cancel 再关闭时，不能让关闭错误吞掉这些包。
        loop {
            match self.transport.read() {
                Ok(Some(bytes)) => {
                    // N436: count inbound bytes.
                    self.stats.bytes_received =
                        self.stats.bytes_received.saturating_add(bytes.len() as u64);
                    self.rx.extend(bytes)
                }
                Ok(None) => break,
                Err(error) => {
                    if self.rx.is_empty() {
                        return Err(CoreError::Transport(error.to_string()));
                    }
                    break;
                }
            }
        }
        loop {
            match mdc_protocol::decode_stream(&self.rx, self.max_message) {
                mdc_protocol::StreamStep::Packet(packet, consumed) => {
                    self.rx.drain(..consumed);
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
                    && !self.pending_requests.is_empty()
                    && virtual_ms.saturating_sub(self.last_send_ms) >= self.config.ack_timeout_ms
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
        self.finished_acks.clear();
        self.held_frames.clear();
        self.copy_budget = HostCopyBudget::new(self.copy_limit);
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
    }
    pub fn enable_reconnect(&mut self, enabled: bool) {
        self.reconnect_enabled = enabled;
        self.config.reconnect = enabled;
    }
    pub fn mark_transport_failure(&mut self) -> Option<u64> {
        if !self.reconnect_enabled {
            self.state = ConnectionState::Disconnected;
            self.transport.close();
            return None;
        }
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        self.state = ConnectionState::Reconnecting;
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
    fn release_request(&mut self, id: u32) {
        self.pending_requests.remove(&id);
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
        let Some(rest) = self.unsent.take() else {
            return Ok(());
        };
        self.write_bytes(&rest)
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
            let begin =
                encode_frame_begin(surface_id, frame_id, width, height, pixels.len() as u32)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
            let mut result = self.write_packet_with_id(MessageType::Frame, begin, request_id, true);
            if result.is_ok() {
                let mut offset = 0u32;
                while (offset as usize) < pixels.len() {
                    let end = ((offset as usize) + max_chunk).min(pixels.len());
                    let chunk = encode_frame_chunk(frame_id, offset, &pixels[offset as usize..end]);
                    result =
                        self.write_packet_with_id(MessageType::Frame, chunk, request_id, false);
                    if result.is_err() {
                        break;
                    }
                    offset = end as u32;
                }
            }
            if result.is_ok() {
                let commit = encode_frame_commit(frame_id);
                result = self.write_packet_with_id(MessageType::Frame, commit, request_id, false);
            }
            result
        };
        if matches!(write_result, Ok(()) | Err(CoreError::ShortWrite)) {
            self.pending_frames
                .insert(request_id, (surface_id.to_string(), frame_id));
            self.frame_sent_at
                .insert(request_id, std::time::Instant::now());
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
        let total = u32::try_from(bytes.len()).map_err(|_| CoreError::TooLarge)?;
        let checksum = package_checksum(bytes);
        let request_id =
            self.write_packet(MessageType::Package, encode_package_begin(total, checksum))?;
        let mut offset: u32 = 0;
        for chunk in bytes.chunks(mdc_protocol::DEFAULT_MAX_CHUNK) {
            self.write_packet(MessageType::Package, encode_package_chunk(offset, chunk))?;
            offset = offset
                .checked_add(chunk.len() as u32)
                .ok_or(CoreError::TooLarge)?;
        }
        self.write_packet(MessageType::Package, encode_package_commit())?;
        self.packages_sent = self.packages_sent.saturating_add(1);
        Ok(request_id)
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
        self.write_packet(MessageType::Config, payload)
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
        self.write_packet(MessageType::Config, payload)
    }

    /// N40: a configuration value learned from the device, if any.
    pub fn config(&self, key: &str) -> Option<&str> {
        self.config_cache.get(key).map(String::as_str)
    }

    pub fn config_values(&self) -> &BTreeMap<String, String> {
        &self.config_cache
    }

    /// N73: play a baked-atlas clip on the device (via the `animation` setting).
    /// N833: a single clip clears a cached blend selection so the two settings
    /// cannot both appear active on the host.
    pub fn set_animation(&mut self, clip: &str) -> Result<u32, CoreError> {
        let reply = self.set_config("animation", clip)?;
        if self.config_cache.contains_key("animation_blend") {
            self.config_cache
                .insert("animation_blend".to_string(), "off".to_string());
        }
        Ok(reply)
    }

    /// N73: stop the device-side animation.
    pub fn stop_animation(&mut self) -> Result<u32, CoreError> {
        let reply = self.set_config("animation", "off")?;
        if self.config_cache.contains_key("animation_blend") {
            self.config_cache
                .insert("animation_blend".to_string(), "off".to_string());
        }
        Ok(reply)
    }

    /// N73: the clip the device reported as active, if any.
    pub fn active_animation(&self) -> Option<&str> {
        self.config("animation").filter(|value| *value != "off")
    }

    /// N833: blend up to four clips (`"idle:700,wave:300"`). A non-empty blend
    /// clears the cached single-clip selection; `off` clears the blend.
    pub fn set_animation_blend(&mut self, blend: &str) -> Result<u32, CoreError> {
        let reply = self.set_config("animation_blend", blend)?;
        if parse_animation_blend(blend).is_some_and(|entries| !entries.is_empty())
            && self.config_cache.contains_key("animation")
        {
            self.config_cache
                .insert("animation".to_string(), "off".to_string());
        }
        Ok(reply)
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
        if reply.ok {
            if let Some(value) = reply.value {
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
                self.release_request(packet.request_id);
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
                self.heartbeat_outstanding = false;
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
                    self.finished_acks.insert(packet.request_id);
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
mod tests {
    use super::*;
    use mdc_protocol::{MessageType, Packet};
    use mdc_transport::MemoryLink;
    fn caps() -> Capabilities {
        Capabilities {
            device_id: "hb".into(),
            firmware: "1".into(),
            surfaces: vec![s()],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        }
    }
    fn s() -> Surface {
        Surface {
            id: "main".into(),
            width: 2,
            height: 2,
            pixel_format: "RGB565".into(),
            stride: 4,
            rotation: 0,
            preferred_tile_size: 0,
            supports_partial_update: false,
            supports_full_framebuffer: false,
        }
    }

    #[test]
    fn surface_weight_config_parser_is_bounded() {
        // N777: 1..=4 entries, each 1..=16; whitespace tolerated.
        assert_eq!(parse_surface_weights("3,1"), Some(vec![3, 1]));
        assert_eq!(parse_surface_weights(" 2 , 4 "), Some(vec![2, 4]));
        for bad in ["", "0", "17", "1,0", "1,2,3,4,5", "1,x", "-1"] {
            assert_eq!(parse_surface_weights(bad), None, "{bad}");
        }
    }

    #[test]
    fn animation_blend_config_parser_is_bounded() {
        // N833: `off`/empty clear the blend; entries are name:weight with unique
        // names, weights 1..=1000, and at most four clips.
        assert_eq!(parse_animation_blend("off"), Some(Vec::new()));
        assert_eq!(parse_animation_blend("  "), Some(Vec::new()));
        assert_eq!(
            parse_animation_blend("idle:700, wave:300"),
            Some(vec![("idle".into(), 700), ("wave".into(), 300)])
        );
        assert_eq!(
            format_animation_blend(&[("idle".into(), 700), ("wave".into(), 300)]),
            "idle:700,wave:300"
        );
        assert_eq!(format_animation_blend(&[]), "off");
        for bad in [
            "idle",
            "idle:0",
            "idle:1001",
            "idle:-1",
            "idle:1,idle:2",
            ":5",
            "off:5",
            "a:1,b:2,c:3,d:4,e:5",
            "this-name-is-longer-than-thirty-two-bytes:1",
        ] {
            assert_eq!(parse_animation_blend(bad), None, "{bad}");
        }
    }

    #[test]
    fn shared_surface_plan_fairly_advances_two_displays() {
        // N758: one KiB of budget selects one tile overall; both surfaces are
        // represented, so 2 KiB serves the first tile of each.
        let mut capabilities = caps();
        capabilities.surfaces = vec![
            Surface {
                id: "main".into(),
                width: 64,
                height: 64,
                stride: 128,
                ..s()
            },
            Surface {
                id: "aux".into(),
                width: 64,
                height: 64,
                stride: 128,
                ..s()
            },
        ];
        let device = Device::new(capabilities);
        let tight = device.plan_shared_tiles(1024);
        assert_eq!(tight.surfaces.len(), 2);
        assert_eq!(tight.surfaces[0].selected, vec![0]);
        assert!(tight.surfaces[1].selected.is_empty());
        assert_eq!(tight.surfaces[0].deferred.len(), 3, "64px/32px = 4 tiles");
        let roomy = device.plan_shared_tiles(2048);
        assert_eq!(roomy.surfaces[0].selected, vec![0]);
        assert_eq!(roomy.surfaces[1].selected, vec![0]);
        // N767: weights share only the extra budget; both surfaces move first.
        let weighted = device.plan_shared_tiles_weighted(3072, &[2, 1]);
        assert_eq!(weighted.surfaces[0].selected, vec![0, 1]);
        assert_eq!(weighted.surfaces[1].selected, vec![0]);
        assert_eq!(weighted.spent, 3072);
    }

    #[test]
    fn validates_frame() {
        let mut q = SurfaceQueue::new(s());
        assert!(q
            .enqueue_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0; 8]
            })
            .is_ok());
        assert_eq!(
            q.enqueue_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0; 7]
            }),
            Err(CoreError::TooLarge)
        );
    }

    #[test]
    fn surface_geometry_writeback_updates_caps_and_queue() {
        let caps = Capabilities {
            device_id: "x".into(),
            firmware: "0".into(),
            surfaces: vec![s()],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        let mut d = Device::new(caps);
        assert!(!d.update_surface_geometry("main", 2, 2).unwrap());
        assert!(d.update_surface_geometry("main", 480, 320).unwrap());
        assert_eq!(d.capabilities.surfaces[0].width, 480);
        assert_eq!(d.capabilities.surfaces[0].stride, 960);
        assert_eq!(d.surfaces[0].info.height, 320);
    }
    #[test]
    fn tile_bounds_checked() {
        let c = Capabilities {
            device_id: "x".into(),
            firmware: "0".into(),
            surfaces: vec![s()],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        let d = Device::new(c);
        assert_eq!(
            d.send_tile(&Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 2,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0; 2]
            }),
            Err(CoreError::TileBounds)
        );
        assert_eq!(d.touch(), Err(CoreError::Unsupported));
    }

    #[test]
    fn reconnect_policy_is_bounded_and_explicit_disconnect_wins() {
        let policy = ReconnectPolicy {
            max_attempts: 3,
            base_delay_ms: 1_000,
            max_delay_ms: 30_000,
            jitter_ms: 10,
        };
        assert_eq!(policy.delay_ms(1, 4), Some(1_004));
        assert_eq!(policy.delay_ms(1, 40), Some(1_010));
        assert_eq!(policy.delay_ms(3, 20), Some(4_010));
        assert_eq!(policy.delay_ms(4, 0), None);
        let tight = ReconnectPolicy {
            max_attempts: 3,
            base_delay_ms: 1_000,
            max_delay_ms: 1_005,
            jitter_ms: 10,
        };
        assert_eq!(tight.delay_ms(1, 0), Some(1_000));
        assert_eq!(tight.delay_ms(1, 9), Some(1_005));
        let (link, _) = MemoryLink::pair();
        let mut session = Session::new(link);
        assert_eq!(session.mark_transport_failure(), Some(1_000));
        session.disconnect();
        assert_eq!(session.mark_transport_failure(), None);
        assert_eq!(session.state, ConnectionState::Disconnected);
    }
    #[test]
    fn touch_rotation_is_bounded() {
        assert_eq!(transform_touch(480, 320, 90, 0, 0), Ok((319, 0)));
        assert_eq!(transform_touch(480, 320, 180, 479, 319), Ok((0, 0)));
        assert_eq!(
            transform_touch(480, 320, 0, 480, 0),
            Err(CoreError::TileBounds)
        );
    }

    fn handshake<T: mdc_transport::Transport>(session: &mut Session<T>, device: &mut MemoryLink) {
        let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps()).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
    }

    #[test]
    fn install_package_requires_a_renderer_capability() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        assert!(matches!(
            session.install_package(&[1, 2, 3]),
            Err(CoreError::Unsupported)
        ));
        assert_eq!(session.packages_sent(), 0);
    }

    #[test]
    fn install_package_chunks_over_the_link_and_reassembles() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        let mut capability = caps();
        capability.renderer = Some(mdc_protocol::RendererCapability::sprite_only(512));
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&capability).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.state, ConnectionState::Ready);

        let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"count":4,"type":"VEC3"}]}"#;
        let model = mdc_model::GltfImporter
            .import(gltf, &mdc_model::ImportLimits::default())
            .unwrap();
        let package =
            mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::sprite_only())
                .unwrap();
        let bytes = package.to_bytes().unwrap();

        session.install_package(&bytes).unwrap();
        assert_eq!(session.packages_sent(), 1);

        // The device side reassembles the container from the PACKAGE stream.
        let mut assembler = mdc_model::PackageAssembler::begin();
        while let Some(raw) = device.read().unwrap() {
            let packet = Packet::decode(&raw, 1 << 20).unwrap().0;
            if packet.kind != MessageType::Package {
                continue;
            }
            match mdc_protocol::decode_package_payload(&packet.payload).unwrap() {
                mdc_protocol::PackagePayload::Begin { .. } => {}
                mdc_protocol::PackagePayload::Chunk { data, .. } => {
                    assembler.push_chunk(&data).unwrap()
                }
                mdc_protocol::PackagePayload::Commit => break,
            }
        }
        assert_eq!(assembler.finish().unwrap(), package);
    }

    #[test]
    fn config_ops_are_capability_gated_and_cached() {
        let (host, mut dev) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        let mut capability = caps();
        capability.config = mdc_protocol::ConfigCapability::new(&["brightness"]);
        write_caps(&mut dev, hello.request_id, &capability);
        session.poll().unwrap();
        assert_eq!(session.state, ConnectionState::Ready);

        // An unadvertised key is refused without sending anything.
        assert!(matches!(
            session.set_config("wifi_password", "hunter2"),
            Err(CoreError::Unsupported)
        ));

        session.set_config("brightness", "128").unwrap();
        let packet = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        assert_eq!(packet.kind, MessageType::Config);
        let command: ConfigCommand = mdc_protocol::decode_control(&packet.payload).unwrap();
        assert_eq!(command.action, "set");
        assert_eq!(command.key, "brightness");
        assert_eq!(command.value.as_deref(), Some("128"));

        let big = "x".repeat(MAX_CONFIG_VALUE_BYTES + 1);
        assert!(matches!(
            session.set_config("brightness", &big),
            Err(CoreError::TooLarge)
        ));

        let reply = ConfigReply {
            key: "brightness".into(),
            ok: true,
            value: Some("200".into()),
            error: None,
        };
        dev.write(
            &Packet {
                version: VERSION,
                kind: MessageType::Config,
                flags: 0,
                request_id: 0,
                payload: encode_control(&reply).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
        session.poll().unwrap();
        assert_eq!(session.config("brightness"), Some("200"));
    }

    #[test]
    fn animation_helpers_drive_the_device_config() {
        let (host, mut dev) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        let mut capability = caps();
        capability.config = mdc_protocol::ConfigCapability::new(&["animation"]);
        write_caps(&mut dev, hello.request_id, &capability);
        session.poll().unwrap();

        session.set_animation("idle").unwrap();
        dev.write(
            &Packet {
                version: VERSION,
                kind: MessageType::Config,
                flags: 0,
                request_id: 0,
                payload: encode_control(&ConfigReply {
                    key: "animation".into(),
                    ok: true,
                    value: Some("idle".into()),
                    error: None,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
        session.poll().unwrap();
        assert_eq!(session.active_animation(), Some("idle"));

        session.stop_animation().unwrap();
        dev.write(
            &Packet {
                version: VERSION,
                kind: MessageType::Config,
                flags: 0,
                request_id: 0,
                payload: encode_control(&ConfigReply {
                    key: "animation".into(),
                    ok: true,
                    value: Some("off".into()),
                    error: None,
                })
                .unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
        session.poll().unwrap();
        assert_eq!(session.active_animation(), None);
    }

    fn frame(bytes: &[u8]) -> Frame {
        Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn stale_ack_keeps_the_new_frame_in_flight() {
        let (host_a, mut dev_a) = MemoryLink::pair();
        let mut session = Session::new(host_a);
        session.connect().unwrap();
        handshake(&mut session, &mut dev_a);
        let old_id = session.send_frame(frame(&[1; 8])).unwrap();
        let _ = dev_a.read().unwrap();
        let (host_b, mut dev_b) = MemoryLink::pair();
        session.switch_transport(host_b).unwrap();
        handshake(&mut session, &mut dev_b);
        let new_id = session.send_frame(frame(&[2; 8])).unwrap();
        assert_ne!(old_id, new_id);
        assert_eq!(session.send_frame(frame(&[3; 8])).unwrap(), 0);
        dev_b
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Ack,
                    flags: 0,
                    request_id: old_id,
                    payload: encode_control(&Ack {
                        received: true,
                        displayed: true,
                        frame_id: 1,
                        error: None,
                        run_id: 0,
                        event_seq: 0,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert!(session.needs_full_frame);
        assert_eq!(
            session.device.as_ref().unwrap().surfaces[0].in_flight(),
            Some(1)
        );
        let mut got = Vec::new();
        while let Some(chunk) = dev_b.read().unwrap() {
            got.extend(chunk);
        }
        assert!(got.windows(8).any(|window| window == [2; 8]));
        assert!(!got.windows(8).any(|window| window == [3; 8]));
    }

    #[test]
    fn short_write_resumes_one_frame_and_blocks_the_next() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(mdc_transport::ShortWriteLink::new(host));
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        session.transport_mut().arm(8);
        assert_eq!(
            session.send_frame(frame(&[2; 8])),
            Err(CoreError::ShortWrite)
        );
        let inflight = session.device.as_ref().unwrap().surfaces[0].in_flight();
        assert!(inflight.is_some());
        assert!(session.device.as_ref().unwrap().surfaces[0]
            .pending_frame_id()
            .is_none());
        assert_eq!(
            session.send_frame(frame(&[3; 8])),
            Err(CoreError::ShortWrite)
        );
        assert_eq!(
            session.device.as_ref().unwrap().surfaces[0].in_flight(),
            inflight
        );
        assert!(session.device.as_ref().unwrap().surfaces[0]
            .pending_frame_id()
            .is_none());
        session.resume_write().unwrap();
        let mut got = Vec::new();
        while let Some(chunk) = device.read().unwrap() {
            got.extend(chunk);
        }
        let (packet, consumed) = Packet::decode(&got, 4096).unwrap();
        assert_eq!(consumed, got.len());
        assert_eq!(packet.kind, MessageType::Frame);
        assert!(packet.payload.windows(8).any(|window| window == [2; 8]));
        assert!(!packet.payload.windows(8).any(|window| window == [3; 8]));
    }

    #[test]
    fn write_without_pong_disconnects_on_virtual_clock() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::with_config(
            host,
            SessionConfig {
                connect_timeout_ms: 0,
                ack_timeout_ms: 0,
                heartbeat_ms: 10,
                reconnect: true,
            },
        );
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        session.send_frame(frame(&[4; 8])).unwrap();
        let _ = device.read().unwrap();
        assert_eq!(session.tick(10).unwrap(), ConnectionState::Ready);
        let (ping, _) = Packet::decode(&device.read().unwrap().unwrap(), 4096).unwrap();
        assert_eq!(ping.kind, MessageType::Ping);
        assert_eq!(session.tick(20).unwrap(), ConnectionState::Ready);
        assert_eq!(session.tick(30).unwrap(), ConnectionState::Ready);
        assert_eq!(session.tick(40), Err(CoreError::HeartbeatMissed));
    }

    #[test]
    fn copy_budget_counts_both_surfaces_and_releases_on_display() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.set_copy_limit(24);
        session.connect().unwrap();
        let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        let mut advertised = caps();
        advertised.surfaces.push(Surface {
            id: "overlay".into(),
            width: 2,
            height: 2,
            pixel_format: "RGB565".into(),
            stride: 4,
            rotation: 0,
            preferred_tile_size: 0,
            supports_partial_update: false,
            supports_full_framebuffer: false,
        });
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&advertised).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        let request = session.send_frame(frame(&[1; 8])).unwrap();
        assert_eq!(session.copy_outstanding(), 24);
        assert_eq!(
            session.send_frame(Frame {
                surface_id: "overlay".into(),
                width: 2,
                height: 2,
                bytes: vec![2; 8],
            }),
            Err(CoreError::CopyBudget)
        );
        assert_eq!(session.copy_outstanding(), 24);
        assert!(session.device.as_ref().unwrap().surfaces[1]
            .in_flight()
            .is_none());
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Ack,
                    flags: 0,
                    request_id: request,
                    payload: encode_control(&Ack {
                        received: true,
                        displayed: true,
                        frame_id: 1,
                        error: None,
                        run_id: 0,
                        event_seq: 0,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.copy_outstanding(), 0);
    }

    #[test]
    fn replayed_displayed_ack_launches_the_pending_frame_once() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        let request = session.send_frame(frame(&[1; 8])).unwrap();
        assert_eq!(session.send_frame(frame(&[2; 8])).unwrap(), 0);
        let _ = device.read().unwrap();
        let ack = Packet {
            version: VERSION,
            kind: MessageType::Ack,
            flags: 0,
            request_id: request,
            payload: encode_control(&Ack {
                received: true,
                displayed: true,
                frame_id: 1,
                error: None,
                run_id: 0,
                event_seq: 0,
            })
            .unwrap(),
        }
        .encode(4096)
        .unwrap();
        device.write(&ack).unwrap();
        session.poll().unwrap();
        let launched = device.read().unwrap().unwrap();
        assert!(launched.windows(8).any(|window| window == [2; 8]));
        device.write(&ack).unwrap();
        session.poll().unwrap();
        assert!(device.read().unwrap().is_none());
    }

    #[test]
    fn input_before_the_new_handshake_is_dropped() {
        let (host, mut device) = MemoryLink::pair();
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        let (host_b, mut device_b) = MemoryLink::pair();
        session.switch_transport(host_b).unwrap();
        let stale = Packet {
            version: VERSION,
            kind: MessageType::Input,
            flags: 0,
            request_id: 0,
            payload: encode_control(&InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "down".into(),
                x: 0,
                y: 0,

                node: None,
            })
            .unwrap(),
        }
        .encode(4096)
        .unwrap();
        device_b.write(&stale).unwrap();
        handshake(&mut session, &mut device_b);
        let early: Vec<_> = manager.lock().unwrap().drain_events().collect();
        assert!(!early
            .iter()
            .any(|event| matches!(event, ManagerEvent::Input { .. })));
        device_b
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Input,
                    flags: 0,
                    request_id: 0,
                    payload: encode_control(&InputEvent {
                        surface_id: "main".into(),
                        pointer_id: 2,
                        phase: "up".into(),
                        x: 1,
                        y: 1,

                        node: None,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            ManagerEvent::Input { event, .. } if event.phase == "up" && event.pointer_id == 2
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            ManagerEvent::Input { event, .. } if event.phase == "down"
        )));
    }
    #[test]
    fn ota_freezes_rendering_and_rejects_missing_slot() {
        let mut unsupported = OtaController::new(false);
        assert_eq!(unsupported.begin(), Err(CoreError::Unsupported));
        let mut ota = OtaController::new(true);
        ota.begin().unwrap();
        ota.verified().unwrap();
        assert!(ota.render_frozen);
        ota.reboot().unwrap();
        ota.confirm(false);
        assert_eq!(ota.state, OtaState::Failed);
        assert!(!ota.render_frozen);
    }
    #[test]
    fn scene_api_stays_unsupported_on_v1() {
        let (link, _) = MemoryLink::pair();
        let mut session = Session::new(link);
        assert_eq!(session.send_scene(&[1, 2, 3]), Err(CoreError::Unsupported));
    }
    #[test]
    fn frame_transaction_accepts_out_of_order_and_idempotent_chunks() {
        let mut tx = FrameAssembler::begin(7, 8, 4).unwrap();
        tx.chunk(4, &[4, 5, 6, 7]).unwrap();
        tx.chunk(0, &[0, 1, 2, 3]).unwrap();
        tx.chunk(0, &[0, 1, 2, 3]).unwrap();
        assert_eq!(tx.commit().unwrap(), vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }
    #[test]
    fn frame_transaction_rejects_conflicts_and_incomplete_commit() {
        let mut tx = FrameAssembler::begin(1, 8, 8).unwrap();
        tx.chunk(0, &[1, 2, 3, 4]).unwrap();
        assert_eq!(
            tx.chunk(2, &[9, 9]),
            Err(TransactionNack::ConflictingDuplicate)
        );
        assert_eq!(tx.chunk(7, &[1, 2]), Err(TransactionNack::InvalidRange));
        assert_eq!(tx.commit(), Err(TransactionNack::Incomplete));
    }
    #[test]
    fn tile_requires_current_base_frame() {
        let tile = Tile {
            surface_id: "main".into(),
            base_frame_id: 4,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0, 0],
        };
        assert_eq!(
            validate_tile_base(&tile, 3),
            Err(TransactionNack::BaseFrameMismatch)
        );
        assert_eq!(validate_tile_base(&tile, 4), Ok(()));
    }
    #[test]
    fn manager_merges_stable_identity_and_bounds_events() {
        let caps = Capabilities {
            device_id: "device".into(),
            firmware: "1".into(),
            surfaces: vec![
                s(),
                Surface {
                    id: "aux".into(),
                    ..s()
                },
            ],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        let mut manager = DeviceManager::new();
        manager.upsert(caps.clone()).unwrap();
        manager.upsert(caps).unwrap();
        assert_eq!(manager.devices().count(), 1);
        assert_eq!(
            manager
                .device("device")
                .unwrap()
                .capabilities
                .surfaces
                .len(),
            2
        );
        assert_eq!(manager.drain_events().count(), 2);
        assert!(manager.remove("device"));
        assert!(manager.device("device").is_none());
    }

    #[test]
    fn moves_coalesce_and_a_full_queue_keeps_down() {
        let mut manager = DeviceManager::new();
        let point = |phase: &str, pointer: u16, x: u16| InputEvent {
            surface_id: "main".into(),
            pointer_id: pointer,
            phase: phase.into(),
            x,
            y: 1,

            node: None,
        };
        manager.push_input("dev".into(), point("move", 1, 1));
        manager.push_input("dev".into(), point("move", 1, 4));
        let coalesced: Vec<_> = manager.drain_events().collect();
        assert_eq!(coalesced.len(), 1);
        match &coalesced[0] {
            ManagerEvent::Input { event, .. } => {
                assert_eq!(event.phase, "move");
                assert_eq!(event.x, 4);
            }
            other => panic!("expected move, got {other:?}"),
        }
        for index in 0..MAX_MANAGER_EVENTS {
            manager.push_event(ManagerEvent::Connected(format!("fill-{index}")));
        }
        manager.push_input("dev".into(), point("down", 7, 2));
        let events: Vec<_> = manager.drain_events().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            ManagerEvent::Error { message, .. } if message.contains("overflow")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ManagerEvent::Input { event, .. } if event.phase == "down" && event.pointer_id == 7 && event.x == 2
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ManagerEvent::Input { event, .. } if event.phase == "cancel" && event.pointer_id == 7
        )));
    }

    #[test]
    fn control_queue_stops_at_32_without_counting_frames() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = device.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello, 4096).unwrap();
        let caps = Capabilities {
            ota: true,
            ..caps()
        };
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        for _ in 0..MAX_CONTROL_IN_FLIGHT {
            session
                .send_ota(OtaCommand {
                    action: "abort".into(),
                    version: None,
                    size: None,
                    sha256: None,
                    offset: None,
                    len: None,
                })
                .unwrap();
        }
        assert_eq!(
            session.send_ota(OtaCommand {
                action: "abort".into(),
                version: None,
                size: None,
                sha256: None,
                offset: None,
                len: None,
            }),
            Err(CoreError::ControlQueueFull)
        );
        assert!(session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![1; 8],
            })
            .is_ok());
    }
    #[test]
    fn manager_rejects_missing_identity() {
        let mut manager = DeviceManager::new();
        let mut caps = Capabilities {
            device_id: "".into(),
            firmware: "1".into(),
            surfaces: vec![s()],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        assert_eq!(
            manager.upsert(caps.clone()),
            Err(CoreError::Protocol(
                "device identity/surfaces missing".into()
            ))
        );
        caps.device_id = "id".into();
        caps.surfaces.clear();
        assert!(manager.upsert(caps).is_err());
    }
    #[test]
    fn manager_connect_session_stores_single_endpoint() {
        let mut manager = DeviceManager::new();
        assert!(manager.connect_session("dev-a", "memory:1").is_none());
        assert_eq!(manager.active_endpoint("dev-a"), Some("memory:1"));
        let prev = manager.connect_session("dev-a", "tcp:127.0.0.1:9");
        assert_eq!(prev.as_deref(), Some("memory:1"));
        assert_eq!(manager.active_endpoint("dev-a"), Some("tcp:127.0.0.1:9"));
        let events: Vec<_> = manager.drain_events().collect();
        assert!(events.iter().any(|e| matches!(
            e,
            ManagerEvent::EndpointSwitched { device_id } if device_id == "dev-a"
        )));
    }

    #[test]
    fn switch_transport_blocks_tiles_until_full_frame_acked() {
        let (host_a, mut dev_a) = MemoryLink::pair();
        let mut session = Session::new(host_a);
        session.connect().unwrap();
        let hello = Packet::decode(&dev_a.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        dev_a
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&Capabilities {
                        device_id: "sw".into(),
                        firmware: "1".into(),
                        surfaces: vec![s()],
                        frame: true,
                        tile: true,
                        touch: false,
                        ota: false,
                        max_message: 4096,
                        max_chunk: 16384,
                        max_in_flight: 1,
                        max_fps: 30,
                        mirror: mdc_protocol::MirrorCapability::disabled(),
                        local_display_fps: 0,
                        mirror_fps: 0,
                        mirror_bandwidth: 0,
                        renderer: None,
                        config: mdc_protocol::ConfigCapability::default(),
                        scene: mdc_protocol::SceneCapability::default(),
                        boot_id: 0,
                        run_id: 0,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();

        let (host_b, mut dev_b) = MemoryLink::pair();
        session.switch_transport(host_b).unwrap();
        assert!(session.needs_full_frame);
        let hello2 = Packet::decode(&dev_b.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        dev_b
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello2.request_id,
                    payload: encode_control(&Capabilities {
                        device_id: "sw".into(),
                        firmware: "1".into(),
                        surfaces: vec![s()],
                        frame: true,
                        tile: true,
                        touch: false,
                        ota: false,
                        max_message: 4096,
                        max_chunk: 16384,
                        max_in_flight: 1,
                        max_fps: 30,
                        mirror: mdc_protocol::MirrorCapability::disabled(),
                        local_display_fps: 0,
                        mirror_fps: 0,
                        mirror_bandwidth: 0,
                        renderer: None,
                        config: mdc_protocol::ConfigCapability::default(),
                        scene: mdc_protocol::SceneCapability::default(),
                        boot_id: 0,
                        run_id: 0,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(
            session.send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0, 0],
            }),
            Err(CoreError::NeedFullFrame)
        );
        let req = session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0; 8],
            })
            .unwrap();
        let frame_pkt = Packet::decode(&dev_b.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        dev_b
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Ack,
                    flags: 0,
                    request_id: frame_pkt.request_id,
                    payload: encode_control(&Ack {
                        received: true,
                        displayed: true,
                        frame_id: req as u64,
                        error: None,
                        run_id: 0,
                        event_seq: 0,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert!(!session.needs_full_frame);
        assert!(session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0, 0],
            })
            .is_ok());
    }
    #[test]
    fn repeated_transactions_release_owned_staging() {
        for frame_id in 0..100_000u64 {
            let mut tx = FrameAssembler::begin(frame_id, 4, 4).unwrap();
            tx.chunk(0, &[1, 2, 3, 4]).unwrap();
            assert_eq!(tx.commit().unwrap(), vec![1, 2, 3, 4]);
        }
    }

    #[test]
    fn session_handshake_reaches_ready() {
        let (host_link, mut device_link) = MemoryLink::pair();
        let mut session = Session::new(host_link);
        session.connect().unwrap();
        let hello_bytes = device_link.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
        assert_eq!(hello.kind, MessageType::Hello);
        let caps = Capabilities {
            device_id: "sim-1".into(),
            firmware: "simulator".into(),
            surfaces: vec![s()],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        let reply = Packet {
            version: VERSION,
            kind: MessageType::Capabilities,
            flags: 0,
            request_id: hello.request_id,
            payload: encode_control(&caps).unwrap(),
        };
        device_link.write(&reply.encode(4096).unwrap()).unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        assert_eq!(
            session.device.as_ref().unwrap().capabilities.device_id,
            "sim-1"
        );
        assert_eq!(
            session.device.as_ref().unwrap().state,
            ConnectionState::Ready
        );
        let frame_id = session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0; 8],
            })
            .unwrap();
        assert_eq!(frame_id, 2);
        let frame_bytes = device_link.read().unwrap().unwrap();
        let (frame, _) = Packet::decode(&frame_bytes, MAX_FRAME_BYTES).unwrap();
        assert_eq!(frame.kind, MessageType::Frame);
        assert_eq!(session.pending_request_count(), 1);
        assert_eq!(
            session.device.as_ref().unwrap().surfaces[0].in_flight(),
            Some(1)
        );
        device_link
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Ack,
                    flags: 0,
                    request_id: frame.request_id,
                    payload: encode_control(&Ack {
                        received: true,
                        displayed: true,
                        frame_id: 1,
                        error: None,
                        run_id: 0,
                        event_seq: 0,
                    })
                    .unwrap(),
                }
                .encode(MAX_FRAME_BYTES)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.pending_request_count(), 0);
        assert!(session.device.as_ref().unwrap().surfaces[0]
            .in_flight()
            .is_none());
        session.disconnect();
        assert_eq!(session.state, ConnectionState::Disconnected);
    }

    #[test]
    fn session_tick_heartbeat_and_ack_timeout() {
        let (host_link, mut device_link) = MemoryLink::pair();
        let mut session = Session::with_config(
            host_link,
            SessionConfig {
                connect_timeout_ms: 100,
                ack_timeout_ms: 50,
                heartbeat_ms: 30,
                reconnect: true,
            },
        );
        session.connect().unwrap();
        let hello_bytes = device_link.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
        let caps = Capabilities {
            device_id: "sim-tick".into(),
            firmware: "simulator".into(),
            surfaces: vec![s()],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        device_link
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        session.tick(0).unwrap();
        assert_eq!(session.tick(30).unwrap(), ConnectionState::Ready);
        let ping = device_link.read().unwrap().unwrap();
        let (pkt, _) = Packet::decode(&ping, 4096).unwrap();
        assert_eq!(pkt.kind, MessageType::Ping);

        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0; 8],
            })
            .unwrap();
        let _ = device_link.read().unwrap();
        assert_eq!(session.tick(30 + 50), Err(CoreError::AckTimeout));
        assert_eq!(session.state, ConnectionState::Reconnecting);
        assert_eq!(
            session.tick(30 + 50 + 1_000).unwrap(),
            ConnectionState::Connecting
        );
        assert!(session.awaiting_transport);
    }

    #[test]
    fn session_provide_transport_rehandshakes_after_reconnect() {
        let (host_link, mut device_link) = MemoryLink::pair();
        let mut session = Session::with_config(
            host_link,
            SessionConfig {
                ack_timeout_ms: 10,
                heartbeat_ms: 0,
                connect_timeout_ms: 1_000,
                reconnect: true,
            },
        );
        session.reconnect_policy = ReconnectPolicy {
            max_attempts: 3,
            base_delay_ms: 100,
            max_delay_ms: 100,
            jitter_ms: 0,
        };
        session.connect().unwrap();
        let hello = device_link.read().unwrap().unwrap();
        let (hello_pkt, _) = Packet::decode(&hello, 4096).unwrap();
        device_link
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello_pkt.request_id,
                    payload: encode_control(&Capabilities {
                        device_id: "re".into(),
                        firmware: "1".into(),
                        surfaces: vec![s()],
                        frame: true,
                        tile: true,
                        touch: false,
                        ota: false,
                        max_message: 4096,
                        max_chunk: 16384,
                        max_in_flight: 1,
                        max_fps: 30,
                        mirror: mdc_protocol::MirrorCapability::disabled(),
                        local_display_fps: 0,
                        mirror_fps: 0,
                        mirror_bandwidth: 0,
                        renderer: None,
                        config: mdc_protocol::ConfigCapability::default(),
                        scene: mdc_protocol::SceneCapability::default(),
                        boot_id: 0,
                        run_id: 0,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0; 8],
            })
            .unwrap();
        let _ = device_link.read();
        assert!(session.tick(20).is_err());
        assert_eq!(session.tick(120).unwrap(), ConnectionState::Connecting);
        assert!(session.awaiting_transport);

        let (host2, mut device2) = MemoryLink::pair();
        session.provide_transport(host2).unwrap();
        let hello2 = device2.read().unwrap().unwrap();
        let (hello2_pkt, _) = Packet::decode(&hello2, 4096).unwrap();
        assert_eq!(hello2_pkt.kind, MessageType::Hello);
        device2
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello2_pkt.request_id,
                    payload: encode_control(&Capabilities {
                        device_id: "re".into(),
                        firmware: "1".into(),
                        surfaces: vec![s()],
                        frame: true,
                        tile: true,
                        touch: false,
                        ota: false,
                        max_message: 4096,
                        max_chunk: 16384,
                        max_in_flight: 1,
                        max_fps: 30,
                        mirror: mdc_protocol::MirrorCapability::disabled(),
                        local_display_fps: 0,
                        mirror_fps: 0,
                        mirror_bandwidth: 0,
                        renderer: None,
                        config: mdc_protocol::ConfigCapability::default(),
                        scene: mdc_protocol::SceneCapability::default(),
                        boot_id: 0,
                        run_id: 0,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        assert!(!session.awaiting_transport);
    }

    #[test]
    fn dirty_tiles_merge_and_send_only_the_changed_area() {
        // N516: overlapping dirty rects collapse to one tile.
        assert_eq!(
            plan_dirty_tiles((8, 8), 4, &[(0, 0, 2, 2), (2, 0, 2, 2)]),
            vec![(0, 0, 4, 4)]
        );
        // A tile-sized rect maps to exactly one tile; a full-surface rect to all.
        assert_eq!(
            plan_dirty_tiles((8, 8), 4, &[(4, 4, 4, 4)]),
            vec![(4, 4, 4, 4)]
        );
        assert_eq!(plan_dirty_tiles((8, 8), 4, &[(0, 0, 8, 8)]).len(), 4);
        // Zero-area and out-of-range rects are dropped/clamped, and the edge
        // tiles are trimmed to the surface.
        assert!(plan_dirty_tiles((8, 8), 4, &[(0, 0, 0, 4)]).is_empty());
        // Tiles come out in tile-grid order (column-major), edges trimmed.
        assert_eq!(
            plan_dirty_tiles((6, 6), 4, &[(0, 0, 6, 6)]),
            vec![(0, 0, 4, 4), (0, 4, 4, 2), (4, 0, 2, 4), (4, 4, 2, 2)]
        );

        // The tile bytes are copied out of the frame at the right offset.
        let mut frame = vec![0u8; 8 * 8 * 2];
        for (i, byte) in frame.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        let tile = extract_tile(&frame, (8, 8), (4, 4, 4, 4));
        assert_eq!(tile.len(), 4 * 4 * 2);
        // Row 0 of the tile starts at (4, 4): byte index (4*8 + 4) * 2 = 72.
        assert_eq!(&tile[..2], &frame[72..74]);

        // End to end: only the dirty tiles are written to the device.
        let (host_link, mut device_link) = MemoryLink::pair();
        let mut session = Session::new(host_link);
        session.connect().unwrap();
        let hello_bytes = device_link.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
        let caps = Capabilities {
            device_id: "sim-dirty".into(),
            firmware: "simulator".into(),
            surfaces: vec![Surface {
                id: "main".into(),
                width: 8,
                height: 8,
                pixel_format: "RGB565".into(),
                stride: 16,
                rotation: 0,
                preferred_tile_size: 4,
                supports_partial_update: true,
                supports_full_framebuffer: false,
            }],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        device_link
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        let sent = session
            .send_dirty_tiles(
                "main",
                1,
                &frame,
                (8, 8),
                4,
                &[(0, 0, 2, 2), (2, 0, 2, 2), (0, 0, 1, 1)],
            )
            .unwrap_or_else(|e| panic!("send_dirty_tiles failed: {e:?}"));
        assert_eq!(sent, 1, "three overlapping rects collapse to one tile");
        assert_eq!(session.stats().tiles_sent, 1);
        assert_eq!(session.stats().tile_bytes_sent, 32);
    }

    #[test]
    fn tile_counters_reconcile_with_the_device() {
        // N536: the host's sent tiles are compared with the device's count.
        let stats = SessionStats {
            tiles_sent: 3,
            tile_bytes_sent: 96,
            ..SessionStats::default()
        };
        let report = reconcile_tiles(&stats, 3);
        assert!(report.consistent);
        assert_eq!(report.missing(), 0);
        assert_eq!(report.bytes_sent, 96);
        // A dropped tile is reported as missing.
        let report = reconcile_tiles(&stats, 2);
        assert!(!report.consistent);
        assert_eq!(report.missing(), 1);
        // A device that reports more than the host sent is inconsistent too.
        let report = reconcile_tiles(&SessionStats::default(), 1);
        assert!(!report.consistent);
        assert_eq!(report.missing(), 0);
    }

    #[test]
    fn dirty_tracker_accumulates_and_escalates() {
        // N526: dirty regions accumulate across frames and switch to a full frame
        // once they cover too much of the surface.
        let mut tracker = DirtyTracker::new((8, 8), 4, 0.5);
        assert_eq!(tracker.decide(), UpdateDecision::Partial(0));
        tracker.mark((0, 0, 1, 1));
        assert_eq!(tracker.decide(), UpdateDecision::Partial(1));
        assert!((tracker.coverage() - 0.25).abs() < 1e-9);
        // A second (adjacent-in-grid) region adds another tile.
        tracker.mark((4, 0, 1, 1));
        assert_eq!(tracker.decide(), UpdateDecision::Partial(2));
        // A third pushes coverage past the threshold -> a full frame.
        tracker.mark((0, 4, 1, 1));
        assert_eq!(tracker.decide(), UpdateDecision::Full);
        assert!(tracker.tiles().is_empty(), "a full frame has no tiles");
        assert_eq!(tracker.take(), UpdateDecision::Full);
        assert!(tracker.pending().is_empty());
        assert_eq!(tracker.updates(), 1);
        // Marking everything is always a full frame.
        tracker.mark_all();
        assert_eq!(tracker.decide(), UpdateDecision::Full);
        tracker.take();
        // Overlapping rectangles collapse before the threshold is applied.
        let mut tight = DirtyTracker::new((8, 8), 4, 0.9);
        tight.mark((0, 0, 2, 2));
        tight.mark((1, 1, 2, 2));
        assert_eq!(tight.decide(), UpdateDecision::Partial(1));
    }

    #[test]
    fn send_dirty_update_chooses_tiles_or_a_frame() {
        // N526: the session sends the tracker's decision.
        let (host_link, mut device_link) = MemoryLink::pair();
        let mut session = Session::new(host_link);
        session.connect().unwrap();
        let hello_bytes = device_link.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
        let caps = Capabilities {
            device_id: "sim-plan".into(),
            firmware: "simulator".into(),
            surfaces: vec![Surface {
                id: "main".into(),
                width: 8,
                height: 8,
                pixel_format: "RGB565".into(),
                stride: 16,
                rotation: 0,
                preferred_tile_size: 4,
                supports_partial_update: true,
                supports_full_framebuffer: false,
            }],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 4,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        device_link
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        let frame = vec![0u8; 8 * 8 * 2];
        let mut tracker = DirtyTracker::new((8, 8), 4, 0.5);
        // A small change goes out as one tile.
        tracker.mark((0, 0, 1, 1));
        assert_eq!(
            session
                .send_dirty_update("main", 0, &frame, &mut tracker)
                .unwrap(),
            UpdateDecision::Partial(1)
        );
        assert_eq!(session.stats().tiles_sent, 1);
        // Nothing dirty sends nothing.
        assert_eq!(
            session
                .send_dirty_update("main", 0, &frame, &mut tracker)
                .unwrap(),
            UpdateDecision::Partial(0)
        );
        // A large change escalates to a full frame.
        tracker.mark_all();
        assert_eq!(
            session
                .send_dirty_update("main", 0, &frame, &mut tracker)
                .unwrap(),
            UpdateDecision::Full
        );
        assert_eq!(session.stats().frames_sent, 1);
        assert_eq!(tracker.updates(), 2, "the empty update does not count");
    }

    #[test]
    fn adaptive_dirty_threshold_follows_the_measured_link() {
        // N732: the same dirty area is tiled on a fast link and sent whole on a
        // link that cannot sustain even one frame per second.
        let dirty = || {
            let mut tracker = DirtyTracker::new((32, 32), 8, 0.5);
            for tile in 0..4 {
                tracker.mark((tile * 8, 0, 8, 8));
            }
            tracker
        };
        let frame_bytes = 32 * 32 * 2;
        let mut slow = dirty();
        assert_eq!(slow.adapt_to_bandwidth(1_000, frame_bytes), 0.05);
        assert_eq!(slow.decide(), UpdateDecision::Full);
        let mut fast = dirty();
        assert_eq!(fast.adapt_to_bandwidth(1_000_000, frame_bytes), 0.9);
        assert_eq!(fast.decide(), UpdateDecision::Partial(4));
        // A zero-sized frame leaves the threshold untouched.
        let mut unchanged = DirtyTracker::new((8, 8), 4, 0.4);
        assert_eq!(unchanged.adapt_to_bandwidth(100, 0), 0.4);
    }

    #[test]
    fn session_measures_bitrate_and_adapts() {
        // N732: the session derives the rate from its own outbound byte counter
        // and the virtual clock, then applies it to a tracker.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        session.tick(0).unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x11; 8],
            })
            .unwrap();
        session.tick(1_000).unwrap();
        let rate = session.measure_bitrate().expect("one second elapsed");
        assert!(rate > 0, "the frame was written");
        let mut tracker = DirtyTracker::new((32, 32), 8, 0.5);
        let threshold = session
            .adapt_dirty_threshold(&mut tracker, 32 * 32 * 2)
            .expect("rate available");
        assert!((0.05..=0.9).contains(&threshold));
        // A second sample with no time elapsed has no rate.
        assert_eq!(session.measure_bitrate(), None);
    }

    #[test]
    fn session_adapts_frame_pacing_to_the_measured_rate() {
        // N741: a low measured rate caps the scheduler at one frame per second.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        session.set_frame_pacing(30, 0);
        session.tick(0).unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x11; 8],
            })
            .unwrap();
        session.tick(1_000).unwrap();
        let config = session
            .adapt_frame_pacing(10_000_000)
            .expect("scheduler and rate present");
        assert_eq!(config.max_fps, 1);
        assert!(config.max_bytes_per_second > 0);
    }

    #[test]
    fn joint_streaming_tuning_updates_both_knobs() {
        // N750: one measured rate drives both pacing and dirty thresholds.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        session.set_frame_pacing(30, 0);
        session.tick(0).unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x11; 8],
            })
            .unwrap();
        session.tick(1_000).unwrap();
        let mut tracker = DirtyTracker::new((32, 32), 8, 0.5);
        for tile in 0..4 {
            tracker.mark((tile * 8, 0, 8, 8));
        }
        let tuning = session
            .tune_streaming(&mut tracker, 10_000_000)
            .expect("rate present");
        assert!(tuning.bitrate > 0);
        assert_eq!(tuning.pacing.unwrap().max_fps, 1);
        assert_eq!(tuning.dirty_threshold, Some(0.05));
        assert_eq!(tracker.decide(), UpdateDecision::Full);
    }

    #[test]
    fn session_send_tile_writes_tile_packet() {
        let (host_link, mut device_link) = MemoryLink::pair();
        let mut session = Session::new(host_link);
        session.connect().unwrap();
        let hello_bytes = device_link.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
        let caps = Capabilities {
            device_id: "sim-tile".into(),
            firmware: "simulator".into(),
            surfaces: vec![s()],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        device_link
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0x11, 0x22],
            })
            .unwrap();
        let bytes = device_link.read().unwrap().unwrap();
        let (pkt, _) = Packet::decode(&bytes, 4096).unwrap();
        assert_eq!(pkt.kind, MessageType::Tile);
        let tile = mdc_protocol::decode_tile_payload(&pkt.payload).unwrap();
        assert_eq!(tile.bytes, vec![0x11, 0x22]);
        // N506: the tile and its bytes are counted.
        let stats = session.stats();
        assert_eq!(stats.tiles_sent, 1);
        assert_eq!(stats.tile_bytes_sent, 2);
        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 2,
                height: 1,
                bytes: vec![0x33, 0x44, 0x55, 0x66],
            })
            .unwrap();
        let stats = session.stats();
        assert_eq!(stats.tiles_sent, 2);
        assert_eq!(stats.tile_bytes_sent, 6);
        // A tile for an unknown surface fails before it is counted.
        assert!(session
            .send_tile(Tile {
                surface_id: "nope".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0x00, 0x00],
            })
            .is_err());
        assert_eq!(session.stats().tiles_sent, 2);
    }

    #[test]
    fn device_tiles_received_folds_into_the_stats_snapshot() {
        // N547: the device's reported applied-tile count is part of one snapshot,
        // and the outstanding gap is derived there.
        let (host_link, mut device_link) = MemoryLink::pair();
        let mut session = Session::new(host_link);
        session.connect().unwrap();
        let hello_bytes = device_link.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
        write_caps(&mut device_link, hello.request_id, &caps());
        session.poll().unwrap();

        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0x11, 0x22],
            })
            .unwrap();
        assert_eq!(session.stats().tiles_sent, 1);
        assert_eq!(session.stats().device_tiles_received, 0, "nothing reported");
        assert_eq!(session.stats().tiles_missing, 1);

        // The device reports it applied the tile.
        let metric = MetricReport {
            name: TILES_RECEIVED_METRIC.into(),
            value: 1.0,
            unit: Some("tiles".into()),
            boot_id: 0,
            run_id: 0,
            event_seq: 1,
        };
        device_link
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Metric,
                    flags: 0,
                    request_id: 0,
                    payload: encode_control(&metric).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.stats().device_tiles_received, 1);
        assert_eq!(session.stats().tiles_missing, 0);
        let report = session.tile_reconciliation();
        assert_eq!((report.sent, report.received), (1, 1));
        assert_eq!(report.missing(), 0);
        assert!(report.consistent);
    }

    #[test]
    fn tile_reconciliation_threshold_forces_a_full_frame() {
        // N557: too many unacknowledged tiles set the full-frame barrier.
        let (host_link, mut device_link) = MemoryLink::pair();
        let mut session = Session::new(host_link);
        session.connect().unwrap();
        let hello_bytes = device_link.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
        write_caps(&mut device_link, hello.request_id, &caps());
        session.poll().unwrap();
        for (x, y) in [(0u16, 0u16), (1, 0), (0, 1)] {
            session
                .send_tile(Tile {
                    surface_id: "main".into(),
                    base_frame_id: 1,
                    x,
                    y,
                    width: 1,
                    height: 1,
                    bytes: vec![0x11, 0x22],
                })
                .unwrap();
        }
        let metric = MetricReport {
            name: TILES_RECEIVED_METRIC.into(),
            value: 1.0,
            unit: None,
            boot_id: 0,
            run_id: 0,
            event_seq: 1,
        };
        device_link
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Metric,
                    flags: 0,
                    request_id: 0,
                    payload: encode_control(&metric).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.tile_reconciliation().missing(), 2);
        // A generous threshold keeps tiling; a tight one forces a full frame.
        assert!(!session.tile_reconciliation().needs_full_frame(5));
        let report = session.enforce_tile_reconciliation(1);
        assert!(report.needs_full_frame(1));
        assert!(session.needs_full_frame);
        // The check itself is side-effect free when it passes.
        session.needs_full_frame = false;
        session.enforce_tile_reconciliation(5);
        assert!(!session.needs_full_frame);
    }

    #[test]
    fn connect_disconnect_loop_is_cheap() {
        for _ in 0..1000 {
            let (host_link, mut device_link) = MemoryLink::pair();
            let mut session = Session::new(host_link);
            session.connect().unwrap();
            let hello_bytes = device_link.read().unwrap().unwrap();
            let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
            let caps = Capabilities {
                device_id: "loop".into(),
                firmware: "simulator".into(),
                surfaces: vec![s()],
                frame: true,
                tile: true,
                touch: false,
                ota: false,
                max_message: 4096,
                max_chunk: 16384,
                max_in_flight: 1,
                max_fps: 30,
                mirror: mdc_protocol::MirrorCapability::disabled(),
                local_display_fps: 0,
                mirror_fps: 0,
                mirror_bandwidth: 0,
                renderer: None,
                config: mdc_protocol::ConfigCapability::default(),
                scene: mdc_protocol::SceneCapability::default(),
                boot_id: 0,
                run_id: 0,
            };
            device_link
                .write(
                    &Packet {
                        version: VERSION,
                        kind: MessageType::Capabilities,
                        flags: 0,
                        request_id: hello.request_id,
                        payload: encode_control(&caps).unwrap(),
                    }
                    .encode(4096)
                    .unwrap(),
                )
                .unwrap();
            assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
            session.disconnect();
            assert_eq!(session.state, ConnectionState::Disconnected);
        }
    }

    #[test]
    fn session_chunks_when_payload_exceeds_max_chunk() {
        let (host_link, mut device_link) = MemoryLink::pair();
        let mut session = Session::new(host_link);
        session.connect().unwrap();
        let hello_bytes = device_link.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello_bytes, 4096).unwrap();
        let caps = Capabilities {
            device_id: "sim-chunk".into(),
            firmware: "simulator".into(),
            surfaces: vec![Surface {
                id: "main".into(),
                width: 4,
                height: 2,
                pixel_format: "RGB565".into(),
                stride: 8,
                rotation: 0,
                preferred_tile_size: 0,
                supports_partial_update: false,
                supports_full_framebuffer: false,
            }],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 8,
            max_in_flight: 1,
            max_fps: 30,
            mirror: mdc_protocol::MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: mdc_protocol::ConfigCapability::default(),
            scene: mdc_protocol::SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        device_link
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        // 4*2*2 = 16 pixel bytes; legacy payload > max_chunk=8
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 4,
                height: 2,
                bytes: (0..16).collect(),
            })
            .unwrap();
        let mut phases = Vec::new();
        while let Some(bytes) = device_link.read().unwrap() {
            let (pkt, _) = Packet::decode(&bytes, 4096).unwrap();
            phases.push(pkt.payload[0]);
        }
        assert!(phases.contains(&mdc_protocol::FRAME_PHASE_BEGIN));
        assert!(phases.contains(&mdc_protocol::FRAME_PHASE_CHUNK));
        assert!(phases.contains(&mdc_protocol::FRAME_PHASE_COMMIT));
    }

    #[test]
    fn latest_unsent_frame_replaces_pending_slot() {
        let mut queue = SurfaceQueue::new(s());
        let frame = |value: u8| Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes: vec![value; 8],
        };
        assert_eq!(queue.enqueue_frame(frame(1)).unwrap(), 1);
        assert_eq!(queue.in_flight(), Some(1));
        assert_eq!(queue.enqueue_frame(frame(2)).unwrap(), 2);
        assert_eq!(queue.pending_frame_id(), Some(2));
        assert_eq!(queue.enqueue_frame(frame(3)).unwrap(), 3);
        assert_eq!(queue.in_flight(), Some(1));
        assert_eq!(queue.pending_frame_id(), Some(3));
        let (launched_id, launched) = queue.ack(1).unwrap();
        assert_eq!(launched_id, 3);
        assert_eq!(launched.bytes, vec![3; 8]);
        assert_eq!(queue.in_flight(), Some(3));
    }

    #[test]
    fn received_ack_holds_the_next_frame_until_displayed() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = device.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello, 4096).unwrap();
        let caps = caps();
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        session.needs_full_frame = true;
        let first = session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x11; 8],
            })
            .unwrap();
        let _ = device.read().unwrap().unwrap();
        assert_eq!(
            session.send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x22; 8],
            }),
            Ok(0)
        );
        assert!(device.read().unwrap().is_none());
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Ack,
                    flags: 0,
                    request_id: first,
                    payload: encode_control(&Ack {
                        received: true,
                        displayed: false,
                        frame_id: first as u64,
                        error: None,
                        run_id: 0,
                        event_seq: 0,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert!(device.read().unwrap().is_none());
        assert!(session.needs_full_frame);
        assert_eq!(
            session.send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0, 0],
            }),
            Err(CoreError::NeedFullFrame)
        );
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Ack,
                    flags: 0,
                    request_id: first,
                    payload: encode_control(&Ack {
                        received: true,
                        displayed: true,
                        frame_id: first as u64,
                        error: None,
                        run_id: 0,
                        event_seq: 0,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        let launched = device.read().unwrap().unwrap();
        assert!(launched.windows(2).any(|pair| pair == [0x22, 0x22]));
        assert!(!session.needs_full_frame);
    }

    #[test]
    fn three_missed_heartbeats_reconnect_and_block_tiles() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::with_config(
            host,
            SessionConfig {
                connect_timeout_ms: 10_000,
                ack_timeout_ms: 0,
                heartbeat_ms: 1_000,
                reconnect: true,
            },
        );
        session.reconnect_policy = ReconnectPolicy {
            max_attempts: 3,
            base_delay_ms: 1_000,
            max_delay_ms: 30_000,
            jitter_ms: 0,
        };
        assert_eq!(session.reconnect_policy.delay_ms(1, 0), Some(1_000));
        assert_eq!(session.reconnect_policy.delay_ms(2, 0), Some(2_000));
        assert_eq!(session.reconnect_policy.delay_ms(3, 0), Some(4_000));
        session.connect().unwrap();
        let hello = device.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello, 4096).unwrap();
        let caps = caps();
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        session.tick(1_000).unwrap();
        assert!(device.read().unwrap().is_some());
        session.tick(2_000).unwrap();
        assert!(device.read().unwrap().is_some());
        session.tick(3_000).unwrap();
        assert!(device.read().unwrap().is_some());
        assert_eq!(session.tick(4_000), Err(CoreError::HeartbeatMissed));
        assert_eq!(session.state, ConnectionState::Reconnecting);
        assert_eq!(session.tick(5_000).unwrap(), ConnectionState::Connecting);
        let (host2, mut device2) = MemoryLink::pair();
        session.provide_transport(host2).unwrap();
        assert!(session.needs_full_frame);
        let hello = device2.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello, 4096).unwrap();
        device2
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(
            session.send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0, 0],
            }),
            Err(CoreError::NeedFullFrame)
        );
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![7; 8],
            })
            .unwrap();
        let frame = device2.read().unwrap().unwrap();
        let (frame, _) = Packet::decode(&frame, 4096).unwrap();
        device2
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Ack,
                    flags: 0,
                    request_id: frame.request_id,
                    payload: encode_control(&Ack {
                        received: true,
                        displayed: true,
                        frame_id: frame.request_id as u64,
                        error: None,
                        run_id: 0,
                        event_seq: 0,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0, 0],
            })
            .unwrap();
    }

    #[test]
    fn thousand_sessions_and_frames_return_the_ledger_to_zero() {
        let mut ledger = AllocationLedger::default();
        let frame_bytes = 8usize;
        for _ in 0..1000 {
            assert!(ledger.alloc(frame_bytes));
            let (host, mut link) = MemoryLink::pair();
            let mut session = Session::new(host);
            session.connect().unwrap();
            let hello = link.read().unwrap().unwrap();
            let (hello, _) = Packet::decode(&hello, 4096).unwrap();
            link.write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&caps()).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
            session.poll().unwrap();
            session.disconnect();
            ledger.free(frame_bytes);
        }
        let (host, mut link) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = link.read().unwrap().unwrap();
        let (hello, _) = Packet::decode(&hello, 4096).unwrap();
        link.write(
            &Packet {
                version: VERSION,
                kind: MessageType::Capabilities,
                flags: 0,
                request_id: hello.request_id,
                payload: encode_control(&caps()).unwrap(),
            }
            .encode(4096)
            .unwrap(),
        )
        .unwrap();
        session.poll().unwrap();
        for value in 0..100u8 {
            assert!(ledger.alloc(frame_bytes));
            session
                .send_frame(Frame {
                    surface_id: "main".into(),
                    width: 2,
                    height: 2,
                    bytes: vec![value; frame_bytes],
                })
                .unwrap();
            let packet = link.read().unwrap().unwrap();
            let (packet, _) = Packet::decode(&packet, 4096).unwrap();
            link.write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Ack,
                    flags: 0,
                    request_id: packet.request_id,
                    payload: encode_control(&Ack {
                        received: true,
                        displayed: true,
                        frame_id: packet.request_id as u64,
                        error: None,
                        run_id: 0,
                        event_seq: 0,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
            session.poll().unwrap();
            ledger.free(frame_bytes);
        }
        session.disconnect();
        assert_eq!(ledger.outstanding, 0);
    }

    fn mirror_caps() -> Capabilities {
        let mut c = caps();
        c.mirror = mdc_protocol::MirrorCapability::full_frame(2, 2, 15);
        c.local_display_fps = 30;
        c.mirror_fps = 15;
        c.mirror_bandwidth = 2 * 2 * 2 * 15;
        c
    }

    fn write_caps(device: &mut MemoryLink, request_id: u32, caps: &Capabilities) {
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id,
                    payload: encode_control(caps).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
    }

    fn mirror_packet(kind: MessageType, payload: Vec<u8>) -> Vec<u8> {
        Packet {
            version: VERSION,
            kind,
            flags: 0,
            request_id: 0,
            payload,
        }
        .encode(4096)
        .unwrap()
    }

    #[test]
    fn mirror_start_is_rejected_without_capability() {
        let (host, mut dev) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        handshake(&mut session, &mut dev);
        assert!(!session.supports_mirror());
        assert_eq!(
            session.start_mirror("main", "full-frame", 15),
            Err(CoreError::Unsupported)
        );
        assert_eq!(session.stop_mirror("main"), Err(CoreError::Unsupported));
        assert!(dev.read().unwrap().is_none());
    }

    #[test]
    fn dirty_region_mirror_patches_the_last_frame() {
        let (host, mut dev) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        let mut caps = mirror_caps();
        caps.mirror.mode.push("dirty-region".into());
        write_caps(&mut dev, hello.request_id, &caps);
        session.poll().unwrap();
        session.start_mirror("main", "dirty-region", 30).unwrap();
        let ctrl = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        let cmd: mdc_protocol::MirrorCommand = mdc_protocol::decode_control(&ctrl.payload).unwrap();
        assert_eq!(cmd.mode, "dirty-region");

        // Full base frame (2x2, all 0x11).
        dev.write(&mirror_packet(
            MessageType::MirrorFrame,
            encode_frame_payload("main", 1, 2, 2, &[0x11; 8]).unwrap(),
        ))
        .unwrap();
        session.poll().unwrap();
        assert_eq!(session.take_mirror_frame().unwrap().bytes, vec![0x11; 8]);

        // Dirty update: only the first row (offsets 0..4) changes to 0x22.
        dev.write(&mirror_packet(
            MessageType::MirrorFrame,
            mdc_protocol::encode_frame_begin("main", 2, 2, 2, 8).unwrap(),
        ))
        .unwrap();
        dev.write(&mirror_packet(
            MessageType::MirrorFrame,
            mdc_protocol::encode_frame_chunk(2, 0, &[0x22; 4]),
        ))
        .unwrap();
        dev.write(&mirror_packet(
            MessageType::MirrorFrame,
            mdc_protocol::encode_frame_commit(2),
        ))
        .unwrap();
        session.poll().unwrap();
        let patched = session.take_mirror_frame().expect("patched frame");
        assert_eq!(
            patched.bytes,
            vec![0x22, 0x22, 0x22, 0x22, 0x11, 0x11, 0x11, 0x11]
        );
        assert_eq!(session.mirror_frames_received(), 2);
    }

    #[test]
    fn mirror_frames_are_bounded_to_latest() {
        let (host, mut dev) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        write_caps(&mut dev, hello.request_id, &mirror_caps());
        session.poll().unwrap();
        assert!(session.supports_mirror());
        session.start_mirror("main", "dirty-region", 30).unwrap();
        // negotiated down to full-frame at the device ceiling.
        let ctrl = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        assert_eq!(ctrl.kind, MessageType::MirrorCtrl);
        let cmd: mdc_protocol::MirrorCommand = mdc_protocol::decode_control(&ctrl.payload).unwrap();
        assert_eq!(cmd.mode, "full-frame");
        assert_eq!(cmd.max_fps, 15);

        for (frame_id, value) in [(17u64, 0x11u8), (34u64, 0x22u8)] {
            let payload = encode_frame_payload("main", frame_id, 2, 2, &[value; 8]).unwrap();
            dev.write(&mirror_packet(MessageType::MirrorFrame, payload))
                .unwrap();
            session.poll().unwrap();
        }
        let frame = session.take_mirror_frame().unwrap();
        assert_eq!(frame.frame_id, 34);
        assert_eq!(frame.bytes, vec![0x22; 8]);
        assert_eq!(session.mirror_frames_received(), 2);
        assert_eq!(session.mirror_frames_dropped(), 1);
        assert!(session.take_mirror_frame().is_none());
    }

    #[test]
    fn mirror_frames_emit_a_metadata_event_and_stats() {
        let (host, mut dev) = MemoryLink::pair();
        let mut session = Session::new(host);
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        session.set_manager(manager.clone());
        session.connect().unwrap();
        let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        write_caps(&mut dev, hello.request_id, &mirror_caps());
        session.poll().unwrap();

        let payload = encode_frame_payload("main", 7, 2, 2, &[0x33; 8]).unwrap();
        dev.write(&mirror_packet(MessageType::MirrorFrame, payload))
            .unwrap();
        session.poll().unwrap();

        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        assert!(
            events.iter().any(|event| matches!(
                event,
                ManagerEvent::Mirror {
                    frame_id: 7,
                    width: 2,
                    height: 2,
                    frames_received: 1,
                    ..
                }
            )),
            "{events:?}"
        );
        let stats = session.mirror_stats();
        assert_eq!(stats.frames_received, 1);
        assert_eq!(stats.frames_dropped, 0);
        assert_eq!(stats.last_frame_id, 7);
    }

    #[test]
    fn mirror_chunked_frame_assembles_and_rejects_bad_geometry() {
        let (host, mut dev) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = Packet::decode(&dev.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        write_caps(&mut dev, hello.request_id, &mirror_caps());
        session.poll().unwrap();

        let begin = encode_frame_begin("main", 5, 2, 2, 8).unwrap();
        dev.write(&mirror_packet(MessageType::MirrorFrame, begin))
            .unwrap();
        session.poll().unwrap();
        let chunk = encode_frame_chunk(5, 0, &[9; 8]);
        dev.write(&mirror_packet(MessageType::MirrorFrame, chunk))
            .unwrap();
        session.poll().unwrap();
        let commit = encode_frame_commit(5);
        dev.write(&mirror_packet(MessageType::MirrorFrame, commit))
            .unwrap();
        session.poll().unwrap();
        let frame = session.take_mirror_frame().unwrap();
        assert_eq!(frame.frame_id, 5);
        assert_eq!(frame.bytes, vec![9; 8]);

        // Geometry no surface advertises is dropped, not stored.
        let bad = encode_frame_payload("main", 6, 4, 2, &[1; 16]).unwrap();
        dev.write(&mirror_packet(MessageType::MirrorFrame, bad))
            .unwrap();
        session.poll().unwrap();
        assert!(session.take_mirror_frame().is_none());
        assert_eq!(session.mirror_frames_received(), 1);
    }

    #[test]
    fn device_reboot_discards_the_previous_run() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        let mut first = caps();
        first.boot_id = 100;
        first.run_id = 1;
        write_caps(&mut device, hello.request_id, &first);
        session.poll().unwrap();
        assert_eq!(session.device_boot_id(), Some(100));
        assert_eq!(session.device_run_id(), 1);

        // A frame becomes in-flight, so there is a pending ACK for this run.
        let request = session.send_frame(frame(&[1; 8])).unwrap();
        assert_ne!(request, 0);
        assert_eq!(session.pending_request_count(), 1);
        assert!(!session.needs_full_frame);

        // Same boot id -> no reset.
        write_caps(&mut device, 900, &first);
        session.poll().unwrap();
        assert_eq!(session.pending_request_count(), 1);

        // New boot id -> the old run's pending ACK is dropped and a full frame
        // is required again.
        let mut second = first.clone();
        second.boot_id = 101;
        second.run_id = 2;
        write_caps(&mut device, 901, &second);
        session.poll().unwrap();
        assert_eq!(session.device_boot_id(), Some(101));
        assert_eq!(session.device_run_id(), 2);
        assert_eq!(session.pending_request_count(), 0);
        assert!(session.needs_full_frame);
    }

    #[test]
    fn stale_run_ack_is_ignored() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        let mut run2 = caps();
        run2.boot_id = 7;
        run2.run_id = 2;
        write_caps(&mut device, hello.request_id, &run2);
        session.poll().unwrap();
        let request = session.send_frame(frame(&[2; 8])).unwrap();
        assert_eq!(session.pending_request_count(), 1);
        // An ACK stamped with run 1 is from the previous boot and must not clear
        // the request.
        let stale = Ack {
            received: true,
            displayed: true,
            frame_id: 1,
            error: None,
            run_id: 1,
            event_seq: 5,
        };
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Ack,
                    flags: 0,
                    request_id: request,
                    payload: encode_control(&stale).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.pending_request_count(), 1);
    }

    #[test]
    fn session_stats_count_frames_and_acks() {
        // N398: the host counters track outbound packets, frames, and received acks.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        assert_eq!(session.state, ConnectionState::Ready);
        assert!(session.stats().packets_sent >= 1, "hello counted");

        let before = session.stats();
        let request = session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0u8; 8],
            })
            .unwrap();
        assert_eq!(session.stats().frames_sent, before.frames_sent + 1);
        assert!(session.stats().bytes_sent > before.bytes_sent);

        // The peer acknowledges (received + displayed).
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Ack,
                    flags: 0,
                    request_id: request,
                    payload: encode_control(&mdc_protocol::Ack {
                        received: true,
                        displayed: true,
                        frame_id: 0,
                        error: None,
                        run_id: 0,
                        event_seq: 1,
                    })
                    .unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert!(session.stats().acks_received >= 1, "the frame was acked");
        // N436: inbound bytes are counted too.
        assert!(session.stats().bytes_received > 0, "received bytes counted");
        // N446: the package/mirror/pacing counters are exposed.
        let _ = (
            session.stats().packages_sent,
            session.stats().mirror_frames,
            session.stats().pacing_dropped,
        );
    }

    #[test]
    fn set_scene_gates_on_the_animation_budget() {
        // N477: a scene with more keys than the device allows is refused.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let mut advertised = caps();
        advertised.scene = mdc_protocol::SceneCapability::enabled_with_animation(64, 4, 1);
        let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&advertised).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.state, ConnectionState::Ready);
        let scene = |keys: u64| mdc_protocol::SceneNode {
            name: "root".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 2, 2]),
            value: Some("#fff".into()),
            value2: None,
            opacity: None,
            visible: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            anim: Some(mdc_protocol::SceneAnim {
                duration_ms: 100,
                looping: true,
                opacity: (0..keys).map(|k| [k as u16, 128]).collect(),
                rect: Vec::new(),
                focus: Vec::new(),
                delay_ms: None,
                speed_permille: None,
                color: Vec::new(),
                rotate: Vec::new(),
                easing: None,
            }),
            children: Vec::new(),
        };
        assert!(session.set_scene(scene(1)).is_ok(), "one key fits");
        assert_eq!(session.set_scene(scene(2)), Err(CoreError::TooLarge));
    }

    #[test]
    fn gesture_config_tunes_the_tap_window() {
        // N488: a short tap window rejects a slow press.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        let manager = std::sync::Arc::new(std::sync::Mutex::new(DeviceManager::new()));
        session.set_manager(manager.clone());
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        session.set_gesture_config(GestureConfig {
            tap_max_ms: 10,
            long_press_ms: 5_000,
            drag_threshold: 6,
            multi_tap_ms: 300,
            multi_tap_radius: 24,
        });
        assert_eq!(session.gesture_config().tap_max_ms, 10);
        let input = |phase: &str| mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: phase.into(),
            x: 1,
            y: 1,
            node: None,
        };
        // Poll at t=0 (down), then advance past the tap window before "up".
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Input,
                    flags: 0,
                    request_id: 1,
                    payload: encode_control(&input("down")).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        session.tick(500).unwrap();
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Input,
                    flags: 0,
                    request_id: 2,
                    payload: encode_control(&input("up")).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        let events: Vec<ManagerEvent> = manager.lock().unwrap().drain_events().collect();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ManagerEvent::Gesture { .. })),
            "a slow press is not a tap"
        );
    }

    #[test]
    fn multi_tap_window_from_the_setter_splits_chains() {
        // N804: the window stored by `set_gesture_config` decides whether the
        // next tap escalates. A tap inside it is a double-tap; a tap outside it
        // starts a new chain. The radius already on the session stays put.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        let manager = std::sync::Arc::new(std::sync::Mutex::new(DeviceManager::new()));
        session.set_manager(manager.clone());
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        let mut config = session.gesture_config();
        let radius = config.multi_tap_radius;
        config.multi_tap_ms = 100;
        session.set_gesture_config(config);
        assert_eq!(session.gesture_config().multi_tap_ms, 100);
        assert_eq!(session.gesture_config().multi_tap_radius, radius);
        let input = |phase: &str| mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: phase.into(),
            x: 4,
            y: 5,
            node: None,
        };
        let mut request = 1u32;
        let mut tap_at = |at: u64| {
            session.tick(at).unwrap();
            for phase in ["down", "up"] {
                device
                    .write(
                        &Packet {
                            version: VERSION,
                            kind: MessageType::Input,
                            flags: 0,
                            request_id: request,
                            payload: encode_control(&input(phase)).unwrap(),
                        }
                        .encode(4096)
                        .unwrap(),
                    )
                    .unwrap();
                request += 1;
            }
            session.poll().unwrap();
            let events: Vec<ManagerEvent> = manager.lock().unwrap().drain_events().collect();
            events.into_iter().find_map(|event| match event {
                ManagerEvent::Gesture { gesture, .. } => Some(gesture.kind),
                _ => None,
            })
        };
        assert_eq!(tap_at(0), Some(GestureKind::Tap));
        assert_eq!(tap_at(100), Some(GestureKind::DoubleTap));
        // N814: a third tap inside the stored window is a triple-tap.
        assert_eq!(tap_at(200), Some(GestureKind::TripleTap));
        // The triple resets the streak; a tap outside the window restarts at 1.
        assert_eq!(tap_at(350), Some(GestureKind::Tap));
        assert_eq!(tap_at(430), Some(GestureKind::DoubleTap));
        assert_eq!(tap_at(600), Some(GestureKind::Tap));
        assert_eq!(tap_at(680), Some(GestureKind::DoubleTap));
    }

    #[test]
    fn gestures_reach_the_manager_event_queue() {
        // N473: down+up inside the tap window becomes a `Gesture` event.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        let manager = std::sync::Arc::new(std::sync::Mutex::new(DeviceManager::new()));
        session.set_manager(manager.clone());
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        let input = |phase: &str| mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id: 3,
            phase: phase.into(),
            x: 4,
            y: 5,
            // N493: the device reports the hit node with the event.
            node: Some("btn".into()),
        };
        for (phase, id) in [("down", 10u32), ("up", 11)] {
            device
                .write(
                    &Packet {
                        version: VERSION,
                        kind: MessageType::Input,
                        flags: 0,
                        request_id: id,
                        payload: encode_control(&input(phase)).unwrap(),
                    }
                    .encode(4096)
                    .unwrap(),
                )
                .unwrap();
        }
        session.poll().unwrap();
        let events: Vec<ManagerEvent> = manager.lock().unwrap().drain_events().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            ManagerEvent::Gesture {
                gesture: Gesture {
                    kind: GestureKind::Tap,
                    pointer_id: 3,
                    x: 4,
                    y: 5,
                    node: Some(node),
                    ..
                },
                ..
            } if node == "btn"
        )));
    }

    #[test]
    fn send_input_requires_the_touch_capability() {
        // N400: `caps()` advertises touch: false, so the send is refused.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        let event = mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: "down".into(),
            x: 1,
            y: 2,

            node: None,
        };
        assert_eq!(session.send_input(event), Err(CoreError::Unsupported));
    }

    #[test]
    fn gesture_tracker_recognises_tap_drag_and_long_press() {
        // N464: tap, drag, and long-press derive from raw input events.
        let event = |phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: phase.into(),
            x,
            y,
            node: None,
        };
        let mut tracker = GestureTracker::new(GestureConfig::default());
        assert_eq!(tracker.on_event(&event("down", 10, 10), 0), None);
        assert_eq!(
            tracker.on_event(&event("up", 10, 10), 100),
            Some(Gesture {
                kind: GestureKind::Tap,
                pointer_id: 1,
                x: 10,
                y: 10,
                node: None,
                scale: 1000,
                rotation: 0,
                dx: 0,
                dy: 0,
            })
        );

        // Moving beyond the threshold becomes a drag.
        assert_eq!(tracker.on_event(&event("down", 10, 10), 200), None);
        assert_eq!(
            tracker.on_event(&event("move", 20, 10), 220),
            Some(Gesture {
                kind: GestureKind::Drag,
                pointer_id: 1,
                x: 20,
                y: 10,
                node: None,
                scale: 1000,
                rotation: 0,
                dx: 0,
                dy: 0,
            })
        );
        assert_eq!(
            tracker.on_event(&event("up", 20, 10), 240),
            Some(Gesture {
                kind: GestureKind::Drag,
                pointer_id: 1,
                x: 20,
                y: 10,
                node: None,
                scale: 1000,
                rotation: 0,
                dx: 0,
                dy: 0,
            })
        );

        // Holding past the long-press window emits once.
        assert_eq!(tracker.on_event(&event("down", 5, 5), 1000), None);
        assert_eq!(tracker.tick(1500), None);
        assert_eq!(
            tracker.tick(1600),
            Some(Gesture {
                kind: GestureKind::LongPress,
                pointer_id: 1,
                x: 5,
                y: 5,
                node: None,
                scale: 1000,
                rotation: 0,
                dx: 0,
                dy: 0,
            })
        );
        assert_eq!(tracker.tick(2000), None, "long-press emits once");
        assert_eq!(tracker.on_event(&event("up", 5, 5), 2100), None);
    }

    #[test]
    fn gesture_tracker_recognises_a_two_pointer_pinch() {
        // N725: two pointers derive scale/rotation from their distance/angle;
        // the release is not a tap, and the midpoint is reported.
        let event = |pointer_id: u16, phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id,
            phase: phase.into(),
            x,
            y,
            node: None,
        };
        let mut tracker = GestureTracker::new(GestureConfig::default());
        assert_eq!(tracker.on_event(&event(1, "down", 10, 10), 0), None);
        assert_eq!(tracker.on_event(&event(2, "down", 20, 10), 10), None);
        // Distance doubles: 10 -> 20 px, angle unchanged.
        assert_eq!(
            tracker.on_event(&event(2, "move", 30, 10), 20),
            Some(Gesture {
                kind: GestureKind::Pinch,
                pointer_id: 2,
                x: 20,
                y: 10,
                node: None,
                scale: 2000,
                rotation: 0,
                dx: 0,
                dy: 0,
            })
        );
        // Diagonal: distance 10*sqrt(2), angle 45 degrees (scale ~1414).
        let gesture = tracker.on_event(&event(2, "move", 20, 20), 30).unwrap();
        assert_eq!(gesture.kind, GestureKind::Pinch);
        assert_eq!(gesture.scale, 1414);
        assert_eq!(gesture.rotation, 45);
        assert_eq!((gesture.x, gesture.y), (15, 15));
        // Both fingers are consumed by the pinch: no tap on release.
        assert_eq!(tracker.on_event(&event(2, "up", 20, 20), 40), None);
        assert_eq!(tracker.on_event(&event(1, "up", 10, 10), 50), None);
    }

    #[test]
    fn pinch_reaches_the_manager_event_queue() {
        // N725: the Session maps pinch into ManagerEvent::Gesture.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        let manager = std::sync::Arc::new(std::sync::Mutex::new(DeviceManager::new()));
        session.set_manager(manager.clone());
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        let input = |pointer_id: u16, phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id,
            phase: phase.into(),
            x,
            y,
            node: None,
        };
        for (index, event) in [
            input(1, "down", 10, 10),
            input(2, "down", 20, 10),
            input(2, "move", 30, 10),
        ]
        .into_iter()
        .enumerate()
        {
            device
                .write(
                    &Packet {
                        version: VERSION,
                        kind: MessageType::Input,
                        flags: 0,
                        request_id: index as u32 + 1,
                        payload: encode_control(&event).unwrap(),
                    }
                    .encode(4096)
                    .unwrap(),
                )
                .unwrap();
            session.poll().unwrap();
        }
        let events: Vec<ManagerEvent> = manager.lock().unwrap().drain_events().collect();
        let pinch = events
            .iter()
            .find_map(|event| match event {
                ManagerEvent::Gesture { gesture, .. } if gesture.kind == GestureKind::Pinch => {
                    Some(gesture)
                }
                _ => None,
            })
            .expect("pinch reaches the manager");
        assert_eq!(pinch.scale, 2000);
        assert_eq!(pinch.rotation, 0);
    }

    #[test]
    fn two_pointer_move_emits_pan_and_pinch_together() {
        // N735: one two-finger move reports both the midpoint translation and
        // the scale/rotation change.
        let event = |pointer_id: u16, phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id,
            phase: phase.into(),
            x,
            y,
            node: None,
        };
        let mut tracker = GestureTracker::new(GestureConfig::default());
        assert!(tracker.on_events(&event(1, "down", 10, 10), 0).is_empty());
        assert!(tracker.on_events(&event(2, "down", 20, 10), 10).is_empty());
        let gestures = tracker.on_events(&event(2, "move", 30, 20), 20);
        assert_eq!(gestures.len(), 2, "{gestures:?}");
        assert_eq!(gestures[0].kind, GestureKind::Pan);
        assert_eq!((gestures[0].x, gestures[0].y), (20, 15));
        assert_eq!((gestures[0].dx, gestures[0].dy), (5, 5));
        assert_eq!(gestures[1].kind, GestureKind::Pinch);
        assert_eq!(gestures[1].scale, 2236);
        assert_eq!(gestures[1].rotation, 27);
        // The single-gesture API still reports the richer pinch.
        let mut tracker = GestureTracker::new(GestureConfig::default());
        tracker.on_events(&event(1, "down", 10, 10), 0);
        tracker.on_events(&event(2, "down", 20, 10), 10);
        assert_eq!(
            tracker
                .on_event(&event(2, "move", 30, 20), 20)
                .unwrap()
                .kind,
            GestureKind::Pinch
        );
    }

    #[test]
    fn three_pointer_swipe_reports_direction() {
        // N745: three fingers moving together emit a Swipe when the midpoint
        // travels beyond the threshold; releases do not become taps.
        let event = |pointer_id: u16, phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id,
            phase: phase.into(),
            x,
            y,
            node: None,
        };
        let mut tracker = GestureTracker::new(GestureConfig::default());
        for (id, x, y) in [(1, 10, 10), (2, 20, 10), (3, 10, 20)] {
            assert!(tracker.on_events(&event(id, "down", x, y), 0).is_empty());
        }
        let gestures = tracker.on_events(&event(3, "move", 40, 20), 10);
        assert_eq!(gestures.len(), 1);
        assert_eq!(gestures[0].kind, GestureKind::Swipe);
        assert_eq!((gestures[0].dx, gestures[0].dy), (10, 0));
        assert_eq!((gestures[0].x, gestures[0].y), (23, 13));
        assert!(tracker.on_events(&event(3, "up", 40, 20), 20).is_empty());
        assert!(tracker.on_events(&event(2, "up", 20, 10), 30).is_empty());
        assert!(tracker.on_events(&event(1, "up", 10, 10), 40).is_empty());
    }

    #[test]
    fn double_and_triple_taps_share_a_window() {
        // N755: taps inside the window escalate to double/triple and then reset;
        // a tap after the window starts a fresh streak.
        let event = |phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: phase.into(),
            x,
            y,
            node: None,
        };
        let tap = |tracker: &mut GestureTracker, at: u64| {
            tracker.on_event(&event("down", 5, 5), at);
            tracker
                .on_event(&event("up", 5, 5), at + 50)
                .map(|g| g.kind)
        };
        let mut tracker = GestureTracker::new(GestureConfig::default());
        assert_eq!(tap(&mut tracker, 0), Some(GestureKind::Tap));
        assert_eq!(tap(&mut tracker, 100), Some(GestureKind::DoubleTap));
        assert_eq!(tap(&mut tracker, 200), Some(GestureKind::TripleTap));
        // The fourth tap starts a new streak, because the triple reset it.
        assert_eq!(tap(&mut tracker, 300), Some(GestureKind::Tap));
        // A slow gap resets to a single tap.
        assert_eq!(tap(&mut tracker, 2_000), Some(GestureKind::Tap));
        // The next tap inside the window is a double again.
        assert_eq!(tap(&mut tracker, 2_200), Some(GestureKind::DoubleTap));
    }

    #[test]
    fn drag_and_long_press_break_the_tap_chain() {
        // N764: only uninterrupted taps escalate; a drag, long press, cancel or
        // multi-touch sequence starts a fresh chain.
        let event = |phase: &str, x: u16, y: u16| mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: phase.into(),
            x,
            y,
            node: None,
        };
        let mut tracker = GestureTracker::new(GestureConfig::default());
        let tap = |tracker: &mut GestureTracker, at: u64| {
            tracker.on_event(&event("down", 5, 5), at);
            tracker
                .on_event(&event("up", 5, 5), at + 40)
                .map(|g| g.kind)
        };
        assert_eq!(tap(&mut tracker, 0), Some(GestureKind::Tap));
        assert_eq!(tap(&mut tracker, 80), Some(GestureKind::DoubleTap));
        // A drag between taps must not turn the next tap into a triple.
        tracker.on_event(&event("down", 5, 5), 160);
        tracker.on_event(&event("move", 20, 5), 170);
        tracker.on_event(&event("up", 20, 5), 180);
        assert_eq!(tap(&mut tracker, 240), Some(GestureKind::Tap));

        // A long press resets too.
        assert_eq!(tap(&mut tracker, 320), Some(GestureKind::DoubleTap));
        tracker.on_event(&event("down", 5, 5), 400);
        assert!(tracker.tick(1_100).is_some(), "long press emitted");
        tracker.on_event(&event("up", 5, 5), 1_150);
        assert_eq!(tap(&mut tracker, 1_250), Some(GestureKind::Tap));

        // A cancel resets even if the release would have been a tap.
        assert_eq!(tap(&mut tracker, 1_330), Some(GestureKind::DoubleTap));
        tracker.on_event(&event("down", 5, 5), 1_400);
        tracker.on_event(&event("cancel", 5, 5), 1_410);
        assert_eq!(tap(&mut tracker, 1_450), Some(GestureKind::Tap));
    }

    #[test]
    fn multi_tap_requires_the_same_node_and_nearby_position() {
        // N774: time alone is not enough; a distant tap or a tap on another
        // node starts a new chain instead of escalating.
        let event = |phase: &str, x: u16, y: u16, node: Option<&str>| mdc_protocol::InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: phase.into(),
            x,
            y,
            node: node.map(str::to_string),
        };
        let tap = |tracker: &mut GestureTracker, at: u64, x: u16, y: u16, node: Option<&str>| {
            tracker.on_event(&event("down", x, y, node), at);
            tracker
                .on_event(&event("up", x, y, node), at + 40)
                .map(|g| g.kind)
        };
        let mut tracker = GestureTracker::new(GestureConfig::default());
        assert_eq!(
            tap(&mut tracker, 0, 10, 10, Some("a")),
            Some(GestureKind::Tap)
        );
        assert_eq!(
            tap(&mut tracker, 80, 12, 11, Some("a")),
            Some(GestureKind::DoubleTap)
        );
        // A different node resets even at the same point.
        assert_eq!(
            tap(&mut tracker, 160, 10, 10, Some("b")),
            Some(GestureKind::Tap)
        );
        // Too far away resets too.
        assert_eq!(
            tap(&mut tracker, 240, 40, 10, Some("b")),
            Some(GestureKind::Tap)
        );
        // Same node and nearby position escalates again.
        assert_eq!(
            tap(&mut tracker, 320, 42, 12, Some("b")),
            Some(GestureKind::DoubleTap)
        );
    }

    #[test]
    fn ota_chunk_ranges_and_missing_ranges() {
        // N737: fixed chunking shortens the final range and subtracts confirmed
        // byte ranges exactly.
        assert_eq!(
            Session::<MemoryLink>::ota_chunk_ranges(10, 4),
            vec![(0, 4), (4, 4), (8, 2)]
        );
        assert_eq!(
            Session::<MemoryLink>::ota_missing_chunks(10, 4, &[(0, 4)]),
            vec![(4, 4), (8, 2)]
        );
        assert_eq!(
            Session::<MemoryLink>::ota_missing_chunks(10, 4, &[(0, 10)]),
            Vec::<(u32, u32)>::new()
        );
    }

    #[test]
    fn ota_chunked_resume_plan_skips_acknowledged_chunks() {
        // N737: at phase 2 the plan contains only missing chunks plus confirm.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        let mut advertised = caps();
        advertised.ota = true;
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&advertised).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        let plan = session.ota_resume_commands_chunked("2.0.0", 10, 4, &[]);
        assert_eq!(plan.len(), 5, "begin + three chunks + confirm");
        assert_eq!(plan[0].action, "begin");
        session.send_ota(plan[0].clone()).unwrap();
        session.send_ota(plan[1].clone()).unwrap();
        let plan = session.ota_resume_commands_chunked("2.0.0", 10, 4, &[(0, 4)]);
        assert_eq!(plan.len(), 3, "two missing chunks + confirm");
        assert!(plan[..2].iter().all(|command| command.action == "chunk"));
        assert_eq!(plan[0].offset, Some(4));
        assert_eq!(plan[1].offset, Some(8));
        assert_eq!(plan[2].action, "confirm");
    }

    #[test]
    fn ota_phase_checkpoint_resumes_across_reconnect() {
        // N419: the OTA phase survives a reconnect so a driver can resume.
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        // `caps()` disables OTA; advertise it so `send_ota` is allowed.
        let mut advertised = caps();
        advertised.ota = true;
        let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Capabilities,
                    flags: 0,
                    request_id: hello.request_id,
                    payload: encode_control(&advertised).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.state, ConnectionState::Ready);
        assert_eq!(session.ota_phase(), 0);

        let command = |action: &str| mdc_protocol::OtaCommand {
            action: action.into(),
            version: None,
            size: None,
            sha256: None,
            offset: None,
            len: None,
        };
        session.send_ota(command("begin")).unwrap();
        assert_eq!(session.ota_phase(), 1);
        session.send_ota(command("chunk")).unwrap();
        assert_eq!(session.ota_phase(), 2);
        // A reconnect edge must not clear the checkpoint.
        session.enable_reconnect(true);
        assert_eq!(session.ota_phase(), 2, "phase survives reconnect");
        // N728: the resume plan only contains the unacknowledged confirm.
        let plan = session.ota_resume_commands("2.0.0", None, None);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].action, "confirm");
        session.reset_ota();
        assert_eq!(session.ota_phase(), 0);
        assert_eq!(
            session.ota_resume_commands("2.0.0", None, None).len(),
            3,
            "a reset starts from begin"
        );
    }

    #[test]
    fn diagnostics_stay_apart_from_the_frame_path() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        let mut boot_caps = caps();
        boot_caps.boot_id = 500;
        boot_caps.run_id = 1;
        write_caps(&mut device, hello.request_id, &boot_caps);
        session.poll().unwrap();
        let _request = session.send_frame(frame(&[3; 8])).unwrap();
        assert_eq!(session.pending_request_count(), 1);

        let metric = MetricReport {
            name: "heap".into(),
            value: 1024.0,
            unit: Some("bytes".into()),
            boot_id: 0,
            run_id: 0,
            event_seq: 1,
        };
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Metric,
                    flags: 0,
                    request_id: 0,
                    payload: encode_control(&metric).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.diagnostic_count(), 1);
        assert_eq!(
            session.pending_request_count(),
            1,
            "a metric must not clear a frame ACK"
        );
        assert_eq!(
            session.take_diagnostics(),
            vec![DiagnosticRecord::Metric(metric)]
        );
        assert_eq!(session.diagnostic_count(), 0);

        // The channel is bounded: the 65th record drops the oldest.
        for seq in 0..(MAX_DIAGNOSTICS as u64 + 5) {
            let metric = MetricReport {
                name: "seq".into(),
                value: seq as f64,
                unit: None,
                boot_id: 0,
                run_id: 0,
                event_seq: seq,
            };
            device
                .write(
                    &Packet {
                        version: VERSION,
                        kind: MessageType::Metric,
                        flags: 0,
                        request_id: 0,
                        payload: encode_control(&metric).unwrap(),
                    }
                    .encode(4096)
                    .unwrap(),
                )
                .unwrap();
            session.poll().unwrap();
        }
        assert_eq!(session.diagnostic_count(), MAX_DIAGNOSTICS);
        let records = session.take_diagnostics();
        assert_eq!(
            records.first(),
            Some(&DiagnosticRecord::Metric(MetricReport {
                name: "seq".into(),
                value: 5.0,
                unit: None,
                boot_id: 0,
                run_id: 0,
                event_seq: 5,
            }))
        );

        // A BootInfo (N316) with a new boot id discards the old run.
        let boot = BootInfo {
            firmware: "2.0.0".into(),
            boot_id: 999,
            run_id: 7,
            reset_reason: Some("watchdog".into()),
        };
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::BootInfo,
                    flags: 0,
                    request_id: 0,
                    payload: encode_control(&boot).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.device_boot_id(), Some(999));
        assert_eq!(session.pending_request_count(), 0);
        assert!(session.needs_full_frame);
        let records = session.take_diagnostics();
        assert!(matches!(
            records.as_slice(),
            [DiagnosticRecord::BootInfo(_)]
        ));
    }

    #[test]
    fn telemetry_also_emits_a_manager_event() {
        let (host, mut device) = MemoryLink::pair();
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        write_caps(&mut device, hello.request_id, &caps());
        session.poll().unwrap();
        let event = mdc_protocol::TelemetryEvent {
            name: "soak".into(),
            level: Some("info".into()),
            fields: std::collections::BTreeMap::new(),
            boot_id: 0,
            run_id: 0,
            event_seq: 1,
        };
        device
            .write(
                &Packet {
                    version: VERSION,
                    kind: MessageType::Telemetry,
                    flags: 0,
                    request_id: 0,
                    payload: encode_control(&event).unwrap(),
                }
                .encode(4096)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        // The full record is in the diagnostics queue ...
        assert!(matches!(
            session.take_diagnostics().as_slice(),
            [DiagnosticRecord::Telemetry(_)]
        ));
        // ... and a metadata-only event is on the manager queue (N357).
        let events: Vec<ManagerEvent> = manager.lock().unwrap().drain_events().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            ManagerEvent::Telemetry { name, level, .. }
                if name == "soak" && level.as_deref() == Some("info")
        )));
    }

    #[test]
    fn frame_pacing_clamps_fps_and_can_be_cleared() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        handshake(&mut session, &mut device);
        session.set_frame_pacing(8, 0); // 125 ms interval, unlimited bytes
                                        // First frame at virtual 0 is sent.
        assert!(session.send_frame(frame(&[1; 8])).is_ok());
        // 10 ms later it is too soon.
        session.tick(10).unwrap();
        assert_eq!(
            session.send_frame(frame(&[2; 8])),
            Err(CoreError::Throttled)
        );
        assert_eq!(
            session.pacing_dropped(),
            0,
            "the fps gate is not a budget drop"
        );
        // At the interval the next frame passes.
        session.tick(125).unwrap();
        assert!(session.send_frame(frame(&[3; 8])).is_ok());
        // Clearing the pacing removes all gating.
        session.clear_frame_pacing();
        session.tick(126).unwrap();
        assert!(session.send_frame(frame(&[4; 8])).is_ok());
    }

    #[test]
    fn frame_pacing_drops_a_frame_over_the_byte_budget() {
        let (host, mut device) = MemoryLink::pair();
        let mut session = Session::new(host);
        session.connect().unwrap();
        // A 40x40 surface so one frame spans several 1 KiB tiles.
        let mut custom = caps();
        custom.surfaces[0] = Surface {
            width: 40,
            height: 40,
            ..s()
        };
        let hello = Packet::decode(&device.read().unwrap().unwrap(), 4096)
            .unwrap()
            .0;
        write_caps(&mut device, hello.request_id, &custom);
        session.poll().unwrap();
        // 8 FPS -> 125 ms -> 8 B/s budget = 1 byte per interval, far below a frame.
        session.set_frame_pacing(8, 8);
        let big = Frame {
            surface_id: "main".into(),
            width: 40,
            height: 40,
            bytes: vec![0; 40 * 40 * 2],
        };
        assert_eq!(session.send_frame(big), Err(CoreError::Throttled));
        assert_eq!(session.pacing_dropped(), 1);
    }

    #[test]
    fn preferred_tile_size_drives_the_host_cut() {
        // No advertised preference -> the host's default edge (32).
        let mut c = caps();
        c.surfaces[0] = Surface {
            width: 70,
            height: 40,
            ..s()
        };
        let device = Device::new(c);
        assert_eq!(
            device.plan_surface_tiles("main"),
            vec![
                (0, 0, 32, 32),
                (32, 0, 32, 32),
                (64, 0, 6, 32),
                (0, 32, 32, 8),
                (32, 32, 32, 8),
                (64, 32, 6, 8),
            ]
        );
        // The device's preferred tile size is used when present.
        let mut c2 = caps();
        c2.surfaces[0] = Surface {
            width: 40,
            height: 40,
            preferred_tile_size: 20,
            ..s()
        };
        let device2 = Device::new(c2);
        assert_eq!(
            device2.plan_surface_tiles("main"),
            vec![
                (0, 0, 20, 20),
                (20, 0, 20, 20),
                (0, 20, 20, 20),
                (20, 20, 20, 20)
            ]
        );
        // Degenerate surfaces never produce tiles.
        assert!(plan_tiles(0, 10, 32).is_empty());
        assert!(plan_tiles(10, 0, 32).is_empty());
        assert!(device.plan_surface_tiles("missing").is_empty());
    }

    #[test]
    fn unimplemented_pixel_format_is_rejected_explicitly() {
        let mut c = caps();
        c.surfaces[0].pixel_format = mdc_protocol::PixelFormat::Rgba8888;
        let device = Device::new(c);
        let error = device.validate_frame(&frame(&[0; 8])).unwrap_err();
        assert_eq!(error, CoreError::Format("RGBA8888".into()));
    }
}
