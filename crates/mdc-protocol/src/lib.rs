//! MyDeviceCanvas Protocol v1. Binary images never pass through CBOR.
mod geometry;
pub use geometry::{canvas_bytes_per_pixel, dual_rgb565_budget, rgb565_bytes};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAGIC: [u8; 2] = *b"MD";
pub const HEADER_LEN: usize = 20;
pub const MAX_CONTROL_PAYLOAD: usize = 4096;
pub const DEFAULT_MAX_CHUNK: usize = 16 * 1024;

/// Frame payload phase byte (leading u8). Prefer this over MessageType flags.
pub const FRAME_PHASE_LEGACY: u8 = 0;
pub const FRAME_PHASE_BEGIN: u8 = 1;
pub const FRAME_PHASE_CHUNK: u8 = 2;
pub const FRAME_PHASE_COMMIT: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
}
pub const VERSION: Version = Version { major: 1, minor: 0 };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Hello = 1,
    Capabilities = 2,
    /// Host -> device image (Form A). Binary payload, phase byte like Frame.
    Frame = 0x10,
    Tile = 0x11,
    /// Device -> host image (Form B mirror). Binary payload, phase byte like Frame.
    /// Kept separate from Frame so a receiver can never confuse a pushed frame
    /// with a mirrored frame.
    MirrorFrame = 0x12,
    Input = 0x20,
    Ota = 0x30,
    /// Host -> device mirror start/stop control (CBOR MirrorCommand, Form B).
    MirrorCtrl = 0x31,
    /// Host -> device runtime package transfer (phase byte like Frame, Form B).
    Package = 0x32,
    /// Host -> device configuration get/set (CBOR ConfigCommand, N39).
    Config = 0x33,
    /// Host -> device vector scene (CBOR SceneCommand, N53).
    Scene = 0x34,
    Ping = 0x40,
    Pong = 0x41,
    Ack = 0x50,
    /// N303: device -> host diagnostic/telemetry types reserved for HIL testing
    /// (report 1). The decoder accepts the values now so the wire enum is stable
    /// for downstream pins, but no payload is implemented in this batch. A host
    /// keeps diagnostics on a channel independent of the link under test; Core
    /// currently ignores the payloads (see `docs/transport-scope.md`).
    Telemetry = 0x60,
    Metric = 0x61,
    Crash = 0x62,
    BootInfo = 0x63,
    Error = 0x7f,
}
impl MessageType {
    /// N303: a reserved diagnostic/telemetry type whose payload is not decoded yet.
    pub fn is_diagnostic(self) -> bool {
        matches!(
            self,
            Self::Telemetry | Self::Metric | Self::Crash | Self::BootInfo
        )
    }
}
impl TryFrom<u8> for MessageType {
    type Error = CodecError;
    fn try_from(v: u8) -> Result<Self, CodecError> {
        match v {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Capabilities),
            0x10 => Ok(Self::Frame),
            0x11 => Ok(Self::Tile),
            0x12 => Ok(Self::MirrorFrame),
            0x20 => Ok(Self::Input),
            0x30 => Ok(Self::Ota),
            0x31 => Ok(Self::MirrorCtrl),
            0x32 => Ok(Self::Package),
            0x33 => Ok(Self::Config),
            0x34 => Ok(Self::Scene),
            0x40 => Ok(Self::Ping),
            0x41 => Ok(Self::Pong),
            0x50 => Ok(Self::Ack),
            0x60 => Ok(Self::Telemetry),
            0x61 => Ok(Self::Metric),
            0x62 => Ok(Self::Crash),
            0x63 => Ok(Self::BootInfo),
            0x7f => Ok(Self::Error),
            _ => Err(CodecError::UnknownType(v)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub version: Version,
    pub kind: MessageType,
    pub flags: u16,
    pub request_id: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("invalid magic")]
    InvalidMagic,
    #[error("reserved header byte is not zero")]
    Reserved,
    #[error("flags must be zero")]
    Flags,
    #[error("truncated packet")]
    Truncated,
    #[error("unsupported major version {0}")]
    Major(u8),
    #[error("unknown message type {0}")]
    UnknownType(u8),
    #[error("payload length exceeds limit {0}")]
    Length(usize),
    #[error("checksum mismatch")]
    Checksum,
    #[error("unknown frame phase {0}")]
    UnknownPhase(u8),
}

fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c9dc5u32, |h, b| {
        h.wrapping_mul(16777619) ^ u32::from(*b)
    })
}
impl Packet {
    pub fn encode(&self, max_payload: usize) -> Result<Vec<u8>, CodecError> {
        if self.payload.len() > max_payload {
            return Err(CodecError::Length(self.payload.len()));
        }
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&MAGIC);
        out.push(self.version.major);
        out.push(self.version.minor);
        out.push(self.kind as u8);
        out.push(0);
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.request_id.to_le_bytes());
        out.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&checksum(&self.payload).to_le_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }
    pub fn decode(input: &[u8], max_payload: usize) -> Result<(Self, usize), CodecError> {
        if input.len() < HEADER_LEN {
            return Err(CodecError::Truncated);
        }
        if input[..2] != MAGIC {
            return Err(CodecError::InvalidMagic);
        }
        let version = Version {
            major: input[2],
            minor: input[3],
        };
        if version.major != VERSION.major {
            return Err(CodecError::Major(version.major));
        }
        // Reject before looking at the declared payload length so a huge length is not allocated.
        if input[5] != 0 {
            return Err(CodecError::Reserved);
        }
        let flags = u16::from_le_bytes(input[6..8].try_into().unwrap());
        if flags != 0 {
            return Err(CodecError::Flags);
        }
        let kind = MessageType::try_from(input[4])?;
        let len = u32::from_le_bytes(input[12..16].try_into().unwrap()) as usize;
        if len > max_payload {
            return Err(CodecError::Length(len));
        }
        let total = HEADER_LEN.checked_add(len).ok_or(CodecError::Length(len))?;
        if input.len() < total {
            return Err(CodecError::Truncated);
        }
        let payload = &input[HEADER_LEN..total];
        if checksum(payload) != u32::from_le_bytes(input[16..20].try_into().unwrap()) {
            return Err(CodecError::Checksum);
        }
        Ok((
            Self {
                version,
                kind,
                flags,
                request_id: u32::from_le_bytes(input[8..12].try_into().unwrap()),
                payload: payload.to_vec(),
            },
            total,
        ))
    }
}

/// One step of a byte stream that may contain serial-log noise before `MD`.
#[derive(Debug, PartialEq, Eq)]
pub enum StreamStep {
    /// A full packet. The `usize` includes any skipped noise in front of the header.
    Packet(Packet, usize),
    /// Wait for more bytes. `discard` is a noise prefix that is not part of a header.
    NeedMore { discard: usize },
    /// The candidate packet is rejected. `discard` covers that packet, not a reinterpretation of its payload.
    Rejected { error: CodecError, discard: usize },
}

/// Skip log noise until the next `MD` header.
/// A checksum or type failure rejects that candidate and does not treat its payload as a new packet.
pub fn decode_stream(input: &[u8], max_payload: usize) -> StreamStep {
    if input.is_empty() {
        return StreamStep::NeedMore { discard: 0 };
    }
    let Some(start) = input.windows(2).position(|pair| pair == MAGIC) else {
        if input.len() < HEADER_LEN {
            return StreamStep::NeedMore { discard: 0 };
        }
        let keep = usize::from(input.last() == Some(&b'M'));
        return StreamStep::NeedMore {
            discard: input.len() - keep,
        };
    };
    match Packet::decode(&input[start..], max_payload) {
        Ok((packet, len)) => StreamStep::Packet(packet, start + len),
        Err(CodecError::Truncated) => StreamStep::NeedMore { discard: start },
        Err(error) => {
            let span = framed_len(&input[start..], max_payload).unwrap_or(1);
            StreamStep::Rejected {
                error,
                discard: start + span,
            }
        }
    }
}

/// Length of a candidate frame when the header is present.
/// An over-long length returns 1 so the caller drops only the false `M`, not the following bytes as payload.
fn framed_len(input: &[u8], max_payload: usize) -> Option<usize> {
    if input.len() < HEADER_LEN || input[..2] != MAGIC {
        return None;
    }
    let len = u32::from_le_bytes(input[12..16].try_into().ok()?) as usize;
    if len > max_payload {
        return Some(1);
    }
    let total = HEADER_LEN.checked_add(len)?;
    if input.len() < total {
        return None;
    }
    Some(total)
}

/// Serde helpers for `skip_serializing_if` on additive fields (N304/N308).
fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}
fn is_zero_u16(value: &u16) -> bool {
    *value == 0
}
fn is_false(value: &bool) -> bool {
    !*value
}

/// Form B mirror capability. `mode` is empty when the device cannot mirror.
/// All fields default so older devices that never advertised mirror decode as
/// disabled (`supports()` is false) instead of failing the control payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MirrorCapability {
    /// Subset of ["full-frame", "dirty-region"]. Empty means unsupported.
    pub mode: Vec<String>,
    pub max_fps: u16,
    pub max_width: u16,
    pub max_height: u16,
    pub pixel_formats: Vec<String>,
    #[serde(default)]
    pub recommended_transport: Option<String>,
}
impl MirrorCapability {
    pub fn disabled() -> Self {
        Self::default()
    }
    /// A single-mode full-frame capability: the safe default for 3D/Live2D.
    pub fn full_frame(max_width: u16, max_height: u16, max_fps: u16) -> Self {
        Self {
            mode: vec!["full-frame".into()],
            max_fps,
            max_width,
            max_height,
            pixel_formats: vec!["RGB565".into()],
            recommended_transport: None,
        }
    }
    pub fn supports(&self) -> bool {
        !self.mode.is_empty() && self.max_fps > 0 && self.max_width > 0 && self.max_height > 0
    }
    pub fn supports_mode(&self, mode: &str) -> bool {
        self.supports() && self.mode.iter().any(|m| m == mode)
    }
    pub fn has_format(&self, format: &str) -> bool {
        self.pixel_formats.iter().any(|f| f == format)
    }
}

/// Host -> device mirror control (CBOR for MessageType::MirrorCtrl).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MirrorCommand {
    /// start | stop
    pub action: String,
    pub surface_id: String,
    /// full-frame | dirty-region
    pub mode: String,
    pub max_fps: u16,
    pub format: String,
}

/// Resolve a device-advertised mirror capability against a host request.
/// Returns `None` when the device cannot mirror at all. Downgrades
/// `dirty-region` to `full-frame` when only full-frame is advertised, and
/// clamps fps/format to the intersection of both sides.
pub fn negotiate_mirror(
    caps: &Capabilities,
    surface_id: &str,
    requested_mode: &str,
    requested_fps: u16,
    requested_format: &str,
) -> Option<MirrorCommand> {
    let mirror = &caps.mirror;
    if !mirror.supports() {
        return None;
    }
    let mode = if mirror.supports_mode(requested_mode) {
        requested_mode
    } else if mirror.supports_mode("full-frame") {
        "full-frame"
    } else {
        return None;
    };
    let format = if mirror.has_format(requested_format) {
        requested_format
    } else if mirror.has_format("RGB565") {
        "RGB565"
    } else {
        return None;
    };
    let fps = requested_fps
        .min(mirror.max_fps)
        .min(caps.mirror_fps.max(1));
    Some(MirrorCommand {
        action: "start".into(),
        surface_id: surface_id.to_string(),
        mode: mode.to_string(),
        max_fps: fps.max(1),
        format: format.to_string(),
    })
}

/// Form B renderer capability (N17): what a device can draw locally, used to
/// drive the model compiler's target-backend choice. Absent on pre-renderer
/// devices; `None` on `Capabilities` means "unknown / Form A only".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RendererCapability {
    pub sprite_2d: bool,
    pub mesh2d: bool,
    pub mesh3d: bool,
    pub skeleton: bool,
    /// N726: the device can skin with dual quaternions (host may select DQS).
    #[serde(default, skip_serializing_if = "is_false")]
    pub dqs: bool,
    pub max_texture: u32,
    pub max_vertices: u32,
}
impl RendererCapability {
    /// A weak MCU: sprites only.
    pub fn sprite_only(max_texture: u32) -> Self {
        Self {
            sprite_2d: true,
            mesh2d: false,
            mesh3d: false,
            skeleton: false,
            dqs: false,
            max_texture,
            max_vertices: 0,
        }
    }
    /// A GPU board: meshes and skeleton.
    pub fn full_3d(max_texture: u32, max_vertices: u32) -> Self {
        Self {
            sprite_2d: true,
            mesh2d: true,
            mesh3d: true,
            skeleton: true,
            dqs: false,
            max_texture,
            max_vertices,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Capabilities {
    pub device_id: String,
    pub firmware: String,
    pub surfaces: Vec<Surface>,
    pub frame: bool,
    pub tile: bool,
    pub touch: bool,
    pub ota: bool,
    pub max_message: u32,
    pub max_chunk: u32,
    pub max_in_flight: u8,
    pub max_fps: u16,
    /// Form B (N03). Defaults keep pre-mirror devices decodable.
    #[serde(default)]
    pub mirror: MirrorCapability,
    #[serde(default)]
    pub local_display_fps: u16,
    #[serde(default)]
    pub mirror_fps: u16,
    #[serde(default)]
    pub mirror_bandwidth: u64,
    /// N17: omitted entirely when absent, so older peers see an unchanged map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<RendererCapability>,
    /// N39: supported configuration keys. Omitted when empty so pre-config peers
    /// see an unchanged map.
    #[serde(default, skip_serializing_if = "ConfigCapability::is_empty")]
    pub config: ConfigCapability,
    /// N53: vector scene limits. Omitted when disabled.
    #[serde(default, skip_serializing_if = "SceneCapability::is_disabled")]
    pub scene: SceneCapability,
    /// N304: device boot identity. A change between two Capabilities replies means
    /// the device rebooted, so the host discards the previous run's pending ACKs.
    /// Defaults to 0 (omitted) so pre-N304 devices stay byte-identical.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub boot_id: u64,
    /// N304: monotonic run counter for the current boot. Ack replies carry the
    /// same value so a late ACK from a previous run can be recognised.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub run_id: u64,
}

/// Vector scene limits a device advertises (N53). `max_nodes == 0` means the
/// device does not implement the Scene path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SceneCapability {
    pub max_nodes: u32,
    pub max_depth: u32,
    /// N477: animation keyframes accepted across the whole scene.
    #[serde(default)]
    pub max_anim_keys: u32,
}
impl SceneCapability {
    pub fn enabled(max_nodes: u32, max_depth: u32) -> Self {
        Self {
            max_nodes,
            max_depth,
            max_anim_keys: SCENE_MAX_ANIM_KEYS,
        }
    }
    /// N477: enable scene animation with an explicit keyframe budget.
    pub fn enabled_with_animation(max_nodes: u32, max_depth: u32, max_anim_keys: u32) -> Self {
        Self {
            max_nodes,
            max_depth,
            max_anim_keys,
        }
    }
    pub fn is_disabled(&self) -> bool {
        self.max_nodes == 0
    }
}

/// N743: one command of a scene `path` stream. Coordinates are absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathCommand {
    Move([i32; 2]),
    Line([i32; 2]),
    Quad([i32; 2], [i32; 2]),
    Cubic([i32; 2], [i32; 2], [i32; 2]),
    /// N753: an SVG-style elliptical arc. `rx`/`ry` are the radii, `rotation`
    /// the x-axis rotation in degrees, and `end` the absolute end point.
    Arc {
        rx: i32,
        ry: i32,
        rotation: i32,
        large_arc: bool,
        sweep: bool,
        end: [i32; 2],
    },
    /// N753: a smooth quadratic segment; the control point is reflected from the
    /// previous quadratic control (or the current point after another command).
    SmoothQuad([i32; 2]),
    /// N763: a smooth cubic segment; the first control point is reflected from
    /// the previous cubic's second control. `(control2, end)`.
    SmoothCubic([i32; 2], [i32; 2]),
    /// N773: a horizontal line to an absolute x (absolute `H` or relative `h`).
    Horizontal(i32),
    /// N773: a vertical line to an absolute y (absolute `V` or relative `v`).
    Vertical(i32),
    Close,
}

/// A node in the host vector scene (CBOR, N53). `kind` selects the primitive
/// (rect/text/sprite/image); `value` is kind-specific. N79 adds optional geometry
/// (`rect: [x, y, w, h]`) and treats `value` on a `rect` as a `#RRGGBB` colour.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SceneNode {
    pub name: String,
    pub kind: String,
    /// N79: `[x, y, width, height]` for kinds that rasterize (rect).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<[u16; 4]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SceneNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// N348: the end colour of a `gradient` node; falls back to `value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value2: Option<String>,
    /// N338: source opacity for rasterised kinds (0..=255; `None` = opaque).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<u8>,
    /// N450: `Some(false)` hides the node and its children from drawing and
    /// hit-testing (`None` = visible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// N463: keyframed opacity/rect animation, sampled on the device clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anim: Option<SceneAnim>,
    /// N475: a quarter-turn rotation (0/90/180/270) for axis-aligned fills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<u16>,
    /// N480: `source-over` (default) or `additive` blending for rect/gradient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<String>,
    /// N484: `left` (default) | `center` | `right` alignment for `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    /// N485: explicit draw order among siblings (higher draws on top; ties keep
    /// document order).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<i32>,
    /// N495: `word` (default) | `char` | `none` wrapping for `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<String>,
    /// N515: `linear` (default) | `radial` fill for `gradient`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// N535: the radial centre as `"cx%,cy%"` of the rect (default `50,50`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
}

/// N724: segment interpolation curve for a scene animation. `linear` is the
/// default; the eased variants keep both endpoints unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Easing {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Easing {
    /// Parse a wire name; unknown/absent values fall back to linear.
    pub fn parse(name: Option<&str>) -> Self {
        match name.map(str::trim) {
            Some("ease-in") => Self::EaseIn,
            Some("ease-out") => Self::EaseOut,
            Some("ease-in-out") => Self::EaseInOut,
            _ => Self::Linear,
        }
    }

    /// Shape a normalized segment fraction in `0..=1`.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            // Slow start, fast end.
            Self::EaseIn => t * t,
            // Fast start, slow end.
            Self::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Self::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - 2.0 * (1.0 - t) * (1.0 - t)
                }
            }
        }
    }
}

/// N825: one colour keyframe. Colours use the same syntax as scene `value`
/// (`#RRGGBB`, `#RGB`, bare hex, named colours).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneColorKey {
    /// Keyframe time in milliseconds.
    pub time_ms: u16,
    /// Colour value at that time.
    pub value: String,
}

/// N463: a keyframed scene animation. Keyframes are `[time_ms, value]` and are
/// interpolated; `duration_ms` bounds the clip.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneAnim {
    /// Clip length in milliseconds (0 is treated as 1).
    #[serde(default)]
    pub duration_ms: u32,
    /// Whether the clip repeats (otherwise it holds the last keyframe).
    #[serde(default)]
    pub looping: bool,
    /// N823: delay before the clip starts, in milliseconds. The first keyframe
    /// is held until then (`None`/0 starts immediately).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u32>,
    /// N823: playback speed in permille (1000 = authored speed). `None` and `0`
    /// both mean 1000, so an old payload keeps its timing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_permille: Option<u16>,
    /// Opacity keyframes: `[time_ms, opacity 0..=255]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opacity: Vec<[u16; 2]>,
    /// Rect keyframes: `[time_ms, x, y, w, h]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rect: Vec<[u16; 5]>,
    /// N546: radial `focus` keyframes: `[time_ms, cx%, cy%]` (percent of the
    /// rect, matching the `focus` field's `"cx%,cy%"` form).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus: Vec<[u16; 3]>,
    /// N825: colour keyframes for kinds whose `value` is a colour. `text`,
    /// `image` and `sprite` keep their payload and are not recoloured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub color: Vec<SceneColorKey>,
    /// N827: quarter-turn keyframes `[time_ms, degrees]`. The sampled angle snaps
    /// to the nearest multiple of 90 (0/90/180/270).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rotate: Vec<[u16; 2]>,
    /// N724: segment easing (`linear` default, `ease-in`, `ease-out`,
    /// `ease-in-out`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub easing: Option<String>,
}

impl SceneAnim {
    /// N724: the parsed segment easing.
    pub fn easing(&self) -> Easing {
        Easing::parse(self.easing.as_deref())
    }

    /// The clip-local time for `time_ms`. N823: `delay_ms` holds the first
    /// keyframe until the delay elapses, and `speed_permille` scales the elapsed
    /// time (0 is treated as 1000 so old payloads keep their timing).
    pub fn local_time(&self, time_ms: u64) -> u32 {
        let delay = u64::from(self.delay_ms.unwrap_or(0));
        if time_ms < delay {
            return 0;
        }
        let speed = u64::from(
            self.speed_permille
                .filter(|speed| *speed > 0)
                .unwrap_or(1000),
        );
        let elapsed = time_ms.saturating_sub(delay).saturating_mul(speed) / 1000;
        let duration = u64::from(self.duration_ms.max(1));
        let local = if self.looping {
            elapsed % duration
        } else {
            elapsed.min(duration)
        };
        local as u32
    }

    /// The sampled opacity at `time_ms`, or `None` when there are no keyframes.
    pub fn opacity_at(&self, time_ms: u64) -> Option<u8> {
        let t = self.local_time(time_ms);
        sample_keys(
            &self.opacity,
            t,
            |k, value| (u32::from(k), u32::from(value)),
            self.easing(),
        )
        .map(|value| value.min(255) as u8)
    }

    /// The sampled rect at `time_ms`, or `None` when there are no keyframes.
    pub fn rect_at(&self, time_ms: u64) -> Option<[u16; 4]> {
        if self.rect.is_empty() {
            return None;
        }
        let t = self.local_time(time_ms);
        let easing = self.easing();
        let channel = |k: usize| -> Option<u16> {
            let keys: Vec<[u16; 2]> = self.rect.iter().map(|frame| [frame[0], frame[k]]).collect();
            sample_keys(
                &keys,
                t,
                |k, value| (u32::from(k), u32::from(value)),
                easing,
            )
            .map(|value| value.min(u16::MAX as u32) as u16)
        };
        Some([channel(1)?, channel(2)?, channel(3)?, channel(4)?])
    }

    /// N546: the sampled radial focus `[cx%, cy%]` at `time_ms`, or `None` when
    /// there are no focus keyframes.
    pub fn focus_at(&self, time_ms: u64) -> Option<[u16; 2]> {
        if self.focus.is_empty() {
            return None;
        }
        let t = self.local_time(time_ms);
        let easing = self.easing();
        let channel = |k: usize| -> Option<u16> {
            let keys: Vec<[u16; 2]> = self
                .focus
                .iter()
                .map(|frame| [frame[0], frame[k]])
                .collect();
            sample_keys(
                &keys,
                t,
                |k, value| (u32::from(k), u32::from(value)),
                easing,
            )
            .map(|value| value.min(100) as u16)
        };
        Some([channel(1)?, channel(2)?])
    }

    /// N825: the sampled colour at `time_ms`, or `None` when there are no valid
    /// colour keyframes. Invalid colour values are skipped rather than treated
    /// as white.
    pub fn color_at(&self, time_ms: u64) -> Option<[u8; 3]> {
        let mut frames: Vec<(u32, [u8; 3])> = self
            .color
            .iter()
            .filter_map(|key| parse_color_rgb(&key.value).map(|rgb| (u32::from(key.time_ms), rgb)))
            .collect();
        frames.sort_by_key(|(time, _)| *time);
        if frames.is_empty() {
            return None;
        }
        let t = self.local_time(time_ms);
        if t <= frames[0].0 {
            return Some(frames[0].1);
        }
        for pair in frames.windows(2) {
            let ((t0, c0), (t1, c1)) = (pair[0], pair[1]);
            if t <= t1 && t1 > t0 {
                let span = (t1 - t0) as f32;
                let f = self.easing().apply((t - t0) as f32 / span);
                let channel = |a: u8, b: u8| -> u8 {
                    (f32::from(a) + (f32::from(b) - f32::from(a)) * f).round() as u8
                };
                return Some([
                    channel(c0[0], c1[0]),
                    channel(c0[1], c1[1]),
                    channel(c0[2], c1[2]),
                ]);
            }
        }
        frames.last().map(|(_, rgb)| *rgb)
    }

    /// N827: the sampled quarter-turn at `time_ms`, snapped to the nearest
    /// multiple of 90 degrees. Returns `None` when there are no rotate keys.
    pub fn rotate_at(&self, time_ms: u64) -> Option<u16> {
        let t = self.local_time(time_ms);
        sample_keys(
            &self.rotate,
            t,
            |k, value| (u32::from(k), u32::from(value)),
            self.easing(),
        )
        .map(|value| ((value.saturating_add(45) / 90) % 4 * 90) as u16)
    }
}

/// N723: parse `"x,y;x,y;..."` into bounded integer points.
fn parse_point_list(spec: &str) -> Option<Vec<[i32; 2]>> {
    let mut points = Vec::new();
    for token in spec.split([';', ' ', '\n', '\t']).filter(|t| !t.is_empty()) {
        let (x, y) = token.split_once(',')?;
        points.push([x.trim().parse().ok()?, y.trim().parse().ok()?]);
        if points.len() > SceneNode::MAX_POINTS {
            return None;
        }
    }
    (points.len() >= 2).then_some(points)
}

/// Interpolate `[time, value]` keyframes at `t`; returns the last value past the end.
fn sample_keys<F>(keys: &[[u16; 2]], t: u32, map: F, easing: Easing) -> Option<u32>
where
    F: Fn(u16, u16) -> (u32, u32),
{
    if keys.is_empty() {
        return None;
    }
    let mut frames: Vec<(u32, u32)> = keys.iter().map(|k| map(k[0], k[1])).collect();
    frames.sort_by_key(|(time, _)| *time);
    if t <= frames[0].0 {
        return Some(frames[0].1);
    }
    for pair in frames.windows(2) {
        let (t0, v0) = pair[0];
        let (t1, v1) = pair[1];
        if t <= t1 && t1 > t0 {
            let span = f32::from((t1 - t0) as u16);
            let f = easing.apply(f32::from((t - t0) as u16) / span);
            return Some(
                (f32::from(v0 as u16) + (f32::from(v1 as u16) - f32::from(v0 as u16)) * f).round()
                    as u32,
            );
        }
    }
    frames.last().map(|(_, value)| *value)
}

/// N825: kinds whose `value` is a colour (the ones a colour track may drive).
fn kind_uses_colour_value(kind: &str) -> bool {
    matches!(
        kind,
        "rect"
            | "circle"
            | "line"
            | "gradient"
            | "rounded-rect"
            | "polygon"
            | "polyline"
            | "arc"
            | "pie"
            | "path"
    )
}

/// N463: apply a node's animation (in place) at `time_ms`, recursively.
pub fn apply_scene_animation(node: &mut SceneNode, time_ms: u64) {
    if let Some(anim) = node.anim.clone() {
        if let Some(opacity) = anim.opacity_at(time_ms) {
            node.opacity = Some(opacity);
        }
        if let Some(rect) = anim.rect_at(time_ms) {
            node.rect = Some(rect);
        }
        // N546: a focus track drives the radial gradient centre.
        if let Some([cx, cy]) = anim.focus_at(time_ms) {
            node.focus = Some(format!("{cx},{cy}"));
        }
        // N825: colour tracks only rewrite `value` for colour-bearing kinds, so
        // text glyphs, image hex and sprite indices are never overwritten.
        if let Some([r, g, b]) = anim.color_at(time_ms) {
            if kind_uses_colour_value(&node.kind) {
                node.value = Some(format!("#{r:02X}{g:02X}{b:02X}"));
            }
        }
        // N827: rotate tracks feed the existing quarter-turn handling.
        if let Some(rotate) = anim.rotate_at(time_ms) {
            node.rotate = Some(rotate);
        }
    }
    for child in &mut node.children {
        apply_scene_animation(child, time_ms);
    }
}
/// N417: the default scene limits used by [`SceneNode::lint`] (matching a typical
/// `SceneCapability`).
pub const SCENE_MAX_NODES: u32 = 512;
pub const SCENE_MAX_DEPTH: u32 = 16;
/// N477: the default animation-keyframe budget for a scene.
pub const SCENE_MAX_ANIM_KEYS: u32 = 256;

/// N417: the result of linting a scene tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SceneLint {
    pub valid: bool,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    pub nodes: u32,
    pub depth: u32,
    /// N829: animation keyframes across the tree (sampled tracks only).
    pub anim_keys: u32,
}

impl SceneNode {
    /// N485: this node's draw order among its siblings (default 0).
    pub fn z(&self) -> i32 {
        self.z.unwrap_or(0)
    }

    /// N480: whether the node adds to the framebuffer instead of covering it.
    pub fn is_additive(&self) -> bool {
        self.blend.as_deref() == Some("additive")
    }

    /// N793: path stroke width from a `mode` token `width=N` (1..=8). A missing
    /// or invalid token stays at 1, so existing 1-pixel paths keep their pixels.
    pub fn stroke_width(&self) -> u8 {
        let mut width = 1u8;
        for token in self.mode.as_deref().unwrap_or("").split(',') {
            let Some(value) = token.trim().strip_prefix("width=") else {
                continue;
            };
            if let Ok(parsed) = value.trim().parse::<u8>() {
                if (1..=8).contains(&parsed) {
                    width = parsed;
                }
            }
        }
        width
    }

    /// N803: path dash length from a `mode` token `dash=N` (1..=64). A missing
    /// or invalid token stays at 0, which paints a solid stroke. A later invalid
    /// token does not clear an earlier dash.
    pub fn stroke_dash(&self) -> u16 {
        let mut dash = 0u16;
        for token in self.mode.as_deref().unwrap_or("").split(',') {
            let Some(value) = token.trim().strip_prefix("dash=") else {
                continue;
            };
            if let Ok(parsed) = value.trim().parse::<u16>() {
                if (1..=64).contains(&parsed) {
                    dash = parsed;
                }
            }
        }
        dash
    }

    /// N813: dash phase from a `mode` token `phase=N` (0..=64). The phase is a
    /// pixel offset into the dash period; it shifts where the first on-segment
    /// starts without changing the dash length. A missing or invalid token stays
    /// at 0, and a later invalid token does not clear an earlier phase.
    pub fn stroke_phase(&self) -> u16 {
        let mut phase = 0u16;
        for token in self.mode.as_deref().unwrap_or("").split(',') {
            let Some(value) = token.trim().strip_prefix("phase=") else {
                continue;
            };
            if let Ok(parsed) = value.trim().parse::<u16>() {
                if parsed <= 64 {
                    phase = parsed;
                }
            }
        }
        phase
    }

    /// N450: whether the node is visible (default true).
    pub fn is_visible(&self) -> bool {
        self.visible.unwrap_or(true)
    }

    /// N417: lint the scene tree (unknown kinds, missing geometry/values, zero
    /// extents, duplicate names, and capability limits).
    pub fn lint(&self) -> SceneLint {
        let mut report = SceneLint {
            valid: true,
            problems: Vec::new(),
            warnings: Vec::new(),
            nodes: self.count(),
            depth: self.depth(),
            anim_keys: self.anim_keys(),
        };
        if report.nodes > SCENE_MAX_NODES {
            report.problems.push(format!(
                "{} nodes over the {SCENE_MAX_NODES} limit",
                report.nodes
            ));
        }
        if report.depth > SCENE_MAX_DEPTH {
            report.problems.push(format!(
                "depth {} over the {SCENE_MAX_DEPTH} limit",
                report.depth
            ));
        }
        if report.anim_keys > SCENE_MAX_ANIM_KEYS {
            report.problems.push(format!(
                "{} animation keys over the {SCENE_MAX_ANIM_KEYS} limit",
                report.anim_keys
            ));
        }
        let mut names = std::collections::BTreeSet::new();
        self.lint_into(&mut report, &mut names);
        report.valid = report.problems.is_empty();
        report
    }

    fn lint_into(&self, report: &mut SceneLint, names: &mut std::collections::BTreeSet<String>) {
        const KINDS: [&str; 16] = [
            "group",
            "rect",
            "text",
            "sprite",
            "image",
            "circle",
            "line",
            "gradient",
            "rounded-rect",
            "clip",
            "polygon",
            "polyline",
            "arc",
            "pie",
            "path",
            "mask",
        ];
        let drawable = !matches!(self.kind.as_str(), "group" | "clip");
        if !KINDS.contains(&self.kind.as_str()) {
            report.warnings.push(format!(
                "node '{}' has unknown kind '{}'",
                self.name, self.kind
            ));
        }
        if drawable && self.rect.is_none() {
            report
                .problems
                .push(format!("node '{}' ({}) has no rect", self.name, self.kind));
        }
        if let Some(rect) = self.rect {
            if drawable && (rect[2] == 0 || rect[3] == 0) {
                report
                    .warnings
                    .push(format!("node '{}' has a zero width or height", self.name));
            }
        }
        match self.kind.as_str() {
            "text" | "sprite" | "image" if self.value.is_none() => report
                .problems
                .push(format!("node '{}' ({}) has no value", self.name, self.kind)),
            "polygon" | "polyline" if self.points().is_none() => report.problems.push(format!(
                "node '{}' ({}) has no valid points",
                self.name, self.kind
            )),
            "path" if self.bezier().is_none() && self.path_commands().is_none() => report
                .problems
                .push(format!("node '{}' (path) has no valid points", self.name)),
            "arc" | "pie" if self.arc_angles().is_none() => report.problems.push(format!(
                "node '{}' ({}) has no valid angles",
                self.name, self.kind
            )),
            "mask" if self.points().is_none() => report
                .problems
                .push(format!("node '{}' (mask) has no valid points", self.name)),
            _ => {}
        }
        if !self.name.is_empty() && !names.insert(self.name.clone()) {
            report
                .warnings
                .push(format!("duplicate node name '{}'", self.name));
        }
        // N829: surface animation tracks that cannot be applied as authored.
        if let Some(anim) = &self.anim {
            if anim.duration_ms == 0 {
                report.warnings.push(format!(
                    "node '{}' has a zero animation duration (treated as 1 ms)",
                    self.name
                ));
            }
            if !anim.looping {
                let late = |time: u16| u32::from(time) > anim.duration_ms;
                if anim.opacity.iter().any(|key| late(key[0]))
                    || anim.rect.iter().any(|key| late(key[0]))
                    || anim.focus.iter().any(|key| late(key[0]))
                    || anim.color.iter().any(|key| late(key.time_ms))
                    || anim.rotate.iter().any(|key| late(key[0]))
                {
                    report.warnings.push(format!(
                        "node '{}' has keyframes after its {} ms duration",
                        self.name, anim.duration_ms
                    ));
                }
            }
            for key in &anim.color {
                if parse_color_rgb(&key.value).is_none() {
                    report.warnings.push(format!(
                        "node '{}' has an invalid colour key '{}'",
                        self.name, key.value
                    ));
                }
            }
            for key in &anim.rotate {
                if key[1] % 90 != 0 {
                    report.warnings.push(format!(
                        "node '{}' rotate key {} snaps to the nearest quarter turn",
                        self.name, key[1]
                    ));
                }
            }
        }
        for child in &self.children {
            child.lint_into(report, names);
        }
    }

    /// N477/N546/N829: animation keyframes across the tree (opacity + rect +
    /// focus + colour + rotate).
    pub fn anim_keys(&self) -> u32 {
        let own = self
            .anim
            .as_ref()
            .map(|anim| {
                (anim.opacity.len()
                    + anim.rect.len()
                    + anim.focus.len()
                    + anim.color.len()
                    + anim.rotate.len()) as u32
            })
            .unwrap_or(0);
        self.children
            .iter()
            .fold(own, |total, child| total.saturating_add(child.anim_keys()))
    }

    /// Total node count including this one.
    pub fn count(&self) -> u32 {
        self.children
            .iter()
            .fold(1u32, |total, child| total.saturating_add(child.count()))
    }
    /// Maximum nesting depth (a leaf is 1).
    pub fn depth(&self) -> u32 {
        self.children
            .iter()
            .map(SceneNode::depth)
            .max()
            .unwrap_or(0)
            + 1
    }
    /// N723: the most vertices a `polygon`/`polyline` node may carry.
    pub const MAX_POINTS: usize = 64;

    /// N723: parse a `polygon`/`polyline` vertex list from `value2`
    /// (`"x,y;x,y;..."`; spaces may separate points). Returns `None` for a
    /// malformed list, fewer than two points, or more than [`Self::MAX_POINTS`].
    pub fn points(&self) -> Option<Vec<[i32; 2]>> {
        parse_point_list(self.value2.as_deref()?)
    }

    /// N734: the start/control/end points of a `path` curve from `value2`.
    /// Three points mean quadratic; four mean cubic. More or fewer are rejected.
    pub fn bezier(&self) -> Option<Vec<[i32; 2]>> {
        let points = parse_point_list(self.value2.as_deref()?)?;
        matches!(points.len(), 3 | 4).then_some(points)
    }

    /// N743: a `path` command stream from `value2`, e.g.
    /// `"M1,1;L5,1;Q8,4 5,8;C1,1 2,2 3,3;Z"`. Commands are bounded to 64 and
    /// must start with `M`; malformed input is rejected.
    ///
    /// N763: lowercase commands are relative to the current point (`m/l/q/c/s/
    /// t/a`) and may be mixed with absolute ones. `S` is a smooth cubic whose
    /// first control point reflects the previous cubic.
    ///
    /// N783: extra coordinate groups after `M/L/Q/C/H/V/S/T/A` repeat that
    /// command (SVG implicit repetition). A repeated `M`/`m` becomes a line.
    pub fn path_commands(&self) -> Option<Vec<PathCommand>> {
        let mut out = Vec::new();
        let mut current = [0i32; 2];
        let mut start = [0i32; 2];
        for part in self
            .value2
            .as_deref()?
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let mut chars = part.chars();
            let raw_op = chars.next()?;
            let op = raw_op.to_ascii_uppercase();
            let relative = raw_op.is_ascii_lowercase();
            let rest = chars.as_str();
            let numbers: Option<Vec<i32>> = rest
                .split([' ', '\t', '\n', ','])
                .filter(|token| !token.is_empty())
                .map(|token| token.parse().ok())
                .collect();
            let numbers = numbers?;
            let stride = match op {
                'M' | 'L' | 'T' => 2,
                'Q' | 'S' => 4,
                'C' => 6,
                'A' => 7,
                'H' | 'V' => 1,
                'Z' => 0,
                _ => return None,
            };
            if stride == 0 {
                if !numbers.is_empty() {
                    return None;
                }
            } else if numbers.is_empty() || numbers.len() % stride != 0 {
                return None;
            }
            let mut offset = 0usize;
            let mut first = true;
            loop {
                if op == 'Z' {
                    out.push(PathCommand::Close);
                    current = start;
                    break;
                }
                if offset >= numbers.len() {
                    break;
                }
                let point_at = |index: usize, current: [i32; 2]| -> [i32; 2] {
                    let x = numbers[index];
                    let y = numbers[index + 1];
                    if relative {
                        [current[0].saturating_add(x), current[1].saturating_add(y)]
                    } else {
                        [x, y]
                    }
                };
                match op {
                    'M' | 'L' => {
                        let at = point_at(offset, current);
                        // SVG: pairs after the first moveto are implicit lineto.
                        let draw_line = op == 'L' || !first;
                        if draw_line {
                            out.push(PathCommand::Line(at));
                        } else {
                            out.push(PathCommand::Move(at));
                            start = at;
                        }
                        current = at;
                    }
                    'Q' => {
                        let control = point_at(offset, current);
                        let at = point_at(offset + 2, current);
                        out.push(PathCommand::Quad(control, at));
                        current = at;
                    }
                    'C' => {
                        let control1 = point_at(offset, current);
                        let control2 = point_at(offset + 2, current);
                        let at = point_at(offset + 4, current);
                        out.push(PathCommand::Cubic(control1, control2, at));
                        current = at;
                    }
                    'A' => {
                        if !matches!(numbers[offset + 3], 0 | 1)
                            || !matches!(numbers[offset + 4], 0 | 1)
                        {
                            return None;
                        }
                        let at = point_at(offset + 5, current);
                        out.push(PathCommand::Arc {
                            rx: numbers[offset].saturating_abs(),
                            ry: numbers[offset + 1].saturating_abs(),
                            rotation: numbers[offset + 2],
                            large_arc: numbers[offset + 3] == 1,
                            sweep: numbers[offset + 4] == 1,
                            end: at,
                        });
                        current = at;
                    }
                    'T' => {
                        let at = point_at(offset, current);
                        out.push(PathCommand::SmoothQuad(at));
                        current = at;
                    }
                    'S' => {
                        let control2 = point_at(offset, current);
                        let at = point_at(offset + 2, current);
                        out.push(PathCommand::SmoothCubic(control2, at));
                        current = at;
                    }
                    'H' => {
                        let x = if relative {
                            current[0].saturating_add(numbers[offset])
                        } else {
                            numbers[offset]
                        };
                        out.push(PathCommand::Horizontal(x));
                        current[0] = x;
                    }
                    'V' => {
                        let y = if relative {
                            current[1].saturating_add(numbers[offset])
                        } else {
                            numbers[offset]
                        };
                        out.push(PathCommand::Vertical(y));
                        current[1] = y;
                    }
                    _ => return None,
                }
                first = false;
                offset += stride;
                if out.len() > 64 {
                    return None;
                }
            }
        }
        match out.first() {
            Some(PathCommand::Move(_)) => Some(out),
            _ => None,
        }
    }

    /// N733: the sector start/end angles (degrees) for `arc`/`pie` from
    /// `value2` (`"start,end"`). A non-positive sweep means the full circle.
    pub fn arc_angles(&self) -> Option<(f32, f32)> {
        let (start, end) = self.value2.as_deref()?.split_once(',')?;
        let start: f32 = start.trim().parse().ok()?;
        let end: f32 = end.trim().parse().ok()?;
        if !start.is_finite() || !end.is_finite() {
            return None;
        }
        let start = start.rem_euclid(360.0);
        let mut sweep = end.rem_euclid(360.0) - start;
        if sweep <= 0.0 {
            sweep += 360.0;
        }
        Some((start, sweep.min(360.0)))
    }

    /// N759: a `group` node's shear from `value2` (`"kx,ky"` permille, where
    /// 1000 shifts a child by its other coordinate). Returns `None` when absent;
    /// malformed or extreme values are clamped to ±2000.
    pub fn skew(&self) -> Option<(i32, i32)> {
        let (kx, ky) = self.value2.as_deref()?.split_once(',')?;
        let kx: i32 = kx.trim().parse().ok()?;
        let ky: i32 = ky.trim().parse().ok()?;
        Some((kx.clamp(-2000, 2000), ky.clamp(-2000, 2000)))
    }

    /// N79/N136: the fill colour for a `rect`, parsed from `value` as
    /// `#RRGGBB`, `#RGB`, or a named colour. Missing or malformed values use
    /// opaque white.
    pub fn color_rgb565(&self) -> u16 {
        match self.value.as_deref() {
            Some(value) => parse_color(value),
            None => 0xFFFF,
        }
    }
    /// N348: the end colour of a `gradient` node (defaults to the start colour).
    pub fn color2_rgb565(&self) -> u16 {
        match self.value2.as_deref() {
            Some(value) => parse_color(value),
            None => self.color_rgb565(),
        }
    }
    /// N338: the node opacity as 8 bits (255 when unset).
    pub fn opacity_u8(&self) -> u8 {
        self.opacity.unwrap_or(255)
    }
}

/// N136/N348/N505: parse `#RRGGBB`, `#RGB`, bare hex, or a named colour
/// (default white). Public so the renderers can parse gradient stops.
pub fn parse_color(value: &str) -> u16 {
    match parse_color_rgb(value) {
        Some([r, g, b]) => rgb565_from(r, g, b),
        None => 0xFFFF,
    }
}

/// N825: parse a colour into 8-bit channels. Unlike [`parse_color`], malformed
/// values return `None` so an animation can skip them.
pub fn parse_color_rgb(value: &str) -> Option<[u8; 3]> {
    if let Some((r, g, b)) = parse_hex_color(value) {
        return Some([r, g, b]);
    }
    named_color(value).map(|(r, g, b)| [r, g, b])
}

/// Parse `#RRGGBB` or `#RGB` (the `#` optional) into 8-bit channels.
fn parse_hex_color(value: &str) -> Option<(u8, u8, u8)> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    match hex.len() {
        6 => {
            let rgb = u32::from_str_radix(hex, 16).ok()?;
            Some((
                ((rgb >> 16) & 0xff) as u8,
                ((rgb >> 8) & 0xff) as u8,
                (rgb & 0xff) as u8,
            ))
        }
        3 => {
            let mut channels = [0u8; 3];
            for (i, ch) in hex.bytes().enumerate() {
                let nibble = (ch as char).to_digit(16)? as u8;
                channels[i] = nibble * 17;
            }
            Some((channels[0], channels[1], channels[2]))
        }
        _ => None,
    }
}

/// A small set of CSS-style colour names, case-insensitive.
fn named_color(value: &str) -> Option<(u8, u8, u8)> {
    match value.to_ascii_lowercase().as_str() {
        "black" => Some((0, 0, 0)),
        "white" => Some((255, 255, 255)),
        "red" => Some((255, 0, 0)),
        "green" => Some((0, 128, 0)),
        "lime" => Some((0, 255, 0)),
        "blue" => Some((0, 0, 255)),
        "yellow" => Some((255, 255, 0)),
        "cyan" | "aqua" => Some((0, 255, 255)),
        "magenta" | "fuchsia" => Some((255, 0, 255)),
        "orange" => Some((255, 165, 0)),
        "gray" | "grey" => Some((128, 128, 128)),
        _ => None,
    }
}

fn rgb565_from(r: u8, g: u8, b: u8) -> u16 {
    ((u16::from(r) >> 3) << 11) | ((u16::from(g) >> 2) << 5) | (u16::from(b) >> 3)
}

/// Host -> device scene command (CBOR for MessageType::Scene).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SceneCommand {
    /// set | clear
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneNode>,
}

/// Configuration keys a device accepts via `CONFIG` (N39). Empty means the
/// device does not implement the configuration host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ConfigCapability {
    pub keys: Vec<String>,
    /// N616: per-key factory defaults the host may read without a `get_config`
    /// round trip. Additive; an older peer omits it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub defaults: std::collections::BTreeMap<String, String>,
}
impl ConfigCapability {
    pub fn new(keys: &[&str]) -> Self {
        Self {
            keys: keys.iter().map(|k| (*k).to_string()).collect(),
            defaults: std::collections::BTreeMap::new(),
        }
    }
    /// N616: the advertised factory default for `key`, if any.
    pub fn default_for(&self, key: &str) -> Option<&str> {
        self.defaults.get(key).map(String::as_str)
    }
    pub fn supports(&self, key: &str) -> bool {
        self.keys.iter().any(|k| k == key)
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Host -> device configuration command (CBOR for MessageType::Config).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigCommand {
    /// get | set
    pub action: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// Device -> host configuration reply (CBOR for MessageType::Config / Ack path).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigReply {
    pub key: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
/// N307: pixel formats a surface/frame path may carry. Only `Rgb565` is
/// implemented; the others decode (so unknown-but-named formats are not lost)
/// but are rejected with an explicit `Unsupported` error. Serialized as the
/// canonical wire string so the encoding is unchanged from the old `String`
/// field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb565,
    Rgb888,
    Rgba8888,
    Gray1,
    Gray4,
    /// Any other advertised format (forward compatible; rejected on send).
    Other(String),
}
impl PixelFormat {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Rgb565 => "RGB565",
            Self::Rgb888 => "RGB888",
            Self::Rgba8888 => "RGBA8888",
            Self::Gray1 => "GRAY1",
            Self::Gray4 => "GRAY4",
            Self::Other(name) => name,
        }
    }
    /// Only RGB565 has an implemented host/frame path in this batch.
    pub fn is_implemented(&self) -> bool {
        matches!(self, Self::Rgb565)
    }
}
impl std::fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl From<&str> for PixelFormat {
    fn from(value: &str) -> Self {
        match value.to_ascii_uppercase().as_str() {
            "RGB565" => Self::Rgb565,
            "RGB888" => Self::Rgb888,
            "RGBA8888" => Self::Rgba8888,
            "GRAY1" => Self::Gray1,
            "GRAY4" => Self::Gray4,
            _ => Self::Other(value.to_string()),
        }
    }
}
impl From<String> for PixelFormat {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}
impl Serialize for PixelFormat {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for PixelFormat {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from(value))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Surface {
    pub id: String,
    pub width: u16,
    pub height: u16,
    pub pixel_format: PixelFormat,
    pub stride: u32,
    pub rotation: u16,
    /// N308: device-advertised preferred tile edge in pixels (0 = no preference).
    /// The host adapts its tile cut to this rather than assuming 16/32.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub preferred_tile_size: u16,
    /// N308: the device can accept partial (tile/dirty) updates.
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_partial_update: bool,
    /// N308: the device can accept a full framebuffer push (Form A).
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_full_framebuffer: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hello {
    pub major: u8,
    pub minor: u8,
    pub max_message: u32,
    pub max_chunk: u32,
    /// N304: host boot identity (0 when unset / pre-N304 peer).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub boot_id: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ack {
    pub received: bool,
    pub displayed: bool,
    pub frame_id: u64,
    pub error: Option<String>,
    /// N304: the device run this ACK belongs to (0 when unset).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub run_id: u64,
    /// N304: monotonic event sequence within the run (0 when unset).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub event_seq: u64,
}

/// Touch / pointer input (CBOR control payload for MessageType::Input).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InputEvent {
    pub surface_id: String,
    pub pointer_id: u16,
    /// down | move | up
    pub phase: String,
    pub x: u16,
    pub y: u16,
    /// N418: the topmost scene node under the pointer, when hit-tested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

/// OTA control command (CBOR for MessageType::Ota).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OtaCommand {
    /// begin | abort | confirm | chunk | timeout
    pub action: String,
    pub version: Option<String>,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    /// N737: a chunk's byte offset (additive; older peers omit it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    /// N737: a chunk's byte length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub len: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
}

/// N367: a device -> host crash/fault report (CBOR for `MessageType::Crash`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrashReport {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pc: Option<u32>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub boot_id: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub run_id: u64,
}

/// N316: a device -> host metric sample (CBOR for `MessageType::Metric`). The
/// diagnostics channel is independent of the frame/ack path, so a saturated
/// image link cannot lose a test result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricReport {
    pub name: String,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub boot_id: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub run_id: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub event_seq: u64,
}

/// N316: a device -> host boot notification (CBOR for `MessageType::BootInfo`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootInfo {
    pub firmware: String,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub boot_id: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub run_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_reason: Option<String>,
}

/// N335: a structured diagnostic event (CBOR for `MessageType::Telemetry`). The
/// payload is an event name, a level, and free-form string fields, so a HIL test
/// can assert on named values without a bespoke opcode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelemetryEvent {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fields: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub boot_id: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub run_id: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub event_seq: u64,
}

/// Decoded FRAME payload variants (leading phase byte).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramePayload {
    Legacy {
        surface_id: String,
        frame_id: u64,
        width: u16,
        height: u16,
        bytes: Vec<u8>,
    },
    Begin {
        surface_id: String,
        frame_id: u64,
        width: u16,
        height: u16,
        total_bytes: u32,
    },
    Chunk {
        frame_id: u64,
        offset: u32,
        data: Vec<u8>,
    },
    Commit {
        frame_id: u64,
    },
}

fn rgb565_len(width: u16, height: u16) -> Result<usize, CodecError> {
    usize::from(width)
        .checked_mul(usize::from(height))
        .and_then(|n| n.checked_mul(2))
        .ok_or(CodecError::Length(usize::MAX))
}

fn read_surface_id(payload: &[u8]) -> Result<(String, usize), CodecError> {
    if payload.is_empty() {
        return Err(CodecError::Truncated);
    }
    let id_len = usize::from(payload[0]);
    let end = 1usize.checked_add(id_len).ok_or(CodecError::Truncated)?;
    if payload.len() < end {
        return Err(CodecError::Truncated);
    }
    let id = std::str::from_utf8(&payload[1..end])
        .map_err(|_| CodecError::Truncated)?
        .to_string();
    Ok((id, end))
}

/// Legacy full-frame payload (phase 0): phase + surface_id_len + id + frame_id + w + h + bytes.
pub fn encode_frame_payload(
    surface_id: &str,
    frame_id: u64,
    width: u16,
    height: u16,
    bytes: &[u8],
) -> Result<Vec<u8>, CodecError> {
    let id = surface_id.as_bytes();
    let id_len = u8::try_from(id.len()).map_err(|_| CodecError::Length(id.len()))?;
    let expected = rgb565_len(width, height)?;
    if bytes.len() != expected {
        return Err(CodecError::Length(bytes.len()));
    }
    let mut payload = Vec::with_capacity(1 + 1 + id.len() + 8 + 2 + 2 + bytes.len());
    payload.push(FRAME_PHASE_LEGACY);
    payload.push(id_len);
    payload.extend_from_slice(id);
    payload.extend_from_slice(&frame_id.to_le_bytes());
    payload.extend_from_slice(&width.to_le_bytes());
    payload.extend_from_slice(&height.to_le_bytes());
    payload.extend_from_slice(bytes);
    Ok(payload)
}

/// FrameBegin: phase=1 + surface_id_len + id + frame_id + w + h + total_bytes.
pub fn encode_frame_begin(
    surface_id: &str,
    frame_id: u64,
    width: u16,
    height: u16,
    total_bytes: u32,
) -> Result<Vec<u8>, CodecError> {
    let id = surface_id.as_bytes();
    let id_len = u8::try_from(id.len()).map_err(|_| CodecError::Length(id.len()))?;
    let expected = rgb565_len(width, height)?;
    if total_bytes as usize != expected {
        return Err(CodecError::Length(total_bytes as usize));
    }
    let mut payload = Vec::with_capacity(1 + 1 + id.len() + 8 + 2 + 2 + 4);
    payload.push(FRAME_PHASE_BEGIN);
    payload.push(id_len);
    payload.extend_from_slice(id);
    payload.extend_from_slice(&frame_id.to_le_bytes());
    payload.extend_from_slice(&width.to_le_bytes());
    payload.extend_from_slice(&height.to_le_bytes());
    payload.extend_from_slice(&total_bytes.to_le_bytes());
    Ok(payload)
}

/// FrameChunk: phase=2 + frame_id + offset + data.
pub fn encode_frame_chunk(frame_id: u64, offset: u32, data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + 8 + 4 + data.len());
    payload.push(FRAME_PHASE_CHUNK);
    payload.extend_from_slice(&frame_id.to_le_bytes());
    payload.extend_from_slice(&offset.to_le_bytes());
    payload.extend_from_slice(data);
    payload
}

/// FrameCommit: phase=3 + frame_id.
pub fn encode_frame_commit(frame_id: u64) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + 8);
    payload.push(FRAME_PHASE_COMMIT);
    payload.extend_from_slice(&frame_id.to_le_bytes());
    payload
}

// ---- Runtime package transfer (N22) --------------------------------------
// Payload phases mirror FRAME: 1 begin, 2 chunk, 3 commit. The container itself
// (MDCP magic + manifest + body) is the byte stream being transferred, so the
// begin only needs the total length and checksum for the device to size and
// verify its assembly buffer before the chunks arrive.

pub const PACKAGE_PHASE_BEGIN: u8 = 1;
pub const PACKAGE_PHASE_CHUNK: u8 = 2;
pub const PACKAGE_PHASE_COMMIT: u8 = 3;

/// Begin: [1][total_bytes u32 BE][checksum u64 BE].
pub fn encode_package_begin(total_bytes: u32, checksum: u64) -> Vec<u8> {
    let mut payload = Vec::with_capacity(13);
    payload.push(PACKAGE_PHASE_BEGIN);
    payload.extend_from_slice(&total_bytes.to_be_bytes());
    payload.extend_from_slice(&checksum.to_be_bytes());
    payload
}

/// Chunk: [2][offset u32 BE][data...].
pub fn encode_package_chunk(offset: u32, data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(5 + data.len());
    payload.push(PACKAGE_PHASE_CHUNK);
    payload.extend_from_slice(&offset.to_be_bytes());
    payload.extend_from_slice(data);
    payload
}

/// Commit: [3].
pub fn encode_package_commit() -> Vec<u8> {
    vec![PACKAGE_PHASE_COMMIT]
}

/// FNV-1a 64-bit over a whole package transfer: the checksum the begin carries
/// so the device can verify the assembled stream before parsing the container.
pub fn package_checksum(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackagePayload {
    Begin { total_bytes: u32, checksum: u64 },
    Chunk { offset: u32, data: Vec<u8> },
    Commit,
}

pub fn decode_package_payload(payload: &[u8]) -> Result<PackagePayload, CodecError> {
    let (phase, body) = payload.split_first().ok_or(CodecError::Truncated)?;
    match *phase {
        PACKAGE_PHASE_BEGIN => {
            if body.len() != 12 {
                return Err(CodecError::Truncated);
            }
            let total_bytes = u32::from_be_bytes(body[0..4].try_into().unwrap());
            let checksum = u64::from_be_bytes(body[4..12].try_into().unwrap());
            Ok(PackagePayload::Begin {
                total_bytes,
                checksum,
            })
        }
        PACKAGE_PHASE_CHUNK => {
            if body.len() < 4 {
                return Err(CodecError::Truncated);
            }
            let offset = u32::from_be_bytes(body[0..4].try_into().unwrap());
            Ok(PackagePayload::Chunk {
                offset,
                data: body[4..].to_vec(),
            })
        }
        PACKAGE_PHASE_COMMIT => {
            if body.is_empty() {
                Ok(PackagePayload::Commit)
            } else {
                Err(CodecError::Length(body.len()))
            }
        }
        other => Err(CodecError::UnknownType(other)),
    }
}

pub fn decode_frame_payload(payload: &[u8]) -> Result<FramePayload, CodecError> {
    if payload.is_empty() {
        return Err(CodecError::Truncated);
    }
    match payload[0] {
        FRAME_PHASE_LEGACY => {
            // layout: [phase][id_len][id...][frame_id][w][h][bytes]
            let body = &payload[1..];
            let (surface_id, id_end) = read_surface_id(body)?;
            let need = id_end + 8 + 2 + 2;
            if body.len() < need {
                return Err(CodecError::Truncated);
            }
            let frame_id = u64::from_le_bytes(body[id_end..id_end + 8].try_into().unwrap());
            let width = u16::from_le_bytes(body[id_end + 8..id_end + 10].try_into().unwrap());
            let height = u16::from_le_bytes(body[id_end + 10..id_end + 12].try_into().unwrap());
            let bytes = body[id_end + 12..].to_vec();
            let expected = rgb565_len(width, height)?;
            if bytes.len() != expected {
                return Err(CodecError::Length(bytes.len()));
            }
            Ok(FramePayload::Legacy {
                surface_id,
                frame_id,
                width,
                height,
                bytes,
            })
        }
        FRAME_PHASE_BEGIN => {
            let body = &payload[1..];
            let (surface_id, id_end) = read_surface_id(body)?;
            let need = id_end + 8 + 2 + 2 + 4;
            if body.len() < need {
                return Err(CodecError::Truncated);
            }
            if body.len() != need {
                return Err(CodecError::Length(body.len()));
            }
            let frame_id = u64::from_le_bytes(body[id_end..id_end + 8].try_into().unwrap());
            let width = u16::from_le_bytes(body[id_end + 8..id_end + 10].try_into().unwrap());
            let height = u16::from_le_bytes(body[id_end + 10..id_end + 12].try_into().unwrap());
            let total_bytes =
                u32::from_le_bytes(body[id_end + 12..id_end + 16].try_into().unwrap());
            let expected = rgb565_len(width, height)?;
            if total_bytes as usize != expected {
                return Err(CodecError::Length(total_bytes as usize));
            }
            Ok(FramePayload::Begin {
                surface_id,
                frame_id,
                width,
                height,
                total_bytes,
            })
        }
        FRAME_PHASE_CHUNK => {
            if payload.len() < 1 + 8 + 4 {
                return Err(CodecError::Truncated);
            }
            let frame_id = u64::from_le_bytes(payload[1..9].try_into().unwrap());
            let offset = u32::from_le_bytes(payload[9..13].try_into().unwrap());
            let data = payload[13..].to_vec();
            Ok(FramePayload::Chunk {
                frame_id,
                offset,
                data,
            })
        }
        FRAME_PHASE_COMMIT => {
            if payload.len() < 1 + 8 {
                return Err(CodecError::Truncated);
            }
            if payload.len() != 1 + 8 {
                return Err(CodecError::Length(payload.len()));
            }
            let frame_id = u64::from_le_bytes(payload[1..9].try_into().unwrap());
            Ok(FramePayload::Commit { frame_id })
        }
        other => Err(CodecError::UnknownPhase(other)),
    }
}

/// Tile: surface_id_len + id + base_frame_id + x + y + w + h + bytes.
pub fn encode_tile_payload(
    surface_id: &str,
    base_frame_id: u64,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    bytes: &[u8],
) -> Result<Vec<u8>, CodecError> {
    let id = surface_id.as_bytes();
    let id_len = u8::try_from(id.len()).map_err(|_| CodecError::Length(id.len()))?;
    let expected = rgb565_len(w, h)?;
    if bytes.len() != expected {
        return Err(CodecError::Length(bytes.len()));
    }
    let mut payload = Vec::with_capacity(1 + id.len() + 8 + 8 + bytes.len());
    payload.push(id_len);
    payload.extend_from_slice(id);
    payload.extend_from_slice(&base_frame_id.to_le_bytes());
    payload.extend_from_slice(&x.to_le_bytes());
    payload.extend_from_slice(&y.to_le_bytes());
    payload.extend_from_slice(&w.to_le_bytes());
    payload.extend_from_slice(&h.to_le_bytes());
    payload.extend_from_slice(bytes);
    Ok(payload)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TilePayload {
    pub surface_id: String,
    pub base_frame_id: u64,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub bytes: Vec<u8>,
}

pub fn decode_tile_payload(payload: &[u8]) -> Result<TilePayload, CodecError> {
    let (surface_id, id_end) = read_surface_id(payload)?;
    let need = id_end + 8 + 2 + 2 + 2 + 2;
    if payload.len() < need {
        return Err(CodecError::Truncated);
    }
    let base_frame_id = u64::from_le_bytes(payload[id_end..id_end + 8].try_into().unwrap());
    let x = u16::from_le_bytes(payload[id_end + 8..id_end + 10].try_into().unwrap());
    let y = u16::from_le_bytes(payload[id_end + 10..id_end + 12].try_into().unwrap());
    let w = u16::from_le_bytes(payload[id_end + 12..id_end + 14].try_into().unwrap());
    let h = u16::from_le_bytes(payload[id_end + 14..id_end + 16].try_into().unwrap());
    let bytes = payload[id_end + 16..].to_vec();
    let expected = rgb565_len(w, h)?;
    if bytes.len() != expected {
        return Err(CodecError::Length(bytes.len()));
    }
    Ok(TilePayload {
        surface_id,
        base_frame_id,
        x,
        y,
        w,
        h,
        bytes,
    })
}

pub fn encode_control<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    serde_cbor::to_vec(value)
        .map_err(|_| CodecError::Length(MAX_CONTROL_PAYLOAD + 1))
        .and_then(|v| {
            if v.len() > MAX_CONTROL_PAYLOAD {
                Err(CodecError::Length(v.len()))
            } else {
                Ok(v)
            }
        })
}
pub fn decode_control<'a, T: Deserialize<'a>>(payload: &'a [u8]) -> Result<T, CodecError> {
    serde_cbor::from_slice(payload).map_err(|_| CodecError::Truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn round_trip_and_partial() {
        let p = Packet {
            version: VERSION,
            kind: MessageType::Ping,
            flags: 0,
            request_id: 4,
            payload: vec![1, 2],
        };
        let b = p.encode(10).unwrap();
        assert_eq!(Packet::decode(&b[..5], 10), Err(CodecError::Truncated));
        let (q, n) = Packet::decode(&b, 10).unwrap();
        assert_eq!(n, b.len());
        assert_eq!(q, p);
    }
    #[test]
    fn rejects_bad_checksum() {
        let p = Packet {
            version: VERSION,
            kind: MessageType::Ping,
            flags: 0,
            request_id: 0,
            payload: vec![1],
        };
        let mut b = p.encode(10).unwrap();
        *b.last_mut().unwrap() = 2;
        assert_eq!(Packet::decode(&b, 10), Err(CodecError::Checksum));
        let mut noisy = b"boot log\n".to_vec();
        noisy.extend_from_slice(&b);
        match decode_stream(&noisy, 10) {
            StreamStep::Rejected {
                error: CodecError::Checksum,
                discard,
            } => {
                assert_eq!(discard, noisy.len());
                assert!(Packet::decode(&noisy[discard.saturating_sub(1)..], 10).is_err());
            }
            other => panic!("checksum must reject the packet, got {other:?}"),
        }
        let good = p.encode(10).unwrap();
        let mut prefixed = b"boot log\n".to_vec();
        prefixed.extend_from_slice(&good);
        match decode_stream(&prefixed, 10) {
            StreamStep::Packet(packet, consumed) => {
                assert_eq!(packet.kind, MessageType::Ping);
                assert_eq!(consumed, prefixed.len());
                assert_ne!(packet.payload, b"boot log\n");
            }
            other => panic!("noise should resync to the ping, got {other:?}"),
        }
    }
    #[test]
    fn frame_payload_rejects_wrong_geometry() {
        assert_eq!(
            encode_frame_payload("main", 1, 2, 2, &[0; 7]),
            Err(CodecError::Length(7))
        );
    }
    #[test]
    fn rejects_major_unknown_type_and_negotiated_limit() {
        let packet = Packet {
            version: VERSION,
            kind: MessageType::Ping,
            flags: 0,
            request_id: 0,
            payload: vec![1],
        };
        let mut bytes = packet.encode(10).unwrap();
        bytes[2] = 2;
        assert_eq!(Packet::decode(&bytes, 10), Err(CodecError::Major(2)));
        let mut bytes = packet.encode(10).unwrap();
        bytes[4] = 0x55; // still unknown after N303 reserved the 0x60..0x63 range
        bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            Packet::decode(&bytes[..HEADER_LEN], usize::MAX),
            Err(CodecError::UnknownType(0x55))
        );
        let mut reserved = packet.encode(10).unwrap();
        reserved[5] = 1;
        reserved[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            Packet::decode(&reserved[..HEADER_LEN], usize::MAX),
            Err(CodecError::Reserved)
        );
        let mut flags = packet.encode(10).unwrap();
        flags[6] = 1;
        flags[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            Packet::decode(&flags[..HEADER_LEN], usize::MAX),
            Err(CodecError::Flags)
        );
        assert_eq!(packet.encode(0), Err(CodecError::Length(1)));
    }
    #[test]
    fn zero_length_packet_is_valid_and_trailing_packet_is_not_lost() {
        let first = Packet {
            version: VERSION,
            kind: MessageType::Ping,
            flags: 0,
            request_id: 1,
            payload: vec![],
        }
        .encode(0)
        .unwrap();
        let second = Packet {
            version: VERSION,
            kind: MessageType::Pong,
            flags: 0,
            request_id: 2,
            payload: vec![],
        }
        .encode(0)
        .unwrap();
        let mut joined = first;
        joined.extend(second);
        let (_, used) = Packet::decode(&joined, 0).unwrap();
        let (next, _) = Packet::decode(&joined[used..], 0).unwrap();
        assert_eq!(next.kind, MessageType::Pong);
    }

    #[test]
    fn legacy_frame_round_trip_with_phase_zero() {
        let bytes = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let enc = encode_frame_payload("main", 42, 2, 2, &bytes).unwrap();
        assert_eq!(enc[0], FRAME_PHASE_LEGACY);
        match decode_frame_payload(&enc).unwrap() {
            FramePayload::Legacy {
                surface_id,
                frame_id,
                width,
                height,
                bytes: out,
            } => {
                assert_eq!(surface_id, "main");
                assert_eq!(frame_id, 42);
                assert_eq!((width, height), (2, 2));
                assert_eq!(out, bytes);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn chunked_frame_begin_chunk_commit_round_trip() {
        let pixels = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let begin = encode_frame_begin("main", 7, 2, 2, 8).unwrap();
        assert_eq!(begin[0], FRAME_PHASE_BEGIN);
        match decode_frame_payload(&begin).unwrap() {
            FramePayload::Begin {
                surface_id,
                frame_id,
                width,
                height,
                total_bytes,
            } => {
                assert_eq!(surface_id, "main");
                assert_eq!(frame_id, 7);
                assert_eq!((width, height, total_bytes), (2, 2, 8));
            }
            other => panic!("unexpected {other:?}"),
        }
        let chunk0 = encode_frame_chunk(7, 0, &pixels[..4]);
        let chunk1 = encode_frame_chunk(7, 4, &pixels[4..]);
        match decode_frame_payload(&chunk0).unwrap() {
            FramePayload::Chunk {
                frame_id,
                offset,
                data,
            } => {
                assert_eq!((frame_id, offset), (7, 0));
                assert_eq!(data, pixels[..4]);
            }
            other => panic!("unexpected {other:?}"),
        }
        match decode_frame_payload(&chunk1).unwrap() {
            FramePayload::Chunk { offset, data, .. } => {
                assert_eq!(offset, 4);
                assert_eq!(data, pixels[4..]);
            }
            other => panic!("unexpected {other:?}"),
        }
        let commit = encode_frame_commit(7);
        assert_eq!(
            decode_frame_payload(&commit).unwrap(),
            FramePayload::Commit { frame_id: 7 }
        );
    }

    #[test]
    fn tile_payload_round_trip() {
        let bytes = vec![0xaa, 0xbb, 0xcc, 0xdd];
        let enc = encode_tile_payload("main", 3, 1, 0, 2, 1, &bytes).unwrap();
        let dec = decode_tile_payload(&enc).unwrap();
        assert_eq!(dec.surface_id, "main");
        assert_eq!(dec.base_frame_id, 3);
        assert_eq!((dec.x, dec.y, dec.w, dec.h), (1, 0, 2, 1));
        assert_eq!(dec.bytes, bytes);
        assert_eq!(
            encode_tile_payload("main", 1, 0, 0, 1, 1, &[0]),
            Err(CodecError::Length(1))
        );
    }

    #[test]
    fn control_structs_round_trip() {
        let input = InputEvent {
            surface_id: "main".into(),
            pointer_id: 0,
            phase: "down".into(),
            x: 10,
            y: 20,

            node: None,
        };
        let ota = OtaCommand {
            action: "begin".into(),
            version: Some("1.0.0".into()),
            size: Some(100),
            sha256: None,
            offset: None,
            len: None,
        };
        let err = ErrorPayload {
            code: "E_UNSUPPORTED".into(),
            message: "touch off".into(),
        };
        assert_eq!(
            decode_control::<InputEvent>(&encode_control(&input).unwrap()).unwrap(),
            input
        );
        assert_eq!(
            decode_control::<OtaCommand>(&encode_control(&ota).unwrap()).unwrap(),
            ota
        );
        assert_eq!(
            decode_control::<ErrorPayload>(&encode_control(&err).unwrap()).unwrap(),
            err
        );
    }

    fn vectors_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/vectors")
    }

    fn parse_hex(hex: &str) -> Vec<u8> {
        let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        (0..clean.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).expect("hex"))
            .collect()
    }

    #[test]
    fn loads_protocol_vectors() {
        let dir = vectors_dir();
        let legacy = std::fs::read_to_string(dir.join("frame_legacy.json")).unwrap();
        // Minimal JSON parse: look for "hex": "..."
        let hex = legacy
            .split("\"hex\"")
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        let bytes = parse_hex(hex);
        match decode_frame_payload(&bytes).unwrap() {
            FramePayload::Legacy {
                surface_id,
                frame_id,
                width,
                height,
                ..
            } => {
                assert_eq!(surface_id, "main");
                assert_eq!(frame_id, 1);
                assert_eq!((width, height), (2, 2));
            }
            other => panic!("unexpected {other:?}"),
        }

        let begin = std::fs::read_to_string(dir.join("frame_begin.json")).unwrap();
        let hex = begin
            .split("\"hex\"")
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        let bytes = parse_hex(hex);
        assert!(matches!(
            decode_frame_payload(&bytes).unwrap(),
            FramePayload::Begin { .. }
        ));

        let tile = std::fs::read_to_string(dir.join("tile.json")).unwrap();
        let hex = tile
            .split("\"hex\"")
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        let bytes = parse_hex(hex);
        let t = decode_tile_payload(&bytes).unwrap();
        assert_eq!(t.w, 1);
        assert_eq!(t.h, 1);

        let mirror = std::fs::read_to_string(dir.join("mirror_frame.json")).unwrap();
        let hex = mirror
            .split("\"hex\"")
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        let bytes = parse_hex(hex);
        match decode_frame_payload(&bytes).unwrap() {
            FramePayload::Legacy {
                surface_id,
                frame_id,
                width,
                height,
                bytes: pixels,
            } => {
                assert_eq!(surface_id, "main");
                assert_eq!(frame_id, 21);
                assert_eq!((width, height), (2, 2));
                assert_eq!(pixels.len(), 8);
            }
            other => panic!("unexpected {other:?}"),
        }

        let truncated = std::fs::read(dir.join("truncated.bin")).unwrap();
        assert_eq!(Packet::decode(&truncated, 4096), Err(CodecError::Truncated));

        // N22: runtime package begin/chunk/commit share the Frame phase style.
        let package = std::fs::read_to_string(dir.join("package_begin.json")).unwrap();
        let hex = package
            .split("\"hex\"")
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        let bytes = parse_hex(hex);
        assert_eq!(
            decode_package_payload(&bytes).unwrap(),
            PackagePayload::Begin {
                total_bytes: 12,
                checksum: 0x0102_0304_0506_0708,
            }
        );
        assert_eq!(bytes, encode_package_begin(12, 0x0102_0304_0506_0708));
        let chunk = encode_package_chunk(4, &[0xaa, 0xbb]);
        assert_eq!(
            decode_package_payload(&chunk).unwrap(),
            PackagePayload::Chunk {
                offset: 4,
                data: vec![0xaa, 0xbb],
            }
        );
        assert_eq!(
            decode_package_payload(&encode_package_commit()).unwrap(),
            PackagePayload::Commit
        );
        assert_eq!(
            decode_package_payload(&[9]),
            Err(CodecError::UnknownType(9))
        );
    }

    #[test]
    fn diagnostic_payloads_round_trip() {
        // N316: the reserved Metric/BootInfo types now carry CBOR payloads.
        let metric = MetricReport {
            name: "fps".into(),
            value: 29.97,
            unit: Some("fps".into()),
            boot_id: 7,
            run_id: 2,
            event_seq: 42,
        };
        let back: MetricReport = decode_control(&encode_control(&metric).unwrap()).unwrap();
        assert_eq!(back, metric);
        // Defaulted fields stay off the wire.
        let bare = MetricReport {
            name: "heap".into(),
            value: 1024.0,
            unit: None,
            boot_id: 0,
            run_id: 0,
            event_seq: 0,
        };
        let bytes = encode_control(&bare).unwrap();
        assert!(!bytes.windows(7).any(|w| w == b"boot_id"));

        let boot = BootInfo {
            firmware: "1.0.0".into(),
            boot_id: 7,
            run_id: 2,
            reset_reason: Some("power-on".into()),
        };
        let back: BootInfo = decode_control(&encode_control(&boot).unwrap()).unwrap();
        assert_eq!(back, boot);
        let boot_vector = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../protocol/vectors/diagnostic_boot_info.json"),
        )
        .unwrap();
        let boot_hex = boot_vector
            .split("\"hex\"")
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        assert_eq!(encode_control(&boot).unwrap(), parse_hex(boot_hex));

        let vector = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../protocol/vectors/diagnostic_metric.json"),
        )
        .unwrap();
        let hex = vector
            .split("\"hex\"")
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        assert_eq!(encode_control(&metric).unwrap(), parse_hex(hex));

        // N335: the reserved Telemetry type carries a structured event.
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("fps".to_string(), "30".to_string());
        let event = TelemetryEvent {
            name: "soak".into(),
            level: Some("info".into()),
            fields,
            boot_id: 7,
            run_id: 2,
            event_seq: 9,
        };
        let back: TelemetryEvent = decode_control(&encode_control(&event).unwrap()).unwrap();
        assert_eq!(back, event);
        assert_eq!(MessageType::try_from(0x60), Ok(MessageType::Telemetry));

        // N367: the reserved Crash type carries a fault report.
        let crash = CrashReport {
            reason: "load-access".into(),
            task: Some("mdc_app".into()),
            pc: Some(0x4000_0000),
            boot_id: 7,
            run_id: 2,
        };
        let back: CrashReport = decode_control(&encode_control(&crash).unwrap()).unwrap();
        assert_eq!(back, crash);
    }

    #[test]
    fn loads_reserved_diagnostic_vector() {
        // N303: the reserved type table is shared with the C++ parser test.
        let dir = vectors_dir();
        let json = std::fs::read_to_string(dir.join("diagnostic_types.json")).unwrap();
        let hex = json
            .split("\"hex\"")
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        let bytes = parse_hex(hex);
        assert_eq!(MessageType::try_from(bytes[0]), Ok(MessageType::Telemetry));
        for (name, byte) in [
            ("Telemetry", 0x60u8),
            ("Metric", 0x61),
            ("Crash", 0x62),
            ("BootInfo", 0x63),
        ] {
            let needle = format!("\"{name}\": {}", byte);
            assert!(json.contains(&needle), "vector must list {name}");
            assert!(MessageType::try_from(byte).unwrap().is_diagnostic());
        }
    }

    #[test]
    fn mirror_frame_payload_round_trip_reuses_frame_phases() {
        let pixels = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let packet = Packet {
            version: VERSION,
            kind: MessageType::MirrorFrame,
            flags: 0,
            request_id: 9,
            payload: encode_frame_payload("main", 11, 2, 2, &pixels).unwrap(),
        };
        let bytes = packet.encode(pixels.len() + 64).unwrap();
        let (decoded, _) = Packet::decode(&bytes, pixels.len() + 64).unwrap();
        assert_eq!(decoded.kind, MessageType::MirrorFrame);
        match decode_frame_payload(&decoded.payload).unwrap() {
            FramePayload::Legacy {
                surface_id,
                frame_id,
                width,
                height,
                bytes,
            } => {
                assert_eq!(surface_id, "main");
                assert_eq!(frame_id, 11);
                assert_eq!((width, height), (2, 2));
                assert_eq!(bytes, pixels);
            }
            other => panic!("unexpected {other:?}"),
        }
        // MirrorFrame is distinct from Frame on the wire.
        assert_ne!(MessageType::MirrorFrame as u8, MessageType::Frame as u8);
        assert_eq!(MessageType::try_from(0x12), Ok(MessageType::MirrorFrame));
        assert_eq!(MessageType::try_from(0x31), Ok(MessageType::MirrorCtrl));
    }

    #[test]
    fn mirror_command_round_trips_over_cbor() {
        let command = MirrorCommand {
            action: "start".into(),
            surface_id: "main".into(),
            mode: "full-frame".into(),
            max_fps: 15,
            format: "RGB565".into(),
        };
        let decoded: MirrorCommand = decode_control(&encode_control(&command).unwrap()).unwrap();
        assert_eq!(decoded, command);
    }

    #[test]
    fn old_capabilities_without_mirror_fields_still_decode() {
        // Simulate a pre-N03 device: a CBOR map without the new keys.
        #[derive(Serialize)]
        struct OldCaps {
            device_id: String,
            firmware: String,
            surfaces: Vec<Surface>,
            frame: bool,
            tile: bool,
            touch: bool,
            ota: bool,
            max_message: u32,
            max_chunk: u32,
            max_in_flight: u8,
            max_fps: u16,
        }
        let old = OldCaps {
            device_id: "old".into(),
            firmware: "0.0.1".into(),
            surfaces: vec![Surface {
                id: "main".into(),
                width: 2,
                height: 2,
                pixel_format: "RGB565".into(),
                stride: 4,
                rotation: 0,
                preferred_tile_size: 0,
                supports_partial_update: false,
                supports_full_framebuffer: false,
            }],
            frame: true,
            tile: false,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 1024,
            max_in_flight: 1,
            max_fps: 1,
        };
        let bytes = encode_control(&old).unwrap();
        let caps: Capabilities = decode_control(&bytes).unwrap();
        assert_eq!(caps.device_id, "old");
        assert!(!caps.mirror.supports());
        assert_eq!(caps.mirror_fps, 0);
        assert_eq!(caps.local_display_fps, 0);
    }

    #[test]
    fn negotiate_mirror_intersects_mode_fps_and_format() {
        let mut caps = Capabilities {
            device_id: "gpu".into(),
            firmware: "1.0.0".into(),
            surfaces: vec![Surface {
                id: "main".into(),
                width: 480,
                height: 480,
                pixel_format: "RGB565".into(),
                stride: 960,
                rotation: 0,
                preferred_tile_size: 0,
                supports_partial_update: false,
                supports_full_framebuffer: false,
            }],
            frame: true,
            tile: false,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: MirrorCapability::disabled(),
            local_display_fps: 60,
            mirror_fps: 10,
            mirror_bandwidth: 5_000_000,
            renderer: None,
            config: ConfigCapability::default(),
            scene: SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        // No mirror advertised -> rejected.
        assert!(negotiate_mirror(&caps, "main", "full-frame", 30, "RGB565").is_none());
        // full-frame only: dirty-region downgrades, fps is clamped to 10.
        caps.mirror = MirrorCapability::full_frame(480, 480, 30);
        let cmd = negotiate_mirror(&caps, "main", "dirty-region", 30, "RGB565").unwrap();
        assert_eq!(cmd.mode, "full-frame");
        assert_eq!(cmd.max_fps, 10);
        assert_eq!(caps.local_display_fps, 60);
        assert_ne!(caps.local_display_fps, caps.mirror_fps);
        // Unknown format falls back to RGB565; unknown mode with no full-frame fails.
        assert_eq!(
            negotiate_mirror(&caps, "main", "full-frame", 5, "BGRA8")
                .unwrap()
                .format,
            "RGB565"
        );
        caps.mirror.mode = vec!["dirty-region".into()];
        assert!(negotiate_mirror(&caps, "main", "full-frame", 5, "RGB565").is_none());
    }

    #[test]
    fn renderer_capability_is_optional_and_round_trips() {
        let mut caps = Capabilities {
            device_id: "sim-linux-virt".into(),
            firmware: "0.1.0-linux".into(),
            surfaces: vec![Surface {
                id: "main".into(),
                width: 800,
                height: 480,
                pixel_format: "RGB565".into(),
                stride: 1600,
                rotation: 0,
                preferred_tile_size: 0,
                supports_partial_update: false,
                supports_full_framebuffer: false,
            }],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 1048576,
            max_chunk: 16384,
            max_in_flight: 1,
            max_fps: 30,
            mirror: MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: ConfigCapability::default(),
            scene: SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        // Absent renderer: the key is not serialized.
        let bytes = encode_control(&caps).unwrap();
        assert!(!bytes.windows(8).any(|w| w == b"renderer"));
        let back: Capabilities = decode_control(&bytes).unwrap();
        assert!(back.renderer.is_none());

        caps.renderer = Some(RendererCapability::full_3d(2048, 200_000));
        let bytes = encode_control(&caps).unwrap();
        // The C++ encoder reproduces these exact bytes (cbor_control_test.cpp).
        let back: Capabilities = decode_control(&bytes).unwrap();
        assert_eq!(back.renderer, caps.renderer);
    }

    #[test]
    fn config_capability_and_command_round_trip() {
        // An empty config capability is omitted entirely (map count unchanged).
        let mut caps = Capabilities {
            device_id: "cfg".into(),
            firmware: "1".into(),
            surfaces: vec![Surface {
                id: "main".into(),
                width: 2,
                height: 2,
                pixel_format: "RGB565".into(),
                stride: 4,
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
            max_chunk: 1024,
            max_in_flight: 1,
            max_fps: 30,
            mirror: MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: ConfigCapability::default(),
            scene: SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        };
        let bytes = encode_control(&caps).unwrap();
        assert!(!bytes.windows(6).any(|w| w == b"config"));

        caps.config = ConfigCapability::new(&["brightness", "rotation"]);
        let bytes = encode_control(&caps).unwrap();
        let back: Capabilities = decode_control(&bytes).unwrap();
        assert_eq!(back.config, caps.config);
        assert!(back.config.supports("brightness"));
        assert!(!back.config.supports("wifi_password"));

        let command = ConfigCommand {
            action: "set".into(),
            key: "brightness".into(),
            value: Some("128".into()),
        };
        let bytes = encode_control(&command).unwrap();
        let back: ConfigCommand = decode_control(&bytes).unwrap();
        assert_eq!(back, command);
        // Canonical bytes are recorded as a protocol vector.
        let vector = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../protocol/vectors/config_command.json"),
        )
        .unwrap();
        let hex = vector
            .split("\"hex\"")
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        assert_eq!(bytes, parse_hex(hex));

        let reply = ConfigReply {
            key: "brightness".into(),
            ok: true,
            value: Some("128".into()),
            error: None,
        };
        let bytes = encode_control(&reply).unwrap();
        let back: ConfigReply = decode_control(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    fn scene_command() -> SceneCommand {
        SceneCommand {
            action: "set".into(),
            scene: Some(SceneNode {
                name: "root".into(),
                kind: "rect".into(),
                rect: None,
                children: vec![SceneNode {
                    name: "label".into(),
                    kind: "text".into(),
                    rect: None,
                    children: Vec::new(),
                    value: Some("hi".into()),
                    opacity: None,
                    visible: None,
                    anim: None,
                    rotate: None,
                    blend: None,
                    align: None,
                    z: None,
                    wrap: None,
                    mode: None,
                    focus: None,
                    value2: None,
                }],
                value: None,
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                value2: None,
            }),
        }
    }

    #[test]
    fn scene_colour_parses_short_hex_and_names() {
        let coloured = |value: &str| SceneNode {
            name: "n".into(),
            kind: "rect".into(),
            rect: None,
            children: Vec::new(),
            value: Some(value.into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        // #RGB expands each nibble.
        assert_eq!(coloured("#F00").color_rgb565(), 0xF800);
        assert_eq!(coloured("#0f0").color_rgb565(), 0x07E0);
        // Names are case-insensitive.
        assert_eq!(coloured("Red").color_rgb565(), 0xF800);
        assert_eq!(coloured("BLUE").color_rgb565(), 0x001F);
        // Unknown names / malformed hex fall back to white.
        assert_eq!(coloured("chartreuse").color_rgb565(), 0xFFFF);
        assert_eq!(coloured("#12").color_rgb565(), 0xFFFF);
    }

    #[test]
    fn scene_rect_geometry_and_colour() {
        let node = SceneNode {
            name: "bg".into(),
            kind: "rect".into(),
            rect: Some([1, 2, 3, 4]),
            children: Vec::new(),
            value: Some("#FF0000".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        assert_eq!(node.color_rgb565(), 0xF800);
        assert_eq!(
            SceneNode {
                name: "x".into(),
                kind: "rect".into(),
                rect: None,
                children: Vec::new(),
                value: None,
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                value2: None,
            }
            .color_rgb565(),
            0xFFFF
        );
        let bytes = encode_control(&node).unwrap();
        let back: SceneNode = decode_control(&bytes).unwrap();
        assert_eq!(back, node);
        let vector = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../protocol/vectors/scene_rect.json"),
        )
        .unwrap();
        let hex = vector
            .split("\"hex\"")
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        assert_eq!(bytes, parse_hex(hex));
    }

    #[test]
    fn scene_capability_and_command_round_trip() {
        let mut caps = Capabilities {
            device_id: "dev1".into(),
            firmware: "1".into(),
            surfaces: vec![Surface {
                id: "main".into(),
                width: 2,
                height: 2,
                pixel_format: "RGB565".into(),
                stride: 4,
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
            max_chunk: 1024,
            max_in_flight: 1,
            max_fps: 30,
            mirror: MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: ConfigCapability::default(),
            scene: SceneCapability::enabled(64, 4),
            boot_id: 0,
            run_id: 0,
        };
        let bytes = encode_control(&caps).unwrap();
        let back: Capabilities = decode_control(&bytes).unwrap();
        assert_eq!(back.scene, caps.scene);

        // A disabled scene capability is omitted.
        caps.scene = SceneCapability::default();
        let bytes = encode_control(&caps).unwrap();
        assert!(!bytes.windows(5).any(|w| w == b"scene"));

        let command = scene_command();
        let root = command.scene.as_ref().unwrap();
        assert_eq!(root.count(), 2);
        assert_eq!(root.depth(), 2);
        let bytes = encode_control(&command).unwrap();
        let back: SceneCommand = decode_control(&bytes).unwrap();
        assert_eq!(back, command);
        let vector = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../protocol/vectors/scene_command.json"),
        )
        .unwrap();
        let hex = vector
            .split("\"hex\"")
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        assert_eq!(bytes, parse_hex(hex));
    }

    #[test]
    fn reserved_diagnostic_types_decode_without_breaking() {
        // N303: the reserved type values decode so a downstream pin is stable,
        // even though the payloads are not implemented yet.
        for (byte, kind) in [
            (0x60u8, MessageType::Telemetry),
            (0x61, MessageType::Metric),
            (0x62, MessageType::Crash),
            (0x63, MessageType::BootInfo),
        ] {
            assert_eq!(MessageType::try_from(byte), Ok(kind));
            assert!(kind.is_diagnostic());
            let packet = Packet {
                version: VERSION,
                kind,
                flags: 0,
                request_id: 7,
                payload: vec![1, 2, 3],
            };
            let bytes = packet.encode(64).unwrap();
            let (decoded, _) = Packet::decode(&bytes, 64).unwrap();
            assert_eq!(decoded.kind, kind);
            assert_eq!(decoded.payload, vec![1, 2, 3]);
        }
        assert!(!MessageType::Ping.is_diagnostic());
        let mut bytes = Packet {
            version: VERSION,
            kind: MessageType::Ping,
            flags: 0,
            request_id: 0,
            payload: vec![],
        }
        .encode(0)
        .unwrap();
        bytes[4] = 0x6f;
        assert_eq!(
            Packet::decode(&bytes, 0),
            Err(CodecError::UnknownType(0x6f))
        );
    }

    #[test]
    fn boot_and_run_ids_are_additive_and_default_to_zero() {
        // N304: the new field is omitted when zero, so a pre-N304 peer is byte
        // identical, and carried through when set.
        let hello = Hello {
            major: 1,
            minor: 0,
            max_message: 4096,
            max_chunk: 1024,
            boot_id: 0,
        };
        let bytes = encode_control(&hello).unwrap();
        assert!(!bytes.windows(7).any(|w| w == b"boot_id"));
        let hello2 = Hello {
            boot_id: 0xDEAD_BEEF,
            ..hello
        };
        let back: Hello = decode_control(&encode_control(&hello2).unwrap()).unwrap();
        assert_eq!(back.boot_id, 0xDEAD_BEEF);

        let ack = Ack {
            received: true,
            displayed: true,
            frame_id: 1,
            error: None,
            run_id: 4,
            event_seq: 9,
        };
        let back: Ack = decode_control(&encode_control(&ack).unwrap()).unwrap();
        assert_eq!(back, ack);
        let bare = Ack {
            received: true,
            displayed: false,
            frame_id: 2,
            error: None,
            run_id: 0,
            event_seq: 0,
        };
        let bytes = encode_control(&bare).unwrap();
        assert!(!bytes.windows(6).any(|w| w == b"run_id"));
    }

    #[test]
    fn pixel_format_enum_keeps_the_wire_string() {
        // N307: only RGB565 is implemented; unknown names survive the round trip
        // but are rejected with an explicit error.
        assert_eq!(PixelFormat::from("RGB565"), PixelFormat::Rgb565);
        assert_eq!(PixelFormat::from("rgb565"), PixelFormat::Rgb565);
        assert_eq!(PixelFormat::from("RGBA8888"), PixelFormat::Rgba8888);
        assert_eq!(
            PixelFormat::from("YUV420"),
            PixelFormat::Other("YUV420".into())
        );
        assert!(PixelFormat::Rgb565.is_implemented());
        for format in [
            PixelFormat::Rgb888,
            PixelFormat::Rgba8888,
            PixelFormat::Gray1,
            PixelFormat::Gray4,
            PixelFormat::Other("YUV420".into()),
        ] {
            assert!(!format.is_implemented());
        }
        let generated = config_command_vector();
        assert_eq!(generated.surfaces[0].pixel_format, PixelFormat::Rgb565);
        // The wire keeps the canonical string, not the Rust variant name.
        let bytes = encode_control(&generated.surfaces[0]).unwrap();
        assert!(bytes.windows(6).any(|w| w == b"RGB565"));
    }

    #[test]
    fn old_surface_without_display_profile_still_decodes() {
        // N308: a pre-N308 Surface map without the display fields decodes to the
        // neutral defaults (no preference, no partial/full claims).
        #[derive(Serialize)]
        struct OldSurface {
            id: String,
            width: u16,
            height: u16,
            pixel_format: String,
            stride: u32,
            rotation: u16,
        }
        let old = OldSurface {
            id: "main".into(),
            width: 2,
            height: 2,
            pixel_format: "RGB565".into(),
            stride: 4,
            rotation: 0,
        };
        let surface: Surface = decode_control(&encode_control(&old).unwrap()).unwrap();
        assert_eq!(surface.pixel_format, PixelFormat::Rgb565);
        assert_eq!(surface.preferred_tile_size, 0);
        assert!(!surface.supports_partial_update);
        assert!(!surface.supports_full_framebuffer);
    }

    /// Build a minimal node with only a kind and name.
    fn plain_node(name: &str, kind: &str) -> SceneNode {
        SceneNode {
            name: name.into(),
            kind: kind.into(),
            rect: None,
            children: Vec::new(),
            value: None,
            value2: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
        }
    }

    #[test]
    fn focus_keyframes_sample_and_drive_the_focus_field() {
        // N546: focus keyframes interpolate (clamped to 0..=100) and are applied
        // onto the node together with opacity/rect.
        let anim = SceneAnim {
            duration_ms: 100,
            looping: false,
            opacity: Vec::new(),
            rect: Vec::new(),
            focus: vec![[0, 0, 100], [100, 100, 0]],
            delay_ms: None,
            speed_permille: None,
            color: Vec::new(),
            rotate: Vec::new(),
            easing: None,
        };
        assert_eq!(anim.focus_at(0), Some([0, 100]));
        assert_eq!(anim.focus_at(50), Some([50, 50]));
        assert_eq!(anim.focus_at(500), Some([100, 0]));
        assert_eq!(anim.focus_at(0).unwrap()[0], 0);

        let mut node = plain_node("g", "gradient");
        node.anim = Some(anim);
        apply_scene_animation(&mut node, 25);
        assert_eq!(node.focus.as_deref(), Some("25,75"));
        assert_eq!(node.anim_keys(), 2, "focus keys count toward the budget");

        // A node with no focus track keeps its authored focus value.
        let mut untouched = plain_node("g", "gradient");
        untouched.focus = Some("10,20".into());
        untouched.anim = Some(SceneAnim {
            duration_ms: 10,
            looping: true,
            opacity: vec![[0, 128]],
            rect: Vec::new(),
            focus: Vec::new(),
            delay_ms: None,
            speed_permille: None,
            color: Vec::new(),
            rotate: Vec::new(),
            easing: None,
        });
        apply_scene_animation(&mut untouched, 5);
        assert_eq!(untouched.focus.as_deref(), Some("10,20"));
    }

    #[test]
    fn scene_animation_easing_shapes_segments_without_moving_endpoints() {
        // N724: ease-in/out/in-out change the midpoint shape but keep t=0/1 exact.
        let anim = |easing: &str| SceneAnim {
            duration_ms: 1000,
            looping: false,
            opacity: vec![[0, 0], [1000, 255]],
            rect: vec![[0, 0, 0, 10, 10], [1000, 100, 10, 10, 10]],
            focus: vec![[0, 0, 100], [1000, 100, 0]],
            delay_ms: None,
            speed_permille: None,
            color: Vec::new(),
            rotate: Vec::new(),
            easing: Some(easing.into()),
        };
        let linear = anim("linear");
        assert_eq!(linear.opacity_at(0), Some(0));
        assert_eq!(linear.opacity_at(1000), Some(255));
        assert_eq!(linear.opacity_at(250), Some(64));
        let ease_in = anim("ease-in");
        assert_eq!(ease_in.opacity_at(0), Some(0));
        assert_eq!(ease_in.opacity_at(1000), Some(255));
        assert!(
            ease_in.opacity_at(250).unwrap() < 64,
            "ease-in is slower at the start"
        );
        let ease_out = anim("ease-out");
        assert!(ease_out.opacity_at(250).unwrap() > 64);
        let ease_in_out = anim("ease-in-out");
        assert!(ease_in_out.opacity_at(250).unwrap() < 64);
        assert_eq!(ease_in_out.opacity_at(500), Some(128));
        // Rect and focus use the same curve.
        assert!(ease_in.rect_at(250).unwrap()[0] < linear.rect_at(250).unwrap()[0]);
        assert!(ease_in.focus_at(250).unwrap()[0] < 50);
        // An unknown name falls back to linear.
        assert_eq!(anim("bogus").opacity_at(250), linear.opacity_at(250));
        // Applying the animation writes the eased opacity onto the node.
        let mut node = plain_node("n", "rect");
        node.anim = Some(ease_in);
        apply_scene_animation(&mut node, 250);
        assert_eq!(node.opacity, Some(16));
    }

    #[test]
    fn scene_delay_and_speed_scale_the_animation_clock() {
        // N823: the first keyframe is held until `delay_ms`; speed scales the
        // elapsed time afterwards. Zero speed keeps the authored timing.
        let anim = |delay: Option<u32>, speed: Option<u16>| SceneAnim {
            duration_ms: 100,
            looping: true,
            opacity: vec![[0, 0], [100, 255]],
            rect: Vec::new(),
            focus: Vec::new(),
            delay_ms: delay,
            speed_permille: speed,
            color: Vec::new(),
            rotate: Vec::new(),
            easing: None,
        };
        let delayed = anim(Some(50), Some(2000));
        assert_eq!(delayed.opacity_at(0), Some(0));
        assert_eq!(delayed.opacity_at(49), Some(0));
        assert_eq!(delayed.opacity_at(50), Some(0), "delay holds the first key");
        assert_eq!(delayed.opacity_at(75), Some(128), "25 ms at 2x = 50 ms");
        assert_eq!(delayed.opacity_at(100), Some(0), "wraps after 50 ms at 2x");
        // A zero/absent speed is 1000 permille, not a freeze or a divide by zero.
        assert_eq!(anim(None, Some(0)).opacity_at(50), Some(128));
        assert_eq!(anim(None, None).opacity_at(50), Some(128));
    }

    #[test]
    fn scene_animation_new_fields_are_additive() {
        // N823/N825/N827: an old CBOR animation payload decodes unchanged and
        // the new tracks stay absent on the wire when unset.
        #[derive(Serialize)]
        struct OldAnim {
            duration_ms: u32,
            looping: bool,
            opacity: Vec<[u16; 2]>,
        }
        let old_bytes = serde_cbor::to_vec(&OldAnim {
            duration_ms: 10,
            looping: true,
            opacity: vec![[0, 255]],
        })
        .unwrap();
        let old: SceneAnim = serde_cbor::from_slice(&old_bytes).unwrap();
        assert!(old.delay_ms.is_none());
        assert!(old.speed_permille.is_none());
        assert!(old.color.is_empty());
        assert!(old.rotate.is_empty());
        assert_eq!(old.opacity_at(5), Some(255));
        let wire = String::from_utf8_lossy(&serde_cbor::to_vec(&old).unwrap()).into_owned();
        assert!(!wire.contains("delay_ms"));
        assert!(!wire.contains("color"));
        assert!(!wire.contains("rotate"));

        let new = SceneAnim {
            duration_ms: 10,
            delay_ms: Some(5),
            speed_permille: Some(2000),
            color: vec![SceneColorKey {
                time_ms: 0,
                value: "red".into(),
            }],
            rotate: vec![[0, 90]],
            ..SceneAnim::default()
        };
        assert_eq!(new.local_time(10), 10);
        assert_eq!(new.color_at(0), Some([255, 0, 0]));
        assert_eq!(new.rotate_at(0), Some(90));
    }

    #[test]
    fn scene_colour_keyframes_drive_colour_values_only() {
        // N825: colour keys interpolate per channel and write `value` on colour
        // kinds. Text/image/sprite payloads are never overwritten.
        let anim = SceneAnim {
            duration_ms: 100,
            looping: false,
            opacity: Vec::new(),
            rect: Vec::new(),
            focus: Vec::new(),
            delay_ms: None,
            speed_permille: None,
            color: vec![
                SceneColorKey {
                    time_ms: 0,
                    value: "#000000".into(),
                },
                SceneColorKey {
                    time_ms: 100,
                    value: "#FFFFFF".into(),
                },
            ],
            rotate: Vec::new(),
            easing: None,
        };
        assert_eq!(anim.color_at(0), Some([0, 0, 0]));
        assert_eq!(anim.color_at(50), Some([128, 128, 128]));
        assert_eq!(anim.color_at(500), Some([255, 255, 255]));
        // Keys are sampled in time order even when authored out of order.
        let unsorted = SceneAnim {
            color: vec![
                SceneColorKey {
                    time_ms: 100,
                    value: "#FFFFFF".into(),
                },
                SceneColorKey {
                    time_ms: 0,
                    value: "#000000".into(),
                },
            ],
            ..anim.clone()
        };
        assert_eq!(unsorted.color_at(0), Some([0, 0, 0]));
        assert_eq!(unsorted.color_at(50), Some([128, 128, 128]));
        // Invalid values are skipped instead of falling back to white.
        let partial = SceneAnim {
            color: vec![
                SceneColorKey {
                    time_ms: 0,
                    value: "not-a-colour".into(),
                },
                SceneColorKey {
                    time_ms: 10,
                    value: "red".into(),
                },
            ],
            ..anim.clone()
        };
        assert_eq!(partial.color_at(0), Some([255, 0, 0]));

        let mut rect = plain_node("r", "rect");
        rect.anim = Some(anim.clone());
        apply_scene_animation(&mut rect, 50);
        assert_eq!(rect.value.as_deref(), Some("#808080"));
        assert_eq!(rect.anim_keys(), 2, "colour keys count toward the budget");

        let mut label = plain_node("t", "text");
        label.value = Some("hi".into());
        label.anim = Some(anim);
        apply_scene_animation(&mut label, 50);
        assert_eq!(label.value.as_deref(), Some("hi"), "glyphs are preserved");
        assert!(!kind_uses_colour_value("text"));
        assert!(kind_uses_colour_value("rect"));
        assert!(kind_uses_colour_value("path"));
    }

    #[test]
    fn scene_rotate_keyframes_snap_to_quarter_turns() {
        // N827: the sampled angle rounds to the nearest multiple of 90.
        let anim = SceneAnim {
            duration_ms: 100,
            looping: true,
            opacity: Vec::new(),
            rect: Vec::new(),
            focus: Vec::new(),
            delay_ms: None,
            speed_permille: None,
            color: Vec::new(),
            rotate: vec![[0, 0], [100, 360]],
            easing: None,
        };
        assert_eq!(anim.rotate_at(0), Some(0));
        assert_eq!(anim.rotate_at(25), Some(90));
        assert_eq!(anim.rotate_at(50), Some(180));
        assert_eq!(anim.rotate_at(75), Some(270));
        assert_eq!(anim.rotate_at(100), Some(0), "360 wraps to 0");
        let mut rect = plain_node("r", "rect");
        rect.anim = Some(anim);
        apply_scene_animation(&mut rect, 50);
        assert_eq!(rect.rotate, Some(180));
    }

    #[test]
    fn scene_lint_reports_animation_keys_and_bad_tracks() {
        // N829: the report counts sampled keys and flags unusable tracks.
        let mut node = plain_node("r", "rect");
        node.rect = Some([0, 0, 4, 4]);
        node.value = Some("#FFFFFF".into());
        node.anim = Some(SceneAnim {
            duration_ms: 10,
            looping: false,
            opacity: vec![[20, 255]],
            rect: Vec::new(),
            focus: Vec::new(),
            delay_ms: None,
            speed_permille: None,
            color: vec![SceneColorKey {
                time_ms: 0,
                value: "bogus".into(),
            }],
            rotate: vec![[0, 45]],
            easing: None,
        });
        let report = node.lint();
        assert_eq!(report.anim_keys, 3);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("invalid colour key")),
            "{:?}",
            report.warnings
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("nearest quarter turn")),
            "{:?}",
            report.warnings
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("after its 10 ms duration")),
            "{:?}",
            report.warnings
        );

        let mut over = plain_node("r", "rect");
        over.rect = Some([0, 0, 1, 1]);
        over.value = Some("#FFFFFF".into());
        over.anim = Some(SceneAnim {
            duration_ms: 1000,
            looping: true,
            opacity: (0..=SCENE_MAX_ANIM_KEYS as u16).map(|k| [k, 128]).collect(),
            ..SceneAnim::default()
        });
        let report = over.lint();
        assert!(!report.valid);
        assert_eq!(report.anim_keys, SCENE_MAX_ANIM_KEYS + 1);
    }

    #[test]
    fn scene_polygon_bezier_and_arc_parsing_is_bounded() {
        // N723/N733/N734: point, curve and angle specs are parsed and bounded.
        let mut node = plain_node("p", "polygon");
        node.value2 = Some("0,0;4,0;4,4".into());
        assert_eq!(node.points().unwrap().len(), 3);
        node.value2 = Some("0,0;1,1".into());
        assert_eq!(node.points().unwrap().len(), 2);
        node.value2 = Some("bad".into());
        assert!(node.points().is_none());
        node.value2 = Some(
            (0..65)
                .map(|i| format!("{i},0"))
                .collect::<Vec<_>>()
                .join(";"),
        );
        assert!(node.points().is_none());

        let mut path = plain_node("c", "path");
        path.value2 = Some("0,0;4,4;8,0".into());
        assert_eq!(path.bezier().unwrap().len(), 3);
        path.value2 = Some("0,0;2,2;4,4;8,0".into());
        assert_eq!(path.bezier().unwrap().len(), 4);
        path.value2 = Some("0,0;8,0".into());
        assert!(path.bezier().is_none());

        let mut arc = plain_node("a", "arc");
        arc.value2 = Some("350,30".into());
        let (start, sweep) = arc.arc_angles().unwrap();
        assert!((start - 350.0).abs() < 0.01);
        assert!((sweep - 40.0).abs() < 0.01);
        arc.value2 = Some("0,360".into());
        assert_eq!(arc.arc_angles(), Some((0.0, 360.0)));
        arc.value2 = Some("x,y".into());
        assert!(arc.arc_angles().is_none());
    }

    #[test]
    fn scene_path_command_stream_is_bounded() {
        // N743: M/L/Q/C/Z parse into commands; a stream must start with M and
        // stay within 64 commands.
        let mut path = plain_node("p", "path");
        path.value2 = Some("M1,1;L5,1;Q8,4 5,8;C1,1 2,2 3,3;Z".into());
        let commands = path.path_commands().unwrap();
        assert_eq!(commands.len(), 5);
        assert_eq!(commands[0], PathCommand::Move([1, 1]));
        assert_eq!(commands[4], PathCommand::Close);
        path.value2 = Some("L1,1;L2,2".into());
        assert!(path.path_commands().is_none(), "must start with M");
        path.value2 = Some(
            std::iter::once("M0,0".to_string())
                .chain((0..64).map(|i| format!("L{i},0")))
                .collect::<Vec<_>>()
                .join(";"),
        );
        assert!(path.path_commands().is_none(), "over 64 commands");

        // N753: elliptical arcs and smooth quadratics parse into commands.
        path.value2 = Some("M0,0;A4,2,0,0,1,8,0;T12,4;Z".into());
        let commands = path.path_commands().unwrap();
        assert_eq!(commands.len(), 4);
        assert_eq!(
            commands[1],
            PathCommand::Arc {
                rx: 4,
                ry: 2,
                rotation: 0,
                large_arc: false,
                sweep: true,
                end: [8, 0],
            }
        );
        assert_eq!(commands[2], PathCommand::SmoothQuad([12, 4]));
        // Arc flags are strict and the radius is non-negative.
        path.value2 = Some("M0,0;A4,2,0,2,1,8,0".into());
        assert!(path.path_commands().is_none(), "large-arc must be 0/1");
        path.value2 = Some("M0,0;A-4,-2,0,0,1,8,0".into());
        assert!(matches!(
            path.path_commands().unwrap()[1],
            PathCommand::Arc { rx: 4, ry: 2, .. }
        ));
    }

    #[test]
    fn relative_path_commands_accumulate_and_smooth_cubics_parse() {
        // N763: lowercase commands are relative to the current point and can be
        // mixed with absolute ones; `S` mirrors the previous cubic control.
        let mut path = plain_node("p", "path");
        path.value2 = Some("M10,10;l5,0;q5,5 5,10;t5,0;s2,2 4,4;a3,3,0,0,1,4,0;z".into());
        let commands = path.path_commands().unwrap();
        assert_eq!(commands[0], PathCommand::Move([10, 10]));
        assert_eq!(commands[1], PathCommand::Line([15, 10]));
        assert_eq!(commands[2], PathCommand::Quad([20, 15], [20, 20]));
        assert_eq!(commands[3], PathCommand::SmoothQuad([25, 20]));
        assert_eq!(commands[4], PathCommand::SmoothCubic([27, 22], [29, 24]));
        assert_eq!(
            commands[5],
            PathCommand::Arc {
                rx: 3,
                ry: 3,
                rotation: 0,
                large_arc: false,
                sweep: true,
                end: [33, 24],
            }
        );
        assert_eq!(commands[6], PathCommand::Close);
        // Absolute `S` followed by a relative `m` offsets from its end point.
        path.value2 = Some("M2,2;L6,2;S8,4 10,2;m1,1;l1,0".into());
        let commands = path.path_commands().unwrap();
        assert_eq!(commands[2], PathCommand::SmoothCubic([8, 4], [10, 2]));
        assert_eq!(commands[3], PathCommand::Move([11, 3]));
        assert_eq!(commands[4], PathCommand::Line([12, 3]));
        // Overflowing relative coordinates saturate rather than wrap.
        path.value2 = Some("M2147483647,0;l1,0".into());
        assert_eq!(
            path.path_commands().unwrap()[1],
            PathCommand::Line([i32::MAX, 0])
        );

        // N773: H/V are single-axis lines, absolute or relative.
        path.value2 = Some("M2,3;H10;v4;h-5;V2;z".into());
        let commands = path.path_commands().unwrap();
        assert_eq!(
            commands,
            vec![
                PathCommand::Move([2, 3]),
                PathCommand::Horizontal(10),
                PathCommand::Vertical(7),
                PathCommand::Horizontal(5),
                PathCommand::Vertical(2),
                PathCommand::Close,
            ]
        );
        path.value2 = Some("M1,1;H".into());
        assert!(path.path_commands().is_none(), "H needs one number");

        // N783: extra coordinate groups repeat the command. A repeated moveto
        // is an implicit lineto, and a ragged tail is rejected.
        path.value2 = Some("M2,3 8,3;L10,3 10,9;H12 14;v2 3;Q1,1 2,2 3,3 4,4".into());
        let commands = path.path_commands().unwrap();
        assert_eq!(
            commands[..6],
            [
                PathCommand::Move([2, 3]),
                PathCommand::Line([8, 3]),
                PathCommand::Line([10, 3]),
                PathCommand::Line([10, 9]),
                PathCommand::Horizontal(12),
                PathCommand::Horizontal(14),
            ]
        );
        assert_eq!(commands[6], PathCommand::Vertical(11));
        assert_eq!(commands[7], PathCommand::Vertical(14));
        assert_eq!(commands[8], PathCommand::Quad([1, 1], [2, 2]));
        assert_eq!(commands[9], PathCommand::Quad([3, 3], [4, 4]));
        path.value2 = Some("m1,1 2,0;c1,0 2,0 3,0 1,1 2,1 3,1".into());
        let commands = path.path_commands().unwrap();
        assert_eq!(commands[0], PathCommand::Move([1, 1]));
        assert_eq!(commands[1], PathCommand::Line([3, 1]));
        assert_eq!(commands[2], PathCommand::Cubic([4, 1], [5, 1], [6, 1]));
        assert_eq!(commands[3], PathCommand::Cubic([7, 2], [8, 2], [9, 2]));
        path.value2 = Some("M0,0;L1,1 2".into());
        assert!(path.path_commands().is_none(), "ragged coordinate group");
        path.value2 = Some(format!(
            "M0,0;L{}",
            (0..64)
                .map(|i| format!("{i},0"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        assert!(
            path.path_commands().is_none(),
            "implicit repeats count toward the 64-command budget"
        );
    }

    #[test]
    fn path_stroke_width_reads_a_mode_token() {
        // N793: `width=N` is 1..=8. Missing and invalid tokens stay at 1 pixel.
        let mut path = plain_node("p", "path");
        assert_eq!(path.stroke_width(), 1);
        path.mode = Some("width=3".into());
        assert_eq!(path.stroke_width(), 3);
        path.mode = Some("linear,width=8".into());
        assert_eq!(path.stroke_width(), 8);
        path.mode = Some("width=0".into());
        assert_eq!(path.stroke_width(), 1);
        path.mode = Some("width=9".into());
        assert_eq!(path.stroke_width(), 1);
        path.mode = Some("width=nope".into());
        assert_eq!(path.stroke_width(), 1);
        path.mode = Some("width=4,width=nope".into());
        assert_eq!(path.stroke_width(), 4);
        // N803: `dash=N` is 1..=64 and does not change the stroke width.
        assert_eq!(path.stroke_dash(), 0);
        path.mode = Some("dash=4,width=3".into());
        assert_eq!(path.stroke_dash(), 4);
        assert_eq!(path.stroke_width(), 3);
        path.mode = Some("width=5,dash=64".into());
        assert_eq!(path.stroke_dash(), 64);
        assert_eq!(path.stroke_width(), 5);
        path.mode = Some("dash=0".into());
        assert_eq!(path.stroke_dash(), 0);
        path.mode = Some("dash=65".into());
        assert_eq!(path.stroke_dash(), 0);
        path.mode = Some("dash=nope".into());
        assert_eq!(path.stroke_dash(), 0);
        path.mode = Some("dash=8,dash=nope".into());
        assert_eq!(path.stroke_dash(), 8);
        assert_eq!(path.stroke_width(), 1);
        // N813: `phase=N` is 0..=64; missing/invalid stays 0 and a later
        // invalid token does not clear an earlier phase.
        assert_eq!(path.stroke_phase(), 0);
        path.mode = Some("dash=4,phase=6".into());
        assert_eq!(path.stroke_phase(), 6);
        assert_eq!(path.stroke_dash(), 4);
        path.mode = Some("phase=64,dash=4".into());
        assert_eq!(path.stroke_phase(), 64);
        path.mode = Some("phase=65".into());
        assert_eq!(path.stroke_phase(), 0);
        path.mode = Some("phase=nope".into());
        assert_eq!(path.stroke_phase(), 0);
        path.mode = Some("phase=6,phase=nope".into());
        assert_eq!(path.stroke_phase(), 6);
        path.mode = Some("phase=0".into());
        assert_eq!(path.stroke_phase(), 0);
    }

    #[test]
    fn group_skew_parses_milliunits_and_clamps() {
        // N759: `group` shear travels in `value2` (permille), so no new field is
        // needed and old scenes decode unchanged.
        let mut group = plain_node("g", "group");
        assert_eq!(group.skew(), None);
        group.value2 = Some("500,-250".into());
        assert_eq!(group.skew(), Some((500, -250)));
        group.value2 = Some("9999,-9999".into());
        assert_eq!(group.skew(), Some((2000, -2000)));
        group.value2 = Some("nope".into());
        assert_eq!(group.skew(), None);
    }

    fn config_command_vector() -> Capabilities {
        Capabilities {
            device_id: "vector".into(),
            firmware: "1".into(),
            surfaces: vec![Surface {
                id: "main".into(),
                width: 2,
                height: 2,
                pixel_format: PixelFormat::Rgb565,
                stride: 4,
                rotation: 0,
                preferred_tile_size: 16,
                supports_partial_update: true,
                supports_full_framebuffer: true,
            }],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 1024,
            max_in_flight: 1,
            max_fps: 30,
            mirror: MirrorCapability::disabled(),
            local_display_fps: 0,
            mirror_fps: 0,
            mirror_bandwidth: 0,
            renderer: None,
            config: ConfigCapability::default(),
            scene: SceneCapability::default(),
            boot_id: 0,
            run_id: 0,
        }
    }
}
