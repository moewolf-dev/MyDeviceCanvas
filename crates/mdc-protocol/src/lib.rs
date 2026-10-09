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
    Frame = 0x10,
    Tile = 0x11,
    Input = 0x20,
    Ota = 0x30,
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
            0x20 => Ok(Self::Input),
            0x30 => Ok(Self::Ota),
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
        let kind = MessageType::try_from(input[4])?;
        let len = u32::from_le_bytes(input[12..16].try_into().unwrap()) as usize;
        if len > max_payload {
            return Err(CodecError::Length(len));
        }
        let total = HEADER_LEN.checked_add(len).ok_or(CodecError::Length(len))?;
        if input.len() < total {
            return Err(CodecError::Truncated);
        }
        let payload = input[HEADER_LEN..total].to_vec();
        if checksum(&payload) != u32::from_le_bytes(input[16..20].try_into().unwrap()) {
            return Err(CodecError::Checksum);
        }
        Ok((
            Self {
                version,
                kind,
                flags: u16::from_le_bytes(input[6..8].try_into().unwrap()),
                request_id: u32::from_le_bytes(input[8..12].try_into().unwrap()),
                payload,
            },
            total,
        ))
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
        assert_eq!(Packet::decode(&bytes, 10), Err(CodecError::UnknownType(99)));
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
        let pixels = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
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

        let truncated = std::fs::read(dir.join("truncated.bin")).unwrap();
        assert_eq!(
            Packet::decode(&truncated, 4096),
            Err(CodecError::Truncated)
        );
    }
}
