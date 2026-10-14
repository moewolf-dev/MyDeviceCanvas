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
    Error = 0x7f,
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
}

/// Vector scene limits a device advertises (N53). `max_nodes == 0` means the
/// device does not implement the Scene path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SceneCapability {
    pub max_nodes: u32,
    pub max_depth: u32,
}
impl SceneCapability {
    pub fn enabled(max_nodes: u32, max_depth: u32) -> Self {
        Self {
            max_nodes,
            max_depth,
        }
    }
    pub fn is_disabled(&self) -> bool {
        self.max_nodes == 0
    }
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
}
impl SceneNode {
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
    /// N79: the fill colour for a `rect`, parsed from `value` as `#RRGGBB` /
    /// `RRGGBB`. Missing or malformed values use opaque white.
    pub fn color_rgb565(&self) -> u16 {
        let Some(value) = self.value.as_deref() else {
            return 0xFFFF;
        };
        let hex = value.strip_prefix('#').unwrap_or(value);
        if hex.len() != 6 {
            return 0xFFFF;
        }
        let Ok(rgb) = u32::from_str_radix(hex, 16) else {
            return 0xFFFF;
        };
        let r = ((rgb >> 16) & 0xff) as u16;
        let g = ((rgb >> 8) & 0xff) as u16;
        let b = (rgb & 0xff) as u16;
        ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)
    }
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
}
impl ConfigCapability {
    pub fn new(keys: &[&str]) -> Self {
        Self {
            keys: keys.iter().map(|k| (*k).to_string()).collect(),
        }
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Surface {
    pub id: String,
    pub width: u16,
    pub height: u16,
    pub pixel_format: String,
    pub stride: u32,
    pub rotation: u16,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hello {
    pub major: u8,
    pub minor: u8,
    pub max_message: u32,
    pub max_chunk: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ack {
    pub received: bool,
    pub displayed: bool,
    pub frame_id: u64,
    pub error: Option<String>,
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
}

/// OTA control command (CBOR for MessageType::Ota).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OtaCommand {
    /// begin | abort | confirm
    pub action: String,
    pub version: Option<String>,
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
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
        bytes[4] = 99;
        bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            Packet::decode(&bytes[..HEADER_LEN], usize::MAX),
            Err(CodecError::UnknownType(99))
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
        };
        let ota = OtaCommand {
            action: "begin".into(),
            version: Some("1.0.0".into()),
            size: Some(100),
            sha256: None,
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
                }],
                value: None,
            }),
        }
    }

    #[test]
    fn scene_rect_geometry_and_colour() {
        let node = SceneNode {
            name: "bg".into(),
            kind: "rect".into(),
            rect: Some([1, 2, 3, 4]),
            children: Vec::new(),
            value: Some("#FF0000".into()),
        };
        assert_eq!(node.color_rgb565(), 0xF800);
        assert_eq!(
            SceneNode {
                name: "x".into(),
                kind: "rect".into(),
                rect: None,
                children: Vec::new(),
                value: None,
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
}
