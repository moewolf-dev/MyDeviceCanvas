use mdc_protocol::{
    encode_control, encode_frame_begin, encode_frame_chunk, encode_frame_commit,
    encode_frame_payload, encode_tile_payload, Ack, Capabilities, ErrorPayload, Hello,
    MessageType, Packet, Surface, VERSION,
};
use mdc_transport::Transport;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use thiserror::Error;

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    #[error("unsupported pixel format: {0}")]
    Format(String),
    #[error("frame dimensions do not match surface")]
    Dimensions,
    #[error("frame is too large")]
    TooLarge,
    #[error("tile is outside surface")]
    TileBounds,
    #[error("queue is full")]
    QueueFull,
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
    #[error("connect timeout")]
    ConnectTimeout,
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

#[derive(Debug, Clone)]
pub struct Frame {
    pub surface_id: String,
    pub width: u16,
    pub height: u16,
    pub bytes: Vec<u8>,
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
}
pub fn validate_tile_base(tile: &Tile, current_frame_id: u64) -> Result<(), TransactionNack> {
    if tile.base_frame_id == current_frame_id {
        Ok(())
    } else {
        Err(TransactionNack::BaseFrameMismatch)
    }
}

pub struct SurfaceQueue {
    pub info: Surface,
    pending: Option<(u64, Frame)>,
    in_flight: Option<u64>,
    next_id: u64,
}
impl SurfaceQueue {
    pub fn new(info: Surface) -> Self {
        Self {
            info,
            pending: None,
            in_flight: None,
            next_id: 1,
        }
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
        if self.info.pixel_format != "RGB565" {
            return Err(CoreError::Format(self.info.pixel_format.clone()));
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
            self.in_flight = Some(id)
        } else {
            self.pending = Some((id, f))
        }
        Ok(id)
    }
    pub fn ack(&mut self, id: u64) {
        if self.in_flight == Some(id) {
            self.in_flight = self.pending.take().map(|(pending_id, _)| pending_id)
        }
    }
}

pub struct Device {
    pub capabilities: Capabilities,
    pub state: ConnectionState,
    pub surfaces: Vec<SurfaceQueue>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerEvent {
    Added(String),
    Removed(String),
    Connected(String),
    Disconnected(String),
    Error { device_id: String, message: String },
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
    pub fn connect_session(&mut self, device_id: &str, endpoint: impl Into<String>) {
        self.active_endpoints
            .insert(device_id.to_string(), endpoint.into());
        if let Some(device) = self.devices.get_mut(device_id) {
            device.state = ConnectionState::Ready;
        }
        self.push_event(ManagerEvent::Connected(device_id.into()));
    }
    pub fn active_endpoint(&self, device_id: &str) -> Option<&str> {
        self.active_endpoints.get(device_id).map(|s| s.as_str())
    }
    fn push_event(&mut self, event: ManagerEvent) {
        if self.events.len() >= 128 {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
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
    /// request_id -> (surface_id, frame_id) for in-flight Frame/Tile acks.
    pending_frames: BTreeMap<u32, (String, u64)>,
    pub reconnect_policy: ReconnectPolicy,
    reconnect_attempt: u32,
    reconnect_enabled: bool,
    pub config: SessionConfig,
    virtual_ms: u64,
    connect_started_ms: u64,
    last_send_ms: u64,
    last_heartbeat_ms: u64,
    reconnect_ready_ms: Option<u64>,
    manager: Option<Arc<Mutex<DeviceManager>>>,
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
            pending_frames: BTreeMap::new(),
            reconnect_policy: ReconnectPolicy::default(),
            reconnect_attempt: 0,
            reconnect_enabled: config.reconnect,
            config,
            virtual_ms: 0,
            connect_started_ms: 0,
            last_send_ms: 0,
            last_heartbeat_ms: 0,
            reconnect_ready_ms: None,
            manager: None,
        }
    }
    pub fn set_manager(&mut self, manager: Arc<Mutex<DeviceManager>>) {
        self.manager = Some(manager);
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
        })
        .map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.write_packet(MessageType::Hello, hello)?;
        Ok(())
    }
    pub fn poll(&mut self) -> Result<ConnectionState, CoreError> {
        while let Some(bytes) = self
            .transport
            .read()
            .map_err(|e| CoreError::Transport(e.to_string()))?
        {
            self.rx.extend(bytes);
        }
        loop {
            match Packet::decode(&self.rx, self.max_message) {
                Ok((packet, consumed)) => {
                    self.rx.drain(..consumed);
                    self.handle_packet(packet)?;
                }
                Err(mdc_protocol::CodecError::Truncated) => break,
                Err(e) => return Err(CoreError::Protocol(e.to_string())),
            }
        }
        Ok(self.state)
    }
    /// Advance virtual clock; drives heartbeat, ack timeout, and reconnect edges.
    pub fn tick(&mut self, virtual_ms: u64) -> Result<ConnectionState, CoreError> {
        self.virtual_ms = virtual_ms;
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
                    && virtual_ms.saturating_sub(self.last_heartbeat_ms)
                        >= self.config.heartbeat_ms
                {
                    self.write_packet(MessageType::Ping, Vec::new())?;
                    self.last_heartbeat_ms = virtual_ms;
                }
            }
            ConnectionState::Reconnecting => {
                if let Some(ready_at) = self.reconnect_ready_ms {
                    if virtual_ms >= ready_at {
                        self.state = ConnectionState::Connecting;
                        self.reconnect_ready_ms = None;
                    }
                }
            }
            _ => {}
        }
        Ok(self.state)
    }
    pub fn disconnect(&mut self) {
        self.reconnect_enabled = false;
        self.state = ConnectionState::Closing;
        self.transport.close();
        self.state = ConnectionState::Disconnected;
        self.device = None;
        self.rx.clear();
        self.pending_requests.clear();
        self.pending_frames.clear();
    }
    pub fn enable_reconnect(&mut self, enabled: bool) {
        self.reconnect_enabled = enabled;
        self.config.reconnect = enabled;
    }
    pub fn mark_transport_failure(&mut self) -> Option<u64> {
        if !self.reconnect_enabled {
            self.state = ConnectionState::Disconnected;
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
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }
    pub fn send_frame(&mut self, frame: Frame) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        {
            let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
            device.validate_frame(&frame)?;
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
        let frame_id = {
            let device = self.device.as_mut().ok_or(CoreError::NotReady)?;
            let surface = device
                .surfaces
                .iter_mut()
                .find(|s| s.info.id == surface_id)
                .ok_or(CoreError::Dimensions)?;
            surface.enqueue_frame(frame)?
        };
        let legacy = encode_frame_payload(&surface_id, frame_id, width, height, &pixels)
            .map_err(|e| CoreError::Protocol(e.to_string()))?;

        let request_id = self.alloc_request_id()?;
        if legacy.len() <= max_chunk {
            self.write_packet_with_id(MessageType::Frame, legacy, request_id, true)?;
        } else {
            let begin =
                encode_frame_begin(&surface_id, frame_id, width, height, pixels.len() as u32)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
            self.write_packet_with_id(MessageType::Frame, begin, request_id, true)?;
            let mut offset = 0u32;
            while (offset as usize) < pixels.len() {
                let end = ((offset as usize) + max_chunk).min(pixels.len());
                let chunk = encode_frame_chunk(frame_id, offset, &pixels[offset as usize..end]);
                self.write_packet_with_id(MessageType::Frame, chunk, request_id, false)?;
                offset = end as u32;
            }
            let commit = encode_frame_commit(frame_id);
            self.write_packet_with_id(MessageType::Frame, commit, request_id, false)?;
        }
        self.pending_frames
            .insert(request_id, (surface_id, frame_id));
        Ok(request_id)
    }
    pub fn send_tile(&mut self, tile: Tile) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
        if !device.capabilities.tile {
            return Err(CoreError::Unsupported);
        }
        device.send_tile(&tile)?;
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
        self.pending_frames
            .insert(request_id, (tile.surface_id, tile.base_frame_id));
        Ok(request_id)
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
        let id = self.alloc_request_id()?;
        self.write_packet_with_id(kind, payload, id, true)?;
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
        self.transport
            .write(&bytes)
            .map_err(|e| CoreError::Transport(e.to_string()))?;
        if track {
            self.pending_requests.insert(id);
            self.last_send_ms = self.virtual_ms;
        }
        Ok(())
    }
    fn handle_packet(&mut self, packet: Packet) -> Result<(), CoreError> {
        match packet.kind {
            MessageType::Capabilities => {
                self.pending_requests.remove(&packet.request_id);
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
            MessageType::Pong => Ok(()),
            MessageType::Ack => {
                let ack: Ack = mdc_protocol::decode_control(&packet.payload)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                if self.pending_requests.contains(&packet.request_id)
                    && (ack.received || ack.displayed)
                {
                    self.pending_requests.remove(&packet.request_id);
                    if let Some((surface_id, frame_id)) =
                        self.pending_frames.remove(&packet.request_id)
                    {
                        if let Some(device) = self.device.as_mut() {
                            if let Some(surface) =
                                device.surfaces.iter_mut().find(|s| s.info.id == surface_id)
                            {
                                surface.ack(frame_id);
                            }
                        }
                    } else if let Some(device) = self.device.as_mut() {
                        for surface in &mut device.surfaces {
                            surface.ack(ack.frame_id);
                        }
                    }
                }
                Ok(())
            }
            MessageType::Error => {
                let _err: Result<ErrorPayload, _> =
                    mdc_protocol::decode_control(&packet.payload);
                Err(CoreError::Protocol("device returned error".into()))
            }
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
        if surface.pixel_format != "RGB565" {
            return Err(CoreError::Format(surface.pixel_format.clone()));
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdc_protocol::{MessageType, Packet};
    use mdc_transport::MemoryLink;
    fn s() -> Surface {
        Surface {
            id: "main".into(),
            width: 2,
            height: 2,
            pixel_format: "RGB565".into(),
            stride: 4,
            rotation: 0,
        }
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
        assert_eq!(policy.delay_ms(3, 20), Some(4_010));
        assert_eq!(policy.delay_ms(4, 0), None);
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
        manager.connect_session("dev-a", "memory:1");
        assert_eq!(manager.active_endpoint("dev-a"), Some("memory:1"));
        manager.connect_session("dev-a", "tcp:127.0.0.1:9");
        assert_eq!(manager.active_endpoint("dev-a"), Some("tcp:127.0.0.1:9"));
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
            session
                .device
                .as_ref()
                .unwrap()
                .surfaces[0]
                .in_flight(),
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
                    })
                    .unwrap(),
                }
                .encode(MAX_FRAME_BYTES)
                .unwrap(),
            )
            .unwrap();
        session.poll().unwrap();
        assert_eq!(session.pending_request_count(), 0);
        assert!(session
            .device
            .as_ref()
            .unwrap()
            .surfaces[0]
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
        assert_eq!(session.tick(30 + 50 + 1_000).unwrap(), ConnectionState::Connecting);
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
            }],
            frame: true,
            tile: true,
            touch: false,
            ota: false,
            max_message: 4096,
            max_chunk: 8,
            max_in_flight: 1,
            max_fps: 30,
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
}
