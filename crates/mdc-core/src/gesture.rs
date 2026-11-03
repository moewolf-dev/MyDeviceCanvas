//! Gesture recognition state (tap/long-press/drag/pinch/pan/swipe).
//!
//! Pure input logic: no transport, no session state.
use super::*;

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
pub(crate) struct PointerState {
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
pub(crate) struct PinchAnchor {
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
    pub(crate) pointers: std::collections::BTreeMap<u16, PointerState>,
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
