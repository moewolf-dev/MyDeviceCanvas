//! Fake device peer for Host Core / protocol integration tests.
use mdc_core::{validate_tile_base, FrameAssembler, Tile, TransactionNack, MAX_FRAME_BYTES};
use mdc_protocol::{
    decode_control, decode_frame_payload, decode_package_payload, decode_tile_payload,
    encode_control, Ack, Capabilities, ConfigCommand, ConfigReply, ErrorPayload, FramePayload,
    Hello, InputEvent, MessageType, MirrorCommand, OtaCommand, PackagePayload, Packet, Surface,
    VERSION,
};
use mdc_transport::{MemoryLink, Transport, TransportError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

/// 虚拟时钟上，begin 之后这么多个 tick 没有 chunk 就回滚。
const OTA_CHUNK_TIMEOUT_TICKS: u64 = 5_000;

/// N41: baseline managed configuration.
fn default_config() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert("brightness".into(), "128".into());
    map.insert("rotation".into(), "0".into());
    map.insert("wifi_ssid".into(), "mdc-sim".into());
    map.insert("wifi_password".into(), "mdc-sim-secret".into());
    map
}

/// N41: secret values are never echoed back in a reply.
fn is_secret_key(key: &str) -> bool {
    key.contains("password") || key.contains("token")
}

/// N41: reject obviously invalid values before storing them.
fn validate_config(key: &str, value: &str) -> Result<(), &'static str> {
    if value.len() > 256 {
        return Err("E_VALUE");
    }
    match key {
        "brightness" => value.parse::<u8>().map(|_| ()).map_err(|_| "E_VALUE"),
        "rotation" => match value {
            "0" | "90" | "180" | "270" => Ok(()),
            _ => Err("E_VALUE"),
        },
        "wifi_ssid" => {
            if value.len() <= 32 {
                Ok(())
            } else {
                Err("E_VALUE")
            }
        }
        "wifi_password" => {
            if !value.is_empty() && value.len() <= 63 {
                Ok(())
            } else {
                Err("E_VALUE")
            }
        }
        _ => Ok(()),
    }
}

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
    /// 先放行这么多个包，再丢掉接下来的一个，并清掉未完成的分块重组。
    pub drop_skip: u32,
    pub armed_drop: bool,
    /// Defer processing for this many virtual ticks after enqueue.
    pub delay_ticks: u64,
    pub disconnect: bool,
}
impl FaultInjector {
    pub fn drop_next(&mut self) {
        self.drop_next = true;
    }
    /// `accept` 个包正常处理，然后丢掉下一个包。用来丢掉分块帧的 commit。
    pub fn drop_one_after(&mut self, accept: u32) {
        self.drop_skip = accept;
        self.armed_drop = true;
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
    /// 彩色刷新承诺。墨水屏必须为 false，不能把彩屏 FPS 写成该板能力。
    pub color_fps: bool,
    /// 触摸坐标旋转，仅接受 0/90/180/270。
    pub rotation: u16,
    /// 第二块 Surface。宽或高为 0 表示没有 overlay。
    pub overlay_width: u16,
    pub overlay_height: u16,
    /// Form B: device renders locally and mirrors frames back to the host.
    pub mirror: bool,
    pub mirror_fps: u16,
    pub local_display_fps: u16,
    /// N27: local renderer capability advertised on the wire (not inferred from
    /// `mirror`). Drives the model-compiler target backend.
    pub renderer_sprite: bool,
    pub renderer_mesh2d: bool,
    pub renderer_mesh3d: bool,
    pub renderer_skeleton: bool,
    pub renderer_max_texture: u32,
    pub renderer_max_vertices: u32,
    /// N41: configuration keys this board accepts.
    pub config_keys: Vec<String>,
}
impl Default for BoardProfile {
    fn default() -> Self {
        Self {
            device_id: "sim-esp32-jc3248w535".into(),
            width: 480,
            height: 320,
            touch: true,
            ota: true,
            color_fps: true,
            rotation: 0,
            overlay_width: 0,
            overlay_height: 0,
            // ESP32-S3 class: Form A only; no GPU to render + mirror at once.
            mirror: false,
            mirror_fps: 0,
            local_display_fps: 30,
            // MCU class: sprites only.
            renderer_sprite: true,
            renderer_mesh2d: false,
            renderer_mesh3d: false,
            renderer_skeleton: false,
            renderer_max_texture: 512,
            renderer_max_vertices: 0,
            // N41: a baseline board accepts a few managed settings.
            config_keys: vec![
                "brightness".into(),
                "rotation".into(),
                "wifi_ssid".into(),
                "wifi_password".into(),
            ],
        }
    }
}
impl BoardProfile {
    /// Resolve a known board id used by CLI `--board`.
    pub fn from_board_id(id: &str) -> Self {
        match id {
            "esp32-jc3248w535-sim" | "default" | "" => Self::default(),
            "linux-virt" => Self {
                device_id: "sim-linux-virt".into(),
                width: 800,
                height: 480,
                touch: false,
                ota: false,
                color_fps: true,
                // Form B: a Linux board with a GPU renders and mirrors.
                mirror: true,
                mirror_fps: 10,
                renderer_mesh2d: true,
                renderer_mesh3d: true,
                renderer_skeleton: true,
                renderer_max_texture: 2048,
                renderer_max_vertices: 200_000,
                ..Self::default()
            },
            "pi4-hdmi" => Self {
                device_id: "sim-pi4-hdmi".into(),
                width: 640,
                height: 480,
                touch: false,
                ota: false,
                color_fps: true,
                mirror: true,
                mirror_fps: 15,
                renderer_mesh2d: true,
                renderer_mesh3d: true,
                renderer_skeleton: true,
                renderer_max_texture: 2048,
                renderer_max_vertices: 200_000,
                ..Self::default()
            },
            "waveshare-eink" => Self {
                device_id: "sim-waveshare-eink".into(),
                width: 800,
                height: 480,
                touch: false,
                ota: false,
                color_fps: false,
                ..Self::default()
            },
            "dual-sim" => Self {
                device_id: "sim-dual".into(),
                width: 8,
                height: 4,
                touch: false,
                ota: false,
                color_fps: true,
                overlay_width: 4,
                overlay_height: 2,
                ..Self::default()
            },
            other => Self {
                device_id: format!("sim-{other}"),
                ..Self::default()
            },
        }
    }
}

/// Counters from the last simulate / poll session (inspector JSON).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SimMetrics {
    pub device_id: String,
    pub frames_received: u64,
    pub tiles_received: u64,
    pub inputs_emitted: u64,
    pub ota_commands: u64,
    pub ota_rollbacks: u64,
    pub acks_sent: u64,
    pub errors_sent: u64,
    pub mirror_frames: u64,
    pub mirror_commands: u64,
    pub packages_received: u64,
    pub config_commands: u64,
    pub current_frame_id: u64,
    pub pixel_bytes: usize,
}

struct PendingDelay {
    ready_at: u64,
    packet: Packet,
}

/// FakeDevice peer speaking Protocol v1 over any [`Transport`].
pub struct FakeDevice {
    link: Box<dyn Transport>,
    profile: BoardProfile,
    pixels: Vec<u8>,
    overlay_pixels: Vec<u8>,
    current_frame_id: u64,
    assembler: Option<(String, u16, u16, FrameAssembler)>,
    pub clock: VirtualClock,
    pub faults: FaultInjector,
    rx: Vec<u8>,
    max_message: usize,
    pub max_chunk: usize,
    delayed: Vec<PendingDelay>,
    connected: bool,
    pub metrics: SimMetrics,
    ota_active: bool,
    /// begin 之后若虚拟时钟越过这个 tick 还没有 chunk，就回滚并解除冻结。
    ota_deadline: Option<u64>,
    firmware: String,
    pending_version: Option<String>,
    /// Emitted once after the next displayed full frame. TCP peers use this
    /// because the host cannot call `emit_input` on another process.
    input_after_frame: Option<InputEvent>,
    /// 仍按下的 pointer。断线时按最后一次合法坐标补发 cancel。
    active_pointers: BTreeMap<u16, (String, u16, u16)>,
    /// Virtual tick when an e-ink refresh finishes. Color panels leave this at 0.
    busy_until: u64,
    /// Form B: set by a host MirrorCtrl start; emit_mirror_frame is the device side.
    mirror_active: bool,
    /// N23: runtime package being assembled from PACKAGE chunks.
    package_assembler: Option<mdc_model::PackageAssembler>,
    /// N23: true once a rendered-ready package was accepted.
    package_ready: bool,
    /// N41: managed configuration values.
    config: BTreeMap<String, String>,
}
impl FakeDevice {
    pub fn from_transport(link: Box<dyn Transport>, profile: BoardProfile) -> Self {
        let pixels = vec![0u8; usize::from(profile.width) * usize::from(profile.height) * 2];
        let overlay_pixels = if profile.overlay_width > 0 && profile.overlay_height > 0 {
            vec![0u8; usize::from(profile.overlay_width) * usize::from(profile.overlay_height) * 2]
        } else {
            Vec::new()
        };
        let device_id = profile.device_id.clone();
        Self {
            link,
            profile,
            pixels,
            overlay_pixels,
            current_frame_id: 0,
            assembler: None,
            clock: VirtualClock::default(),
            faults: FaultInjector::default(),
            rx: Vec::new(),
            max_message: MAX_FRAME_BYTES,
            max_chunk: mdc_protocol::DEFAULT_MAX_CHUNK,
            delayed: Vec::new(),
            connected: true,
            metrics: SimMetrics {
                device_id,
                pixel_bytes: 0,
                ..SimMetrics::default()
            },
            ota_active: false,
            ota_deadline: None,
            firmware: "1.0.0".into(),
            pending_version: None,
            input_after_frame: None,
            active_pointers: BTreeMap::new(),
            busy_until: 0,
            mirror_active: false,
            package_assembler: None,
            package_ready: false,
            config: default_config(),
        }
    }

    pub fn pair(profile: BoardProfile) -> (MemoryLink, Self) {
        let (host, device) = MemoryLink::pair();
        (host, Self::from_transport(Box::new(device), profile))
    }
    pub fn with_default_profile() -> (MemoryLink, Self) {
        Self::pair(BoardProfile::default())
    }

    /// Replace the byte channel (e.g. after host reconnect with a new MemoryLink pair).
    pub fn replace_transport(&mut self, link: Box<dyn Transport>) {
        self.link = link;
        self.connected = true;
        self.rx.clear();
        self.delayed.clear();
        self.assembler = None;
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn current_frame_id(&self) -> u64 {
        self.current_frame_id
    }
    pub fn firmware(&self) -> &str {
        &self.firmware
    }
    /// Queue one input event to send after the next displayed full frame.
    pub fn emit_input_after_next_frame(&mut self, event: InputEvent) {
        self.input_after_frame = Some(event);
    }
    /// TCP peer helper: one `down` event after the next displayed full frame.
    pub fn emit_down_after_next_frame(&mut self, x: u16, y: u16) {
        self.emit_input_after_next_frame(InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: "down".into(),
            x,
            y,
        });
    }
    fn flush_scripted_input(&mut self) -> Result<(), SimError> {
        if let Some(event) = self.input_after_frame.take() {
            return self.emit_input(event);
        }
        Ok(())
    }
    pub fn pending_version(&self) -> Option<&str> {
        self.pending_version.as_deref()
    }
    pub fn profile(&self) -> &BoardProfile {
        &self.profile
    }
    pub fn metrics_snapshot(&self) -> SimMetrics {
        let mut m = self.metrics.clone();
        m.current_frame_id = self.current_frame_id;
        m.pixel_bytes = self.pixels.len();
        m.device_id = self.profile.device_id.clone();
        m
    }
    pub fn overlay_pixels(&self) -> &[u8] {
        &self.overlay_pixels
    }
    fn surface_size(&self, surface_id: &str) -> Option<(u16, u16)> {
        if surface_id == "main" {
            Some((self.profile.width, self.profile.height))
        } else if surface_id == "overlay"
            && self.profile.overlay_width > 0
            && self.profile.overlay_height > 0
        {
            Some((self.profile.overlay_width, self.profile.overlay_height))
        } else {
            None
        }
    }
    fn store_surface_pixels(
        &mut self,
        surface_id: &str,
        width: u16,
        height: u16,
        bytes: Vec<u8>,
    ) -> Result<(), SimError> {
        let Some((expect_w, expect_h)) = self.surface_size(surface_id) else {
            return Err(SimError::Protocol("unknown surface".into()));
        };
        if width != expect_w || height != expect_h {
            return Err(SimError::Protocol("geometry".into()));
        }
        let expected = usize::from(expect_w) * usize::from(expect_h) * 2;
        if bytes.len() != expected {
            return Err(SimError::Protocol("geometry".into()));
        }
        if surface_id == "overlay" {
            self.overlay_pixels = bytes;
        } else {
            self.pixels = bytes;
        }
        Ok(())
    }
    pub fn assert_pixels_eq(&self, expected: &[u8]) {
        assert_eq!(self.pixels, expected, "pixel buffer mismatch");
    }
    /// Emit a touch/input event toward the host session (capability-gated).
    pub fn emit_input(&mut self, event: InputEvent) -> Result<(), SimError> {
        if !self.profile.touch {
            return Err(SimError::Protocol("touch capability disabled".into()));
        }
        let (x, y) = mdc_core::transform_touch(
            self.profile.width,
            self.profile.height,
            self.profile.rotation,
            event.x,
            event.y,
        )
        .map_err(|_| SimError::Protocol("touch out of bounds".into()))?;
        let event = InputEvent { x, y, ..event };
        match event.phase.as_str() {
            "up" | "cancel" => {
                self.active_pointers.remove(&event.pointer_id);
            }
            _ => {
                self.active_pointers.insert(
                    event.pointer_id,
                    (event.surface_id.clone(), event.x, event.y),
                );
            }
        }
        self.metrics.inputs_emitted = self.metrics.inputs_emitted.saturating_add(1);
        self.write_control(MessageType::Input, 0, &event)
    }
    fn emit_pointer_cancels(&mut self) -> Result<(), SimError> {
        let pending: Vec<InputEvent> = self
            .active_pointers
            .iter()
            .map(|(pointer_id, (surface_id, x, y))| InputEvent {
                surface_id: surface_id.clone(),
                pointer_id: *pointer_id,
                phase: "cancel".into(),
                x: *x,
                y: *y,
            })
            .collect();
        self.active_pointers.clear();
        for event in pending {
            self.metrics.inputs_emitted = self.metrics.inputs_emitted.saturating_add(1);
            self.write_control(MessageType::Input, 0, &event)?;
        }
        Ok(())
    }
    pub fn capabilities(&self) -> Capabilities {
        let stride = u32::from(self.profile.width) * 2;
        let mut surfaces = vec![Surface {
            id: "main".into(),
            width: self.profile.width,
            height: self.profile.height,
            pixel_format: "RGB565".into(),
            stride,
            rotation: self.profile.rotation,
        }];
        if self.profile.overlay_width > 0 && self.profile.overlay_height > 0 {
            surfaces.push(Surface {
                id: "overlay".into(),
                width: self.profile.overlay_width,
                height: self.profile.overlay_height,
                pixel_format: "RGB565".into(),
                stride: u32::from(self.profile.overlay_width) * 2,
                rotation: 0,
            });
        }
        Capabilities {
            device_id: self.profile.device_id.clone(),
            firmware: self.firmware.clone(),
            surfaces,
            frame: true,
            tile: true,
            touch: self.profile.touch,
            ota: self.profile.ota,
            max_message: self.max_message as u32,
            max_chunk: self.max_chunk as u32,
            max_in_flight: 1,
            max_fps: if self.profile.color_fps { 30 } else { 0 },
            mirror: if self.profile.mirror {
                mdc_protocol::MirrorCapability::full_frame(
                    self.profile.width,
                    self.profile.height,
                    self.profile.mirror_fps,
                )
            } else {
                mdc_protocol::MirrorCapability::disabled()
            },
            local_display_fps: if self.profile.color_fps {
                self.profile.local_display_fps
            } else {
                0
            },
            mirror_fps: if self.profile.mirror {
                self.profile.mirror_fps
            } else {
                0
            },
            mirror_bandwidth: if self.profile.mirror {
                // Advertise 1 byte/px/row budget at the mirror fps; advisory only.
                u64::from(self.profile.width)
                    .saturating_mul(u64::from(self.profile.height))
                    .saturating_mul(2)
                    .saturating_mul(u64::from(self.profile.mirror_fps))
            } else {
                0
            },
            // N27: local renderer capability from the board profile (not inferred
            // from `mirror`).
            renderer: Some(mdc_protocol::RendererCapability {
                sprite_2d: self.profile.renderer_sprite,
                mesh2d: self.profile.renderer_mesh2d,
                mesh3d: self.profile.renderer_mesh3d,
                skeleton: self.profile.renderer_skeleton,
                max_texture: self.profile.renderer_max_texture,
                max_vertices: self.profile.renderer_max_vertices,
            }),
            // N41: supported configuration keys from the board profile.
            config: mdc_protocol::ConfigCapability {
                keys: self.profile.config_keys.clone(),
            },
        }
    }
    /// Advance virtual clock and process inbound packets.
    pub fn poll(&mut self) -> Result<(), SimError> {
        if self.faults.disconnect {
            self.faults.disconnect = false;
            self.emit_pointer_cancels()?;
            self.connected = false;
            self.link.close();
            return Err(SimError::Disconnected);
        }
        if !self.connected {
            self.link.close();
            return Err(SimError::Disconnected);
        }
        self.expire_ota();
        while let Some(bytes) = self
            .link
            .read()
            .map_err(|e| SimError::Transport(e.to_string()))?
        {
            self.rx.extend(bytes);
        }
        loop {
            match mdc_protocol::decode_stream(&self.rx, self.max_message) {
                mdc_protocol::StreamStep::Packet(packet, consumed) => {
                    self.rx.drain(..consumed);
                    self.enqueue_or_handle(packet)?;
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
                    return Err(SimError::Protocol(error.to_string()));
                }
            }
        }
        self.flush_delayed()?;
        Ok(())
    }
    fn expire_ota(&mut self) {
        let Some(deadline) = self.ota_deadline else {
            return;
        };
        if !self.ota_active || self.clock.tick < deadline {
            return;
        }
        self.pending_version = None;
        self.ota_active = false;
        self.ota_deadline = None;
        self.metrics.ota_rollbacks = self.metrics.ota_rollbacks.saturating_add(1);
    }
    pub fn advance(&mut self, ticks: u64) -> Result<(), SimError> {
        self.clock.advance(ticks);
        self.poll()
    }
    fn enqueue_or_handle(&mut self, packet: Packet) -> Result<(), SimError> {
        if self.faults.armed_drop {
            if self.faults.drop_skip > 0 {
                self.faults.drop_skip -= 1;
            } else {
                self.faults.armed_drop = false;
                self.assembler = None;
                return self.reply_error(packet.request_id, "E_DROPPED", "packet dropped");
            }
        }
        if self.faults.drop_next {
            self.faults.drop_next = false;
            self.assembler = None;
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
                    return self.reply_error(packet.request_id, "E_VERSION", "unsupported major");
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
            MessageType::Ping => {
                self.write_packet(MessageType::Pong, packet.request_id, Vec::new())
            }
            MessageType::Ota => self.handle_ota(packet),
            MessageType::MirrorCtrl => self.handle_mirror_ctrl(packet),
            MessageType::Package => self.handle_package(packet),
            MessageType::Config => self.handle_config(packet),
            MessageType::Input => {
                // Host→device input is unusual; reject unless touch is enabled.
                if !self.profile.touch {
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
    fn handle_ota(&mut self, packet: Packet) -> Result<(), SimError> {
        if !self.profile.ota {
            return self.reply_error(
                packet.request_id,
                "E_UNSUPPORTED",
                "ota capability disabled",
            );
        }
        let cmd: OtaCommand =
            decode_control(&packet.payload).map_err(|e| SimError::Protocol(e.to_string()))?;
        self.metrics.ota_commands = self.metrics.ota_commands.saturating_add(1);
        let rollback = |device: &mut Self| {
            device.pending_version = None;
            device.ota_active = false;
            device.ota_deadline = None;
            device.metrics.ota_rollbacks = device.metrics.ota_rollbacks.saturating_add(1);
        };
        match cmd.action.as_str() {
            "begin" => {
                if cmd.size.unwrap_or(0) > 1024 * 1024 {
                    return self.reply_error(packet.request_id, "E_OTA", "image exceeds 1 MiB");
                }
                self.pending_version = Some(cmd.version.unwrap_or_else(|| "pending".into()));
                self.ota_active = true;
                self.ota_deadline = Some(self.clock.tick.saturating_add(OTA_CHUNK_TIMEOUT_TICKS));
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
            "chunk" => {
                if cmd.sha256.as_deref() == Some("bad") {
                    rollback(self);
                    return self.reply_error(packet.request_id, "E_OTA", "bad image");
                }
                self.ota_deadline = Some(self.clock.tick.saturating_add(OTA_CHUNK_TIMEOUT_TICKS));
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
            "abort" | "timeout" => {
                rollback(self);
                if cmd.action == "timeout" {
                    self.reply_error(packet.request_id, "E_OTA", "chunk timeout")
                } else {
                    self.ack(packet.request_id, self.current_frame_id, true, false)
                }
            }
            "confirm" => {
                if !self.ota_active {
                    return self.reply_error(packet.request_id, "E_OTA", "no active ota");
                }
                if let Some(version) = self.pending_version.take() {
                    self.firmware = version;
                }
                self.ota_active = false;
                self.ota_deadline = None;
                self.ack(packet.request_id, self.current_frame_id, true, true)
            }
            other => self.reply_error(
                packet.request_id,
                "E_OTA",
                &format!("unknown action {other}"),
            ),
        }
    }
    /// Form B: host asked to start/stop mirroring. Capability-gated.
    fn handle_mirror_ctrl(&mut self, packet: Packet) -> Result<(), SimError> {
        if !self.profile.mirror {
            return self.reply_error(
                packet.request_id,
                "E_UNSUPPORTED",
                "mirror capability disabled",
            );
        }
        let cmd: MirrorCommand =
            decode_control(&packet.payload).map_err(|e| SimError::Protocol(e.to_string()))?;
        self.metrics.mirror_commands = self.metrics.mirror_commands.saturating_add(1);
        match cmd.action.as_str() {
            "start" => {
                self.mirror_active = true;
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
            "stop" => {
                self.mirror_active = false;
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
            other => self.reply_error(
                packet.request_id,
                "E_MIRROR",
                &format!("unknown action {other}"),
            ),
        }
    }
    /// N23: assemble a host-uploaded runtime package; on commit verify its
    /// integrity and that this device's renderer can run it.
    fn handle_package(&mut self, packet: Packet) -> Result<(), SimError> {
        let payload = decode_package_payload(&packet.payload)
            .map_err(|e| SimError::Protocol(e.to_string()))?;
        match payload {
            PackagePayload::Begin { .. } => {
                self.package_assembler = Some(mdc_model::PackageAssembler::begin());
                Ok(())
            }
            PackagePayload::Chunk { data, .. } => {
                let Some(assembler) = self.package_assembler.as_mut() else {
                    return self.reply_error(packet.request_id, "E_PACKAGE", "chunk before begin");
                };
                if let Err(error) = assembler.push_chunk(&data) {
                    return self.reply_error(packet.request_id, "E_PACKAGE", &error.to_string());
                }
                Ok(())
            }
            PackagePayload::Commit => {
                let Some(assembler) = self.package_assembler.take() else {
                    return self.reply_error(packet.request_id, "E_PACKAGE", "commit before begin");
                };
                let package = match assembler.finish() {
                    Ok(package) => package,
                    Err(error) => {
                        return self.reply_error(packet.request_id, "E_PACKAGE", &error.to_string())
                    }
                };
                let caps = self.capabilities();
                let renderer = mdc_model::RendererCapability::from_wire(caps.renderer.as_ref());
                if let Err(error) = package.accepted_by(&renderer) {
                    return self.reply_error(packet.request_id, "E_PACKAGE", &error.to_string());
                }
                self.package_ready = true;
                self.metrics.packages_received = self.metrics.packages_received.saturating_add(1);
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
        }
    }
    /// N23: true once a runtime package has been accepted and is render-ready.
    pub fn package_ready(&self) -> bool {
        self.package_ready
    }
    /// N41: read a managed configuration value (test / inspector helper).
    pub fn config_value(&self, key: &str) -> Option<&str> {
        self.config.get(key).map(String::as_str)
    }
    /// N41: apply a configuration get/set command and reply with ConfigReply.
    fn handle_config(&mut self, packet: Packet) -> Result<(), SimError> {
        let command: ConfigCommand =
            decode_control(&packet.payload).map_err(|e| SimError::Protocol(e.to_string()))?;
        self.metrics.config_commands = self.metrics.config_commands.saturating_add(1);
        if !self.profile.config_keys.iter().any(|k| k == &command.key) {
            return self.reply_config(&command.key, false, None, Some("E_UNSUPPORTED"));
        }
        match command.action.as_str() {
            "get" => {
                let value = self
                    .config
                    .get(&command.key)
                    .cloned()
                    .unwrap_or_else(|| "".to_string());
                let shown = if is_secret_key(&command.key) {
                    "***".to_string()
                } else {
                    value
                };
                self.reply_config(&command.key, true, Some(shown), None)
            }
            "set" => {
                let value = command.value.clone().unwrap_or_default();
                if let Err(error) = validate_config(&command.key, &value) {
                    return self.reply_config(&command.key, false, None, Some(error));
                }
                self.config.insert(command.key.clone(), value.clone());
                let shown = if is_secret_key(&command.key) {
                    "***".to_string()
                } else {
                    value
                };
                self.reply_config(&command.key, true, Some(shown), None)
            }
            _ => self.reply_config(&command.key, false, None, Some("E_ACTION")),
        }
    }
    fn reply_config(
        &mut self,
        key: &str,
        ok: bool,
        value: Option<String>,
        error: Option<&str>,
    ) -> Result<(), SimError> {
        let reply = ConfigReply {
            key: key.to_string(),
            ok,
            value,
            error: error.map(str::to_string),
        };
        self.write_control(MessageType::Config, 0, &reply)
    }
    /// Form B: true once a host MirrorCtrl start has been accepted.
    pub fn mirror_active(&self) -> bool {
        self.mirror_active
    }
    /// Form B: device pushes the current rendered `main` pixels back to the host.
    pub fn emit_mirror_frame(&mut self) -> Result<(), SimError> {
        if !self.profile.mirror {
            return Err(SimError::Protocol("mirror capability disabled".into()));
        }
        let payload = mdc_protocol::encode_frame_payload(
            "main",
            self.current_frame_id,
            self.profile.width,
            self.profile.height,
            &self.pixels,
        )
        .map_err(|e| SimError::Protocol(e.to_string()))?;
        self.metrics.mirror_frames = self.metrics.mirror_frames.saturating_add(1);
        self.write_packet(MessageType::MirrorFrame, 0, payload)
    }
    fn note_refresh(&mut self) {
        if self.profile.color_fps {
            return;
        }
        self.busy_until = self.clock.tick.saturating_add(100);
    }
    fn handle_frame(&mut self, packet: Packet) -> Result<(), SimError> {
        if self.ota_active {
            return self.reply_error(packet.request_id, "E_FROZEN", "render frozen during ota");
        }
        if !self.profile.color_fps && self.busy_until > self.clock.tick {
            return self.reply_error(packet.request_id, "E_BUSY", "panel is refreshing");
        }
        let decoded =
            decode_frame_payload(&packet.payload).map_err(|e| SimError::Protocol(e.to_string()))?;
        match decoded {
            FramePayload::Legacy {
                surface_id,
                frame_id,
                width,
                height,
                bytes,
            } => {
                if self
                    .store_surface_pixels(&surface_id, width, height, bytes)
                    .is_err()
                {
                    return self.reply_error(packet.request_id, "E_DIM", "geometry mismatch");
                }
                self.current_frame_id = frame_id;
                self.assembler = None;
                self.note_refresh();
                self.metrics.frames_received = self.metrics.frames_received.saturating_add(1);
                self.ack(packet.request_id, frame_id, true, true)?;
                self.flush_scripted_input()
            }
            FramePayload::Begin {
                surface_id,
                frame_id,
                width,
                height,
                total_bytes,
            } => {
                if self.surface_size(&surface_id) != Some((width, height)) {
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
                    return self.reply_error(packet.request_id, "E_CHUNK", &format!("{nack:?}"));
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
                        if self.store_surface_pixels(&_sid, _w, _h, bytes).is_err() {
                            return self.reply_error(
                                packet.request_id,
                                "E_DIM",
                                "geometry mismatch",
                            );
                        }
                        self.current_frame_id = frame_id;
                        self.note_refresh();
                        self.metrics.frames_received =
                            self.metrics.frames_received.saturating_add(1);
                        self.ack(packet.request_id, frame_id, true, true)?;
                        self.flush_scripted_input()
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
        if self.ota_active {
            return self.reply_error(packet.request_id, "E_FROZEN", "render frozen during ota");
        }
        let tile =
            decode_tile_payload(&packet.payload).map_err(|e| SimError::Protocol(e.to_string()))?;
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
        let (buffer_width, overlay) = if core_tile.surface_id == "overlay" {
            if self.overlay_pixels.is_empty() {
                return self.reply_error(packet.request_id, "E_DIM", "unknown surface");
            }
            (self.profile.overlay_width, true)
        } else if core_tile.surface_id == "main" {
            (self.profile.width, false)
        } else {
            return self.reply_error(packet.request_id, "E_DIM", "unknown surface");
        };
        let buffer_height = if overlay {
            self.profile.overlay_height
        } else {
            self.profile.height
        };
        if u32::from(core_tile.x) + u32::from(core_tile.width) > u32::from(buffer_width)
            || u32::from(core_tile.y) + u32::from(core_tile.height) > u32::from(buffer_height)
        {
            return self.reply_error(packet.request_id, "E_DIM", "tile outside surface");
        }
        let stride = usize::from(buffer_width) * 2;
        let row_bytes = usize::from(core_tile.width) * 2;
        let pixels = if overlay {
            &mut self.overlay_pixels
        } else {
            &mut self.pixels
        };
        for row in 0..usize::from(core_tile.height) {
            let dst_y = usize::from(core_tile.y) + row;
            let dst_off = dst_y * stride + usize::from(core_tile.x) * 2;
            let src_off = row * row_bytes;
            pixels[dst_off..dst_off + row_bytes]
                .copy_from_slice(&core_tile.bytes[src_off..src_off + row_bytes]);
        }
        self.metrics.tiles_received = self.metrics.tiles_received.saturating_add(1);
        self.ack(packet.request_id, self.current_frame_id, true, true)
    }
    fn ack(
        &mut self,
        request_id: u32,
        frame_id: u64,
        received: bool,
        displayed: bool,
    ) -> Result<(), SimError> {
        self.metrics.acks_sent = self.metrics.acks_sent.saturating_add(1);
        let ack = Ack {
            received,
            displayed,
            frame_id,
            error: None,
        };
        self.write_control(MessageType::Ack, request_id, &ack)
    }
    fn reply_error(&mut self, request_id: u32, code: &str, message: &str) -> Result<(), SimError> {
        self.metrics.errors_sent = self.metrics.errors_sent.saturating_add(1);
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
        let payload = encode_control(value).map_err(|e| SimError::Protocol(e.to_string()))?;
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
    use mdc_protocol::OtaCommand;
    use mdc_transport::MemoryLink;

    #[test]
    fn config_set_get_validate_and_redact() {
        let (host, mut device) = FakeDevice::pair(BoardProfile::default());
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        session.set_config("brightness", "200").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(session.config("brightness"), Some("200"));
        assert_eq!(device.config_value("brightness"), Some("200"));

        // An invalid value is rejected by the device and keeps the old value.
        session.set_config("brightness", "999").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(session.config("brightness"), Some("200"));

        // A secret is never echoed back.
        session.get_config("wifi_password").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(session.config("wifi_password"), Some("***"));
        assert_eq!(device.config_value("wifi_password"), Some("mdc-sim-secret"));
        assert!(device.metrics.config_commands >= 3);
    }

    #[test]
    fn renderer_capability_follows_the_board_profile() {
        let (_, esp) = FakeDevice::with_default_profile();
        let renderer = esp.capabilities().renderer.unwrap();
        assert!(renderer.sprite_2d && !renderer.mesh2d && !renderer.skeleton);
        assert_eq!(renderer.max_texture, 512);

        let (_, gpu) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
        let renderer = gpu.capabilities().renderer.unwrap();
        assert!(renderer.mesh2d && renderer.mesh3d && renderer.skeleton);
        assert_eq!(renderer.max_texture, 2048);
        assert_eq!(renderer.max_vertices, 200_000);
    }

    #[test]
    fn accepts_a_runtime_package_and_reports_ready() {
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

        let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"count":4,"type":"VEC3"}]}"#;
        let model = mdc_model::GltfImporter
            .import(gltf, &mdc_model::ImportLimits::default())
            .unwrap();
        let package =
            mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::sprite_only())
                .unwrap();
        session
            .install_package(&package.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        assert!(device.package_ready());
        assert_eq!(device.metrics.packages_received, 1);
    }

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
    fn linux_virt_profile_matches_board_yaml() {
        let p = BoardProfile::from_board_id("linux-virt");
        assert_eq!(p.device_id, "sim-linux-virt");
        assert_eq!(p.width, 800);
        assert_eq!(p.height, 480);
        assert!(!p.touch);
        assert!(!p.ota);
        assert!(p.color_fps);
        let pi = BoardProfile::from_board_id("pi4-hdmi");
        assert_eq!((pi.width, pi.height), (640, 480));
        assert!(pi.color_fps);
        let eink = BoardProfile::from_board_id("waveshare-eink");
        assert!(!eink.color_fps, "e-ink must not claim color FPS");
    }

    #[test]
    fn mirror_capability_follows_profile_and_gates_control() {
        // ESP32-S3 class: Form A only, no mirror capability.
        let (_, esp) = FakeDevice::pair(BoardProfile::default());
        assert!(!esp.capabilities().mirror.supports());
        assert_eq!(esp.capabilities().mirror_fps, 0);

        // A GPU board advertises mirror; local display and mirror FPS differ.
        let profile = BoardProfile {
            width: 4,
            height: 2,
            mirror: true,
            mirror_fps: 5,
            local_display_fps: 30,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let caps = device.capabilities();
        assert!(caps.mirror.supports());
        assert_eq!(caps.local_display_fps, 30);
        assert_eq!(caps.mirror_fps, 5);
        assert_ne!(caps.local_display_fps, caps.mirror_fps);
        assert!(caps.mirror_bandwidth > 0);

        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(session.supports_mirror());
        session.start_mirror("main", "full-frame", 30).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.mirror_active());

        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 4,
                height: 2,
                bytes: vec![7; 16],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        // Device mirrors its rendered frame back to the host.
        device.emit_mirror_frame().unwrap();
        session.poll().unwrap();
        let mirrored = session.take_mirror_frame().unwrap();
        assert_eq!(mirrored.frame_id, 1);
        assert_eq!((mirrored.width, mirrored.height), (4, 2));
        assert_eq!(mirrored.bytes, vec![7; 16]);

        session.stop_mirror("main").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(!device.mirror_active());
    }

    #[test]
    fn device_without_mirror_rejects_mirror_control() {
        let (host, mut esp) = FakeDevice::pair(BoardProfile::default());
        let mut session = Session::new(host);
        session.connect().unwrap();
        esp.poll().unwrap();
        session.poll().unwrap();
        assert!(!session.supports_mirror());
        assert_eq!(
            session.start_mirror("main", "full-frame", 30),
            Err(mdc_core::CoreError::Unsupported)
        );
        assert_eq!(
            session.stop_mirror("main"),
            Err(mdc_core::CoreError::Unsupported)
        );
        assert!(esp.emit_mirror_frame().is_err());
        assert!(!esp.mirror_active());
        // Nothing but the handshake reply ever reached the device.
        assert_eq!(esp.metrics.mirror_commands, 0);
    }

    #[test]
    fn eink_busy_keeps_the_previous_frame() {
        let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("waveshare-eink"));
        assert_eq!(device.capabilities().max_fps, 0);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let first = vec![0x11; 800 * 480 * 2];
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 800,
                height: 480,
                bytes: first,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels()[0], 0x11);
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 800,
                height: 480,
                bytes: vec![0x22; 800 * 480 * 2],
            })
            .unwrap();
        device.poll().unwrap();
        assert_eq!(device.pixels()[0], 0x11);
        let err = session.poll().unwrap_err();
        assert!(err.to_string().contains("E_BUSY"), "{err}");
        device.advance(100).unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 800,
                height: 480,
                bytes: vec![0x22; 800 * 480 * 2],
            })
            .unwrap();
        device.poll().unwrap();
        assert_eq!(device.pixels()[0], 0x22);
    }

    #[test]
    fn default_profile_is_480x320() {
        let (_, device) = FakeDevice::with_default_profile();
        assert_eq!((device.profile.width, device.profile.height), (480, 320));
        assert_eq!(device.pixels().len(), 480 * 320 * 2);
    }

    #[test]
    fn emit_input_reaches_session_manager() {
        use mdc_core::{DeviceManager, ManagerEvent};
        use std::sync::{Arc, Mutex};
        let profile = BoardProfile {
            width: 2,
            height: 2,
            touch: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 0,
                phase: "down".into(),
                x: 1,
                y: 1,
            })
            .unwrap();
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        assert!(events.iter().any(|e| matches!(
            e,
            ManagerEvent::Input {
                event,
                ..
            } if event.phase == "down"
        )));
    }

    #[test]
    fn scripted_input_follows_displayed_frame() {
        use mdc_core::{DeviceManager, Frame, ManagerEvent, Session};
        use std::sync::{Arc, Mutex};
        let profile = BoardProfile {
            width: 2,
            height: 2,
            touch: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        device.emit_input_after_next_frame(InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: "down".into(),
            x: 1,
            y: 1,
        });
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            ManagerEvent::Input { event, .. } if event.x == 1 && event.y == 1 && event.phase == "down"
        )));
    }

    #[test]
    fn ota_gated_by_capability() {
        use mdc_protocol::OtaCommand;
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ota: false,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(
            session.send_ota(OtaCommand {
                action: "begin".into(),
                version: None,
                size: None,
                sha256: None,
            }),
            Err(mdc_core::CoreError::Unsupported)
        );

        let profile = BoardProfile {
            width: 2,
            height: 2,
            ota: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_ota(OtaCommand {
                action: "begin".into(),
                version: Some("1.0.0".into()),
                size: Some(10),
                sha256: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.metrics.ota_commands, 1);
    }

    fn ota(
        session: &mut Session<MemoryLink>,
        device: &mut FakeDevice,
        action: &str,
        version: Option<&str>,
        size: Option<u64>,
        sha: Option<&str>,
        expect_error: bool,
    ) {
        session
            .send_ota(OtaCommand {
                action: action.into(),
                version: version.map(str::to_string),
                size,
                sha256: sha.map(str::to_string),
            })
            .unwrap();
        device.poll().unwrap();
        let polled = session.poll();
        if expect_error {
            assert!(polled.is_err());
        } else {
            polled.unwrap();
        }
    }

    #[test]
    fn ota_failure_keeps_installed_firmware() {
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ota: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.firmware(), "1.0.0");

        ota(
            &mut session,
            &mut device,
            "begin",
            Some("9.9.9"),
            Some(2 * 1024 * 1024),
            None,
            true,
        );
        assert_eq!(device.firmware(), "1.0.0");
        assert!(device.pending_version().is_none());

        ota(
            &mut session,
            &mut device,
            "begin",
            Some("2.0.0"),
            Some(32),
            None,
            false,
        );
        assert_eq!(device.firmware(), "1.0.0");
        ota(
            &mut session,
            &mut device,
            "chunk",
            None,
            None,
            Some("bad"),
            true,
        );
        assert_eq!(device.firmware(), "1.0.0");
        assert!(device.metrics.ota_rollbacks >= 1);

        ota(
            &mut session,
            &mut device,
            "begin",
            Some("2.0.0"),
            Some(32),
            None,
            false,
        );
        ota(&mut session, &mut device, "abort", None, None, None, false);
        assert_eq!(device.firmware(), "1.0.0");

        ota(
            &mut session,
            &mut device,
            "begin",
            Some("2.0.0"),
            Some(32),
            None,
            false,
        );
        ota(&mut session, &mut device, "timeout", None, None, None, true);
        assert_eq!(device.firmware(), "1.0.0");

        ota(
            &mut session,
            &mut device,
            "begin",
            Some("2.0.0"),
            Some(32),
            None,
            false,
        );
        ota(
            &mut session,
            &mut device,
            "confirm",
            None,
            None,
            None,
            false,
        );
        assert_eq!(device.firmware(), "2.0.0");
    }

    #[test]
    fn ota_begin_freezes_pixels_until_abort() {
        use mdc_core::{Frame, Session};
        use mdc_protocol::OtaCommand;
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ota: true,
            touch: false,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let painted = vec![0x11u8; 8];
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: painted.clone(),
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), painted.as_slice());
        session
            .send_ota(OtaCommand {
                action: "begin".into(),
                version: Some("2.0.0".into()),
                size: Some(32),
                sha256: Some("0".repeat(64)),
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x22; 8],
            })
            .unwrap();
        device.poll().unwrap();
        assert!(session.poll().is_err());
        assert_eq!(device.pixels(), painted.as_slice());
        session
            .send_ota(OtaCommand {
                action: "abort".into(),
                version: None,
                size: None,
                sha256: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x33; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x33; 8]);
    }

    #[test]
    fn dual_surface_accepts_overlay_and_rejects_unknown() {
        use mdc_core::{Frame, Session};
        let profile = BoardProfile::from_board_id("dual-sim");
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let surfaces = &session.device.as_ref().unwrap().capabilities.surfaces;
        assert_eq!(surfaces.len(), 2);
        assert_eq!(surfaces[1].id, "overlay");
        let main_before = device.pixels().to_vec();
        session
            .send_frame(Frame {
                surface_id: "overlay".into(),
                width: 4,
                height: 2,
                bytes: vec![0xAB; 16],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.overlay_pixels(), &[0xAB; 16]);
        assert_eq!(device.pixels(), main_before.as_slice());
        let missing = session.send_frame(Frame {
            surface_id: "nope".into(),
            width: 4,
            height: 2,
            bytes: vec![0; 16],
        });
        assert!(missing.is_err());
    }

    #[test]
    fn overlay_tile_updates_overlay_and_rejects_out_of_bounds() {
        use mdc_core::{Frame, Session, Tile};
        let profile = BoardProfile::from_board_id("dual-sim");
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let main_before = device.pixels().to_vec();
        session
            .send_frame(Frame {
                surface_id: "overlay".into(),
                width: 4,
                height: 2,
                bytes: vec![0xAB; 16],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_tile(Tile {
                surface_id: "overlay".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0x11, 0x22],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(&device.overlay_pixels()[..2], &[0x11, 0x22]);
        assert_eq!(&device.overlay_pixels()[2..], &[0xAB; 14]);
        assert_eq!(device.pixels(), main_before.as_slice());
        let overlay_before = device.overlay_pixels().to_vec();
        let rejected = session.send_tile(Tile {
            surface_id: "overlay".into(),
            base_frame_id: 1,
            x: 4,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0, 0],
        });
        assert_eq!(rejected, Err(mdc_core::CoreError::TileBounds));
        let payload = mdc_protocol::encode_tile_payload("overlay", 1, 4, 0, 1, 1, &[0, 0]).unwrap();
        device
            .handle_tile(mdc_protocol::Packet {
                version: mdc_protocol::VERSION,
                kind: mdc_protocol::MessageType::Tile,
                flags: 0,
                request_id: 99,
                payload,
            })
            .unwrap();
        let err = session.poll().unwrap_err();
        assert!(err.to_string().contains("E_DIM"), "{err}");
        assert_eq!(device.overlay_pixels(), overlay_before.as_slice());
        assert_eq!(device.pixels(), main_before.as_slice());
    }

    #[test]
    fn corners_and_two_pointers_cancel_only_the_ones_still_down() {
        use mdc_core::{DeviceManager, ManagerEvent, Session};
        use std::sync::{Arc, Mutex};
        let profile = BoardProfile {
            width: 4,
            height: 2,
            touch: true,
            rotation: 90,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let corners = [(0u16, 0u16), (3, 0), (0, 1), (3, 1)];
        for (x, y) in corners {
            let (tx, ty) = mdc_core::transform_touch(4, 2, 90, x, y).unwrap();
            device
                .emit_input(InputEvent {
                    surface_id: "main".into(),
                    pointer_id: 1,
                    phase: "down".into(),
                    x,
                    y,
                })
                .unwrap();
            session.poll().unwrap();
            let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    ManagerEvent::Input { event, .. } if event.x == tx && event.y == ty
                )),
                "corner ({x},{y}) -> ({tx},{ty})"
            );
            device
                .emit_input(InputEvent {
                    surface_id: "main".into(),
                    pointer_id: 1,
                    phase: "up".into(),
                    x,
                    y,
                })
                .unwrap();
            session.poll().unwrap();
            let _ = manager.lock().unwrap().drain_events().count();
        }
        assert!(mdc_core::transform_touch(4, 2, 90, 4, 0).is_err());
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "down".into(),
                x: 0,
                y: 0,
            })
            .unwrap();
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 2,
                phase: "down".into(),
                x: 1,
                y: 0,
            })
            .unwrap();
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 2,
                phase: "up".into(),
                x: 1,
                y: 0,
            })
            .unwrap();
        device.faults.request_disconnect();
        assert!(device.poll().is_err());
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        let cancels: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ManagerEvent::Input { event, .. } if event.phase == "cancel" => {
                    Some(event.pointer_id)
                }
                _ => None,
            })
            .collect();
        assert_eq!(cancels, vec![1]);
    }

    #[test]
    fn rotation_transforms_touch_and_drops_out_of_bounds() {
        use mdc_core::{DeviceManager, ManagerEvent, Session};
        use std::sync::{Arc, Mutex};
        let profile = BoardProfile {
            width: 4,
            height: 2,
            touch: true,
            rotation: 90,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "down".into(),
                x: 0,
                y: 0,
            })
            .unwrap();
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            ManagerEvent::Input { event, .. } if event.x == 1 && event.y == 0
        )));
        assert!(device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "down".into(),
                x: 4,
                y: 0,
            })
            .is_err());
    }

    #[test]
    fn drop_next_keeps_previous_pixels() {
        use mdc_core::{Frame, Session};
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x11; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device.faults.drop_next();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x22; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x11; 8]);
        assert_eq!(session.pending_request_count(), 1);
    }

    #[test]
    fn linux_virt_is_800x480_and_rejects_early_tile() {
        use mdc_core::{Frame, Session, Tile};
        let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let surface = &session.device.as_ref().unwrap().capabilities.surfaces[0];
        assert_eq!((surface.width, surface.height), (800, 480));
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
        device.poll().unwrap();
        assert!(session.poll().is_err());
        let bytes = vec![0x44u8; 800 * 480 * 2];
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 800,
                height: 480,
                bytes,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0x44, 0x44],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(session.pending_request_count(), 0);
    }

    #[test]
    fn pending_frame_is_painted_only_after_the_inflight_ack() {
        use mdc_core::Frame;
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let send = |session: &mut Session<MemoryLink>, value: u8| {
            session
                .send_frame(Frame {
                    surface_id: "main".into(),
                    width: 2,
                    height: 2,
                    bytes: vec![value; 8],
                })
                .unwrap()
        };
        assert_ne!(send(&mut session, 0x11), 0);
        assert_eq!(send(&mut session, 0x22), 0);
        assert_eq!(send(&mut session, 0x33), 0);
        device.poll().unwrap();
        assert_eq!(device.pixels(), &[0x11; 8]);
        session.poll().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x33; 8]);
    }

    #[test]
    fn saturated_queue_rejects_a_tile() {
        use mdc_core::{CoreError, Tile};
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![1; 8],
            })
            .unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![2; 8],
            })
            .unwrap();
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
            Err(CoreError::QueueFull)
        );
    }

    #[test]
    fn disconnect_cancels_only_the_pointer_still_down() {
        use mdc_core::{DeviceManager, ManagerEvent};
        use std::sync::{Arc, Mutex};
        let profile = BoardProfile {
            width: 4,
            height: 2,
            touch: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let down = |pointer_id, x, y| InputEvent {
            surface_id: "main".into(),
            pointer_id,
            phase: "down".into(),
            x,
            y,
        };
        device.emit_input(down(1, 0, 0)).unwrap();
        device.emit_input(down(2, 1, 0)).unwrap();
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "up".into(),
                x: 0,
                y: 0,
            })
            .unwrap();
        session.poll().unwrap();
        device.faults.request_disconnect();
        assert_eq!(device.poll(), Err(SimError::Disconnected));
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        let cancels: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ManagerEvent::Input { event, .. } if event.phase == "cancel" => {
                    Some((event.pointer_id, event.x, event.y))
                }
                _ => None,
            })
            .collect();
        assert_eq!(cancels, vec![(2, 1, 0)]);
    }

    #[test]
    fn ota_virtual_timeout_unfreezes_without_changing_firmware() {
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ota: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_ota(OtaCommand {
                action: "begin".into(),
                version: Some("2.0.0".into()),
                size: Some(32),
                sha256: Some("0".repeat(64)),
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device.clock.advance(5_000);
        device.poll().unwrap();
        assert_eq!(device.firmware(), "1.0.0");
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x55; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x55; 8]);
    }

    #[test]
    fn dropped_commit_keeps_pixels_and_the_next_frame_displays() {
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.max_chunk = 8;
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x11; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x11; 8]);
        // begin + chunk 放行，丢掉 commit。
        device.faults.drop_one_after(2);
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x22; 8],
            })
            .unwrap();
        device.poll().unwrap();
        let _ = session.poll();
        assert_eq!(device.pixels(), &[0x11; 8]);
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x33; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x33; 8]);
    }
}
