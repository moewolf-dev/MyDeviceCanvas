//! Fake device peer for Host Core / protocol integration tests.
use mdc_core::{validate_tile_base, FrameAssembler, Tile, TransactionNack, MAX_FRAME_BYTES};
use mdc_protocol::{
    decode_control, decode_frame_payload, decode_tile_payload, encode_control, Ack, Capabilities,
    ErrorPayload, FramePayload, Hello, MessageType, Packet, Surface, VERSION,
};
use mdc_transport::{MemoryLink, Transport, TransportError};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SimError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("disconnected")]
    Disconnected,
    #[error("base frame mismatch")]
    BaseMismatch,
}

/// Virtual time counter (no wall clock).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtualClock {
    pub tick: u64,
}
impl VirtualClock {
    pub fn advance(&mut self, by: u64) {
        self.tick = self.tick.saturating_add(by);
    }
}

/// Optional fault injection for simulator peer tests.
#[derive(Debug, Clone, Default)]
pub struct FaultInjector {
    pub drop_next: bool,
    /// Defer processing for this many virtual ticks after enqueue.
    pub delay_ticks: u64,
    pub disconnect: bool,
}
impl FaultInjector {
    pub fn drop_next(&mut self) {
        self.drop_next = true;
    }
    pub fn delay(&mut self, ticks: u64) {
        self.delay_ticks = ticks;
    }
    pub fn request_disconnect(&mut self) {
        self.disconnect = true;
    }
}

/// Board profile dimensions used for Capabilities / pixel buffer.
#[derive(Debug, Clone)]
pub struct BoardProfile {
    pub device_id: String,
    pub width: u16,
    pub height: u16,
    pub touch: bool,
    pub ota: bool,
}
impl Default for BoardProfile {
    fn default() -> Self {
        Self {
            device_id: "sim-esp32-jc3248w535".into(),
            width: 480,
            height: 320,
            touch: true,
            ota: true,
        }
    }
}

struct PendingDelay {
    ready_at: u64,
    packet: Packet,
}

/// FakeDevice peer speaking Protocol v1 over MemoryLink.
pub struct FakeDevice {
    link: MemoryLink,
    profile: BoardProfile,
    pixels: Vec<u8>,
    current_frame_id: u64,
    assembler: Option<(String, u16, u16, FrameAssembler)>,
    pub clock: VirtualClock,
    pub faults: FaultInjector,
    rx: Vec<u8>,
    max_message: usize,
    pub max_chunk: usize,
    delayed: Vec<PendingDelay>,
    connected: bool,
}
impl FakeDevice {
    pub fn pair(profile: BoardProfile) -> (MemoryLink, Self) {
        let (host, device) = MemoryLink::pair();
        let pixels = vec![0u8; usize::from(profile.width) * usize::from(profile.height) * 2];
        (
            host,
            Self {
                link: device,
                profile,
                pixels,
                current_frame_id: 0,
                assembler: None,
                clock: VirtualClock::default(),
                faults: FaultInjector::default(),
                rx: Vec::new(),
                max_message: MAX_FRAME_BYTES,
                max_chunk: mdc_protocol::DEFAULT_MAX_CHUNK,
                delayed: Vec::new(),
                connected: true,
            },
        )
    }
    pub fn with_default_profile() -> (MemoryLink, Self) {
        Self::pair(BoardProfile::default())
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn current_frame_id(&self) -> u64 {
        self.current_frame_id
    }
    pub fn assert_pixels_eq(&self, expected: &[u8]) {
        assert_eq!(self.pixels, expected, "pixel buffer mismatch");
    }
    pub fn capabilities(&self) -> Capabilities {
        let stride = u32::from(self.profile.width) * 2;
        Capabilities {
            device_id: self.profile.device_id.clone(),
            firmware: "mdc-simulator".into(),
            surfaces: vec![Surface {
                id: "main".into(),
                width: self.profile.width,
                height: self.profile.height,
                pixel_format: "RGB565".into(),
                stride,
                rotation: 0,
            }],
            frame: true,
            tile: true,
            touch: self.profile.touch,
            ota: self.profile.ota,
            max_message: self.max_message as u32,
            max_chunk: self.max_chunk as u32,
            max_in_flight: 1,
            max_fps: 30,
        }
    }
    /// Advance virtual clock and process inbound packets.
    pub fn poll(&mut self) -> Result<(), SimError> {
        if self.faults.disconnect || !self.connected {
            self.connected = false;
            self.link.close();
            return Err(SimError::Disconnected);
        }
        while let Some(bytes) = self
            .link
            .read()
            .map_err(|e| SimError::Transport(e.to_string()))?
        {
            self.rx.extend(bytes);
        }
        loop {
            match Packet::decode(&self.rx, self.max_message) {
                Ok((packet, consumed)) => {
                    self.rx.drain(..consumed);
                    self.enqueue_or_handle(packet)?;
                }
                Err(mdc_protocol::CodecError::Truncated) => break,
                Err(e) => return Err(SimError::Protocol(e.to_string())),
            }
        }
        self.flush_delayed()?;
        Ok(())
    }
    pub fn advance(&mut self, ticks: u64) -> Result<(), SimError> {
        self.clock.advance(ticks);
        self.poll()
    }
    fn enqueue_or_handle(&mut self, packet: Packet) -> Result<(), SimError> {
        if self.faults.drop_next {
            self.faults.drop_next = false;
            return Ok(());
        }
        if self.faults.delay_ticks > 0 {
            let ready_at = self.clock.tick.saturating_add(self.faults.delay_ticks);
            self.faults.delay_ticks = 0;
            self.delayed.push(PendingDelay { ready_at, packet });
            return Ok(());
        }
        self.handle_packet(packet)
    }
    fn flush_delayed(&mut self) -> Result<(), SimError> {
        let mut still = Vec::new();
        let mut ready = Vec::new();
        for item in self.delayed.drain(..) {
            if item.ready_at <= self.clock.tick {
                ready.push(item.packet);
            } else {
                still.push(item);
            }
        }
        self.delayed = still;
        for packet in ready {
            self.handle_packet(packet)?;
        }
        Ok(())
    }
    fn handle_packet(&mut self, packet: Packet) -> Result<(), SimError> {
        match packet.kind {
            MessageType::Hello => {
                let hello: Hello = decode_control(&packet.payload)
                    .map_err(|e| SimError::Protocol(e.to_string()))?;
                if hello.major != VERSION.major {
                    return self.reply_error(
                        packet.request_id,
                        "E_VERSION",
                        "unsupported major",
                    );
                }
                self.max_message = self
                    .max_message
                    .min(hello.max_message as usize)
                    .min(MAX_FRAME_BYTES);
                if hello.max_chunk > 0 {
                    self.max_chunk = self.max_chunk.min(hello.max_chunk as usize);
                }
                let caps = self.capabilities();
                self.write_control(MessageType::Capabilities, packet.request_id, &caps)
            }
            MessageType::Frame => self.handle_frame(packet),
            MessageType::Tile => self.handle_tile(packet),
            MessageType::Ping => self.write_packet(MessageType::Pong, packet.request_id, Vec::new()),
            MessageType::Input | MessageType::Ota => {
                if (packet.kind == MessageType::Input && !self.profile.touch)
                    || (packet.kind == MessageType::Ota && !self.profile.ota)
                {
                    return self.reply_error(
                        packet.request_id,
                        "E_UNSUPPORTED",
                        "capability disabled",
                    );
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    fn handle_frame(&mut self, packet: Packet) -> Result<(), SimError> {
        let decoded = decode_frame_payload(&packet.payload)
            .map_err(|e| SimError::Protocol(e.to_string()))?;
        match decoded {
            FramePayload::Legacy {
                surface_id: _,
                frame_id,
                width,
                height,
                bytes,
            } => {
                if width != self.profile.width || height != self.profile.height {
                    return self.reply_error(packet.request_id, "E_DIM", "geometry mismatch");
                }
                self.pixels = bytes;
                self.current_frame_id = frame_id;
                self.assembler = None;
                self.ack(packet.request_id, frame_id, true, true)
            }
            FramePayload::Begin {
                surface_id,
                frame_id,
                width,
                height,
                total_bytes,
            } => {
                if width != self.profile.width || height != self.profile.height {
                    return self.reply_error(packet.request_id, "E_DIM", "geometry mismatch");
                }
                let asm = FrameAssembler::begin(frame_id, total_bytes as usize, self.max_chunk)
                    .map_err(|e| SimError::Protocol(e.to_string()))?;
                self.assembler = Some((surface_id, width, height, asm));
                Ok(())
            }
            FramePayload::Chunk {
                frame_id,
                offset,
                data,
            } => {
                let Some((_, _, _, asm)) = self.assembler.as_mut() else {
                    return self.reply_error(packet.request_id, "E_CHUNK", "no begin");
                };
                if asm.frame_id != frame_id {
                    return self.reply_error(packet.request_id, "E_CHUNK", "frame_id mismatch");
                }
                if let Err(nack) = asm.chunk(offset as usize, &data) {
                    return self.reply_error(
                        packet.request_id,
                        "E_CHUNK",
                        &format!("{nack:?}"),
                    );
                }
                Ok(())
            }
            FramePayload::Commit { frame_id } => {
                let Some((_sid, _w, _h, asm)) = self.assembler.take() else {
                    return self.reply_error(packet.request_id, "E_COMMIT", "no begin");
                };
                if asm.frame_id != frame_id {
                    return self.reply_error(packet.request_id, "E_COMMIT", "frame_id mismatch");
                }
                match asm.commit() {
                    Ok(bytes) => {
                        self.pixels = bytes;
                        self.current_frame_id = frame_id;
                        self.ack(packet.request_id, frame_id, true, true)
                    }
                    Err(TransactionNack::Incomplete) => {
                        self.reply_error(packet.request_id, "E_INCOMPLETE", "incomplete frame")
                    }
                    Err(other) => {
                        self.reply_error(packet.request_id, "E_COMMIT", &format!("{other:?}"))
                    }
                }
            }
        }
    }
    fn handle_tile(&mut self, packet: Packet) -> Result<(), SimError> {
        let tile = decode_tile_payload(&packet.payload)
            .map_err(|e| SimError::Protocol(e.to_string()))?;
        let core_tile = Tile {
            surface_id: tile.surface_id,
            base_frame_id: tile.base_frame_id,
            x: tile.x,
            y: tile.y,
            width: tile.w,
            height: tile.h,
            bytes: tile.bytes,
        };
        if let Err(TransactionNack::BaseFrameMismatch) =
            validate_tile_base(&core_tile, self.current_frame_id)
        {
            return self.reply_error(packet.request_id, "E_BASE", "base frame mismatch");
        }
        let stride = usize::from(self.profile.width) * 2;
        let row_bytes = usize::from(core_tile.width) * 2;
        for row in 0..usize::from(core_tile.height) {
            let dst_y = usize::from(core_tile.y) + row;
            let dst_off = dst_y * stride + usize::from(core_tile.x) * 2;
            let src_off = row * row_bytes;
            self.pixels[dst_off..dst_off + row_bytes]
                .copy_from_slice(&core_tile.bytes[src_off..src_off + row_bytes]);
        }
        self.ack(packet.request_id, self.current_frame_id, true, true)
    }
    fn ack(
        &mut self,
        request_id: u32,
        frame_id: u64,
        received: bool,
        displayed: bool,
    ) -> Result<(), SimError> {
        let ack = Ack {
            received,
            displayed,
            frame_id,
            error: None,
        };
        self.write_control(MessageType::Ack, request_id, &ack)
    }
    fn reply_error(&mut self, request_id: u32, code: &str, message: &str) -> Result<(), SimError> {
        let err = ErrorPayload {
            code: code.into(),
            message: message.into(),
        };
        self.write_control(MessageType::Error, request_id, &err)
    }
    fn write_control<T: serde::Serialize>(
        &mut self,
        kind: MessageType,
        request_id: u32,
        value: &T,
    ) -> Result<(), SimError> {
        let payload =
            encode_control(value).map_err(|e| SimError::Protocol(e.to_string()))?;
        self.write_packet(kind, request_id, payload)
    }
    fn write_packet(
        &mut self,
        kind: MessageType,
        request_id: u32,
        payload: Vec<u8>,
    ) -> Result<(), SimError> {
        if !self.connected {
            return Err(SimError::Disconnected);
        }
        let packet = Packet {
            version: VERSION,
            kind,
            flags: 0,
            request_id,
            payload,
        };
        let bytes = packet
            .encode(self.max_message)
            .map_err(|e| SimError::Protocol(e.to_string()))?;
        match self.link.write(&bytes) {
            Ok(()) => Ok(()),
            Err(TransportError::Closed) => {
                self.connected = false;
                Err(SimError::Disconnected)
            }
            Err(e) => Err(SimError::Transport(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdc_core::{ConnectionState, Frame, Session, Tile};

    #[test]
    fn hello_capabilities_and_legacy_frame() {
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        assert_eq!(
            session.device.as_ref().unwrap().capabilities.device_id,
            "sim-esp32-jc3248w535"
        );
        let pixels = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: pixels.clone(),
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device.assert_pixels_eq(&pixels);
        assert_eq!(session.pending_request_count(), 0);
    }

    #[test]
    fn accepts_chunked_frame_and_tile_with_base_check() {
        let profile = BoardProfile {
            width: 4,
            height: 2,
            device_id: "chunk-sim".into(),
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        // Force small chunk on device side via Hello negotiation: Session sends DEFAULT_MAX_CHUNK;
        // override after handshake by setting device.max_chunk before frame — Session uses caps.
        let mut session = Session::new(host);
        session.connect().unwrap();
        // Shrink device max_chunk before answering so capabilities advertise 8.
        device.max_chunk = 8;
        device.poll().unwrap();
        session.poll().unwrap();

        let pixels: Vec<u8> = (0..16).collect();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 4,
                height: 2,
                bytes: pixels.clone(),
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device.assert_pixels_eq(&pixels);
        assert_eq!(device.current_frame_id(), 1);

        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 1,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0xaa, 0xbb],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(&device.pixels()[2..4], &[0xaa, 0xbb]);

        // Bad base is rejected (Error); session surfaces protocol error on poll.
        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 99,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0, 0],
            })
            .unwrap();
        device.poll().unwrap();
        assert!(session.poll().is_err());
    }

    #[test]
    fn fault_injector_drop_delay_disconnect() {
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.faults.drop_next();
        device.poll().unwrap(); // HELLO dropped
        assert_ne!(session.poll().unwrap(), ConnectionState::Ready);

        // Re-send path: new session pair for delay + disconnect
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.faults.delay(5);
        device.poll().unwrap(); // HELLO delayed
        assert_ne!(session.poll().unwrap(), ConnectionState::Ready);
        device.advance(5).unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        device.faults.request_disconnect();
        assert_eq!(device.poll(), Err(SimError::Disconnected));
    }

    #[test]
    fn default_profile_is_480x320() {
        let (_, device) = FakeDevice::with_default_profile();
        assert_eq!((device.profile.width, device.profile.height), (480, 320));
        assert_eq!(device.pixels().len(), 480 * 320 * 2);
    }
}
