//! `mdc-mirror` — Form B Host Shadow Framebuffer (N04).
//!
//! Consumes device -> host `MIRRORFRAME` packets, converts RGB565 to BGRA8888,
//! keeps a bounded front/back shadow buffer, and publishes complete frames to a
//! [`MirrorSink`] (an OBS adapter or a preview implements it later).
//!
//! This crate depends only on `mdc-protocol` + `mdc-transport`; it never links
//! OBS, Electron or Tauri, and it does not render. See
//! `docs/render-topology-decision.md` §6–§7 for the memory and OBS contract.
use mdc_protocol::{decode_frame_payload, FramePayload, MessageType, HEADER_LEN};
use mdc_transport::Transport;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;

/// Hard cap on front+back shadow buffers (N01 §6). 480×480 BGRA double buffer
/// is ~1.8 MiB; the cap leaves room for larger surfaces without unbounded use.
pub const MAX_SHADOW_BYTES: usize = 8 * 1024 * 1024;
/// Concurrent readers allowed on one slot before `acquire` refuses.
pub const MAX_LEASES: u32 = 4;
/// Largest mirror frame the staging path will assemble (one host framebuffer).
pub const MAX_MIRROR_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MirrorError {
    #[error("shadow geometry exceeds the {MAX_SHADOW_BYTES}-byte budget")]
    Budget,
    #[error("unsupported mirror mode: {0}")]
    Mode(String),
    #[error("mirror capability is not advertised")]
    Unsupported,
    #[error("frame geometry/size mismatch")]
    Geometry,
    #[error("shadow buffer is busy")]
    Busy,
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("transport: {0}")]
    Transport(String),
}

/// A complete mirrored frame published to a consumer.
pub struct ShadowFrame<'a> {
    pub width: u16,
    pub height: u16,
    pub generation: u64,
    pub frame_id: u64,
    /// BGRA8888, tightly packed. Valid only for the duration of the call.
    pub pixels: &'a [u8],
}

/// A borrowed view of a leased slot.
pub struct ShadowFrameView<'a> {
    pub width: u16,
    pub height: u16,
    pub generation: u64,
    pub pixels: &'a [u8],
}

/// OBS / preview consumer. Called with a borrowed frame; the library reuses the
/// buffer only after every lease is released.
pub trait MirrorSink {
    fn on_frame(&mut self, frame: ShadowFrame<'_>);
}

impl<F> MirrorSink for F
where
    F: FnMut(ShadowFrame<'_>),
{
    fn on_frame(&mut self, frame: ShadowFrame<'_>) {
        self(frame)
    }
}

/// Convert tightly packed RGB565 little-endian into BGRA8888.
/// `dst` length must be `src.len() / 2 * 4`.
pub fn rgb565_to_bgra(src: &[u8], dst: &mut [u8]) {
    for (i, chunk) in src.chunks_exact(2).enumerate() {
        let v = u16::from_le_bytes([chunk[0], chunk[1]]);
        let r = (v >> 11) & 0x1f;
        let g = (v >> 5) & 0x3f;
        let b = v & 0x1f;
        let r8 = ((r << 3) | (r >> 2)) as u8;
        let g8 = ((g << 2) | (g >> 4)) as u8;
        let b8 = ((b << 3) | (b >> 2)) as u8;
        let o = i * 4;
        dst[o] = b8;
        dst[o + 1] = g8;
        dst[o + 2] = r8;
        dst[o + 3] = 0xff;
    }
}

/// An outstanding consumer lease on one slot. Owning the counter (no borrow of
/// the framebuffer) lets a consumer hold a slot while a newer frame is written.
pub struct FrameLease {
    index: usize,
    generation: u64,
    counter: Arc<AtomicU32>,
}
impl FrameLease {
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
impl Drop for FrameLease {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Front/back BGRA8888 shadow buffers with lease tracking. Two slots only; a
/// slot still leased by a consumer is never overwritten.
pub struct ShadowFramebuffer {
    width: u16,
    height: u16,
    slot_bytes: usize,
    slots: [Vec<u8>; 2],
    leases: [Arc<AtomicU32>; 2],
    front: usize,
    generation: u64,
    published: u64,
    busy_drops: u64,
}

impl ShadowFramebuffer {
    pub fn new(width: u16, height: u16) -> Result<Self, MirrorError> {
        let slot_bytes = usize::from(width)
            .checked_mul(usize::from(height))
            .and_then(|n| n.checked_mul(4))
            .ok_or(MirrorError::Budget)?;
        if width == 0 || height == 0 || slot_bytes.checked_mul(2).is_none() {
            return Err(MirrorError::Budget);
        }
        if slot_bytes * 2 > MAX_SHADOW_BYTES {
            return Err(MirrorError::Budget);
        }
        Ok(Self {
            width,
            height,
            slot_bytes,
            slots: [vec![0u8; slot_bytes], vec![0u8; slot_bytes]],
            leases: [Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0))],
            front: 0,
            generation: 0,
            published: 0,
            busy_drops: 0,
        })
    }

    pub fn width(&self) -> u16 {
        self.width
    }
    pub fn height(&self) -> u16 {
        self.height
    }
    pub fn slot_bytes(&self) -> usize {
        self.slot_bytes
    }
    pub fn total_bytes(&self) -> usize {
        self.slot_bytes * 2
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn published(&self) -> u64 {
        self.published
    }
    pub fn busy_drops(&self) -> u64 {
        self.busy_drops
    }
    /// True when the non-front slot is leased by a consumer.
    pub fn back_is_leased(&self) -> bool {
        self.leases[1 - self.front].load(Ordering::Acquire) > 0
    }

    /// Write one RGB565 frame into the free back slot and promote it to front.
    /// Returns `Busy` when the back slot is still leased by a consumer.
    pub fn present(&mut self, rgb565: &[u8]) -> Result<u64, MirrorError> {
        let expected = usize::from(self.width) * usize::from(self.height) * 2;
        if rgb565.len() != expected {
            return Err(MirrorError::Geometry);
        }
        let back = 1 - self.front;
        if self.leases[back].load(Ordering::Acquire) > 0 {
            self.busy_drops = self.busy_drops.saturating_add(1);
            return Err(MirrorError::Busy);
        }
        rgb565_to_bgra(rgb565, &mut self.slots[back]);
        self.front = back;
        self.generation = self.generation.saturating_add(1);
        self.published = self.published.saturating_add(1);
        Ok(self.generation)
    }

    /// Lease the front buffer for a consumer. Bounded by [`MAX_LEASES`].
    pub fn acquire(&self) -> Option<FrameLease> {
        let index = self.front;
        let counter = &self.leases[index];
        if counter.load(Ordering::Acquire) >= MAX_LEASES {
            return None;
        }
        counter.fetch_add(1, Ordering::AcqRel);
        Some(FrameLease {
            index,
            generation: self.generation,
            counter: Arc::clone(counter),
        })
    }

    /// Borrow the pixels of a previously leased slot.
    pub fn frame_view<'a>(&'a self, lease: &FrameLease) -> ShadowFrameView<'a> {
        ShadowFrameView {
            width: self.width,
            height: self.height,
            generation: lease.generation,
            pixels: &self.slots[lease.index],
        }
    }
}

struct MirrorAssembler {
    frame_id: u64,
    expected: usize,
    staging: Vec<u8>,
    ranges: Vec<(usize, usize)>,
    max_chunk: usize,
}

impl MirrorAssembler {
    fn begin(frame_id: u64, expected: usize, max_chunk: usize) -> Result<Self, MirrorError> {
        if expected == 0 || expected > MAX_MIRROR_FRAME_BYTES || max_chunk == 0 {
            return Err(MirrorError::Budget);
        }
        Ok(Self {
            frame_id,
            expected,
            staging: vec![0u8; expected],
            ranges: Vec::new(),
            max_chunk,
        })
    }
    fn chunk(&mut self, offset: usize, bytes: &[u8]) -> Result<(), MirrorError> {
        let end = offset
            .checked_add(bytes.len())
            .ok_or(MirrorError::Geometry)?;
        if bytes.is_empty() || bytes.len() > self.max_chunk || end > self.expected {
            return Err(MirrorError::Geometry);
        }
        for &(start, finish) in &self.ranges {
            if offset < finish && end > start {
                if offset >= start && end <= finish && self.staging[offset..end] == *bytes {
                    return Ok(());
                }
                return Err(MirrorError::Geometry);
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
    fn commit(self) -> Result<Vec<u8>, MirrorError> {
        if self.ranges.len() == 1 && self.ranges[0] == (0, self.expected) {
            Ok(self.staging)
        } else {
            Err(MirrorError::Geometry)
        }
    }
}

/// Counters a host / inspector can report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorStats {
    pub frames_received: u64,
    pub frames_published: u64,
    pub frames_dropped: u64,
    pub last_frame_id: u64,
    pub generation: u64,
}

/// Consumes a device's mirrored byte stream into a [`ShadowFramebuffer`].
pub struct MirrorReceiver<S: MirrorSink> {
    fb: ShadowFramebuffer,
    sink: S,
    surface_id: String,
    rx: Vec<u8>,
    assembler: Option<MirrorAssembler>,
    max_message: usize,
    max_chunk: usize,
    stats: MirrorStats,
}

impl<S: MirrorSink> MirrorReceiver<S> {
    /// Build a receiver for one surface. `mode` must be `full-frame`; the
    /// dirty-region optimization is not implemented yet and is rejected.
    pub fn new(
        surface_id: &str,
        width: u16,
        height: u16,
        mode: &str,
        sink: S,
    ) -> Result<Self, MirrorError> {
        if mode != "full-frame" {
            return Err(MirrorError::Mode(mode.to_string()));
        }
        Ok(Self {
            fb: ShadowFramebuffer::new(width, height)?,
            sink,
            surface_id: surface_id.to_string(),
            rx: Vec::new(),
            assembler: None,
            max_message: MAX_MIRROR_FRAME_BYTES,
            max_chunk: mdc_protocol::DEFAULT_MAX_CHUNK,
            stats: MirrorStats::default(),
        })
    }

    pub fn stats(&self) -> MirrorStats {
        let mut stats = self.stats.clone();
        stats.generation = self.fb.generation();
        stats
    }
    pub fn shadow_bytes(&self) -> usize {
        self.fb.total_bytes()
    }
    pub fn sink(&self) -> &S {
        &self.sink
    }
    /// Lease the latest published frame (for a preview consumer).
    pub fn latest(&self) -> Option<FrameLease> {
        self.fb.acquire()
    }
    /// Borrow a leased frame for a preview consumer.
    pub fn frame_view<'a>(&'a self, lease: &FrameLease) -> ShadowFrameView<'a> {
        self.fb.frame_view(lease)
    }

    /// Read everything currently available from a transport and ingest it.
    pub fn pump<T: Transport>(&mut self, transport: &mut T) -> Result<(), MirrorError> {
        loop {
            match transport.read() {
                Ok(Some(bytes)) => self.feed(&bytes)?,
                Ok(None) => return Ok(()),
                Err(error) => return Err(MirrorError::Transport(error.to_string())),
            }
        }
    }

    /// Ingest raw protocol bytes. A mirror frame that is corrupt, oversized, or
    /// for the wrong geometry is dropped without breaking the stream.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), MirrorError> {
        self.rx.extend_from_slice(bytes);
        loop {
            match mdc_protocol::decode_stream(&self.rx, self.max_message) {
                mdc_protocol::StreamStep::Packet(packet, consumed) => {
                    self.rx.drain(..consumed);
                    if packet.kind == MessageType::MirrorFrame {
                        self.handle_mirror(&packet.payload);
                    }
                }
                mdc_protocol::StreamStep::NeedMore { discard } => {
                    if discard > 0 {
                        self.rx.drain(..discard.min(self.rx.len()));
                    }
                    if self.rx.len() > self.max_message + HEADER_LEN {
                        self.rx.clear();
                        self.assembler = None;
                        return Err(MirrorError::Protocol("mirror stream overrun".into()));
                    }
                    return Ok(());
                }
                mdc_protocol::StreamStep::Rejected { discard, .. } => {
                    self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
                    self.assembler = None;
                    if discard > 0 {
                        self.rx.drain(..discard.min(self.rx.len()));
                    }
                }
            }
        }
    }

    fn handle_mirror(&mut self, payload: &[u8]) {
        let decoded = match decode_frame_payload(payload) {
            Ok(value) => value,
            Err(_) => {
                self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
                return;
            }
        };
        match decoded {
            FramePayload::Legacy {
                surface_id,
                frame_id,
                width,
                height,
                bytes,
            } => self.deliver(surface_id, frame_id, width, height, &bytes),
            FramePayload::Begin {
                frame_id,
                total_bytes,
                ..
            } => {
                self.assembler =
                    MirrorAssembler::begin(frame_id, total_bytes as usize, self.max_chunk).ok();
                if self.assembler.is_none() {
                    self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
                }
            }
            FramePayload::Chunk {
                frame_id,
                offset,
                data,
            } => {
                let ok = match self.assembler.as_mut() {
                    Some(assembler) if assembler.frame_id == frame_id => {
                        assembler.chunk(offset as usize, &data).is_ok()
                    }
                    _ => false,
                };
                if !ok {
                    self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
                }
            }
            FramePayload::Commit { frame_id } => {
                let finished = match self.assembler.take() {
                    Some(assembler) if assembler.frame_id == frame_id => {
                        assembler.commit().ok().map(|bytes| (frame_id, bytes))
                    }
                    _ => None,
                };
                match finished {
                    Some((frame_id, bytes)) => {
                        let (w, h) = (self.fb.width(), self.fb.height());
                        self.deliver(self.surface_id.clone(), frame_id, w, h, &bytes);
                    }
                    None => {
                        self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
                    }
                }
            }
        }
    }

    fn deliver(
        &mut self,
        surface_id: String,
        frame_id: u64,
        width: u16,
        height: u16,
        bytes: &[u8],
    ) {
        let expected = usize::from(width) * usize::from(height) * 2;
        if surface_id != self.surface_id
            || width != self.fb.width()
            || height != self.fb.height()
            || bytes.len() != expected
            || expected > MAX_MIRROR_FRAME_BYTES
        {
            self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
            return;
        }
        self.stats.frames_received = self.stats.frames_received.saturating_add(1);
        match self.fb.present(bytes) {
            Ok(_) => {
                self.stats.frames_published = self.stats.frames_published.saturating_add(1);
                self.stats.last_frame_id = frame_id;
                // Split the borrow so the sink reads the leased front buffer.
                let fb = &self.fb;
                let sink = &mut self.sink;
                if let Some(lease) = fb.acquire() {
                    let view = fb.frame_view(&lease);
                    sink.on_frame(ShadowFrame {
                        width: view.width,
                        height: view.height,
                        generation: view.generation,
                        frame_id,
                        pixels: view.pixels,
                    });
                }
            }
            Err(_) => {
                self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
            }
        }
    }
}

/// Test double for an OBS adapter: records frame metadata and the last pixels.
#[derive(Default)]
pub struct MockObsSink {
    pub frames: Vec<(u64, u16, u16)>,
    pub last: Vec<u8>,
}

impl MirrorSink for MockObsSink {
    fn on_frame(&mut self, frame: ShadowFrame<'_>) {
        self.frames
            .push((frame.frame_id, frame.width, frame.height));
        self.last = frame.pixels.to_vec();
    }
}

/// Encode a BGRA8888 frame as a binary PPM (P6) image. OBS-adjacent previews and
/// tests can use this without linking an image library.
pub fn encode_ppm(width: u16, height: u16, bgra: &[u8]) -> Vec<u8> {
    let pixels = usize::from(width) * usize::from(height);
    let mut out = Vec::with_capacity(pixels * 3 + 32);
    out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    for chunk in bgra.chunks_exact(4) {
        out.push(chunk[2]); // R
        out.push(chunk[1]); // G
        out.push(chunk[0]); // B
    }
    out
}

/// Reference OBS/preview sink: keeps the last frame encoded as PPM bytes.
#[derive(Default)]
pub struct PpmSink {
    pub frame_count: u64,
    pub last: Vec<u8>,
}

impl PpmSink {
    pub fn last_bytes(&self) -> &[u8] {
        &self.last
    }
}

impl MirrorSink for PpmSink {
    fn on_frame(&mut self, frame: ShadowFrame<'_>) {
        self.last = encode_ppm(frame.width, frame.height, frame.pixels);
        self.frame_count = self.frame_count.saturating_add(1);
    }
}

/// A complete published frame owned by a slow consumer (OBS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedFrame {
    pub frame_id: u64,
    pub generation: u64,
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u8>,
}

/// Bounded latest-wins frame queue for a consumer slower than the device. Both
/// the frame count and the total pixel bytes are capped; a full queue drops the
/// oldest frame and counts it, so memory never grows without bound.
pub struct PublishQueue {
    max_frames: usize,
    max_bytes: usize,
    bytes: usize,
    frames: std::collections::VecDeque<QueuedFrame>,
    dropped: u64,
}

impl PublishQueue {
    pub fn new(max_frames: usize, max_bytes: usize) -> Self {
        Self {
            max_frames,
            max_bytes,
            bytes: 0,
            frames: std::collections::VecDeque::new(),
            dropped: 0,
        }
    }
    pub fn len(&self) -> usize {
        self.frames.len()
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
    /// Newest frame without consuming it — a pacing boundary lets OBS read the
    /// same frame more than once (OBS 30 FPS vs device 15 FPS).
    pub fn latest(&self) -> Option<&QueuedFrame> {
        self.frames.back()
    }
    pub fn push(&mut self, frame: QueuedFrame) {
        if self.max_frames == 0 || frame.pixels.len() > self.max_bytes {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        while !self.frames.is_empty()
            && (self.frames.len() + 1 > self.max_frames
                || self.bytes + frame.pixels.len() > self.max_bytes)
        {
            if let Some(old) = self.frames.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.pixels.len());
                self.dropped = self.dropped.saturating_add(1);
            }
        }
        self.bytes += frame.pixels.len();
        self.frames.push_back(frame);
    }
    pub fn pop(&mut self) -> Option<QueuedFrame> {
        let frame = self.frames.pop_front()?;
        self.bytes = self.bytes.saturating_sub(frame.pixels.len());
        Some(frame)
    }
}

/// A [`MirrorSink`] that copies each frame into a shared [`PublishQueue`]. The
/// copy is bounded by the queue's byte cap, never by the device rate.
pub struct QueueSink {
    queue: Arc<Mutex<PublishQueue>>,
}

impl QueueSink {
    pub fn new(max_frames: usize, max_bytes: usize) -> Self {
        Self {
            queue: Arc::new(Mutex::new(PublishQueue::new(max_frames, max_bytes))),
        }
    }
    pub fn queue(&self) -> Arc<Mutex<PublishQueue>> {
        Arc::clone(&self.queue)
    }
    pub fn with_queue(queue: Arc<Mutex<PublishQueue>>) -> Self {
        Self { queue }
    }
}

impl MirrorSink for QueueSink {
    fn on_frame(&mut self, frame: ShadowFrame<'_>) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push(QueuedFrame {
                frame_id: frame.frame_id,
                generation: frame.generation,
                width: frame.width,
                height: frame.height,
                pixels: frame.pixels.to_vec(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdc_protocol::{
        encode_frame_begin, encode_frame_chunk, encode_frame_commit, encode_frame_payload, Packet,
        VERSION,
    };
    use mdc_transport::MemoryLink;

    fn mirror_packet(payload: Vec<u8>) -> Vec<u8> {
        Packet {
            version: VERSION,
            kind: MessageType::MirrorFrame,
            flags: 0,
            request_id: 0,
            payload,
        }
        .encode(MAX_MIRROR_FRAME_BYTES)
        .unwrap()
    }

    #[test]
    fn shadow_budget_rejects_oversized_geometry_before_allocating() {
        // 4000×4000 BGRA ×2 is ~122 MiB, far above the 8 MiB cap.
        assert!(matches!(
            ShadowFramebuffer::new(4000, 4000),
            Err(MirrorError::Budget)
        ));
        assert!(matches!(
            ShadowFramebuffer::new(0, 10),
            Err(MirrorError::Budget)
        ));
        let ok = ShadowFramebuffer::new(480, 480).unwrap();
        assert_eq!(ok.slot_bytes(), 480 * 480 * 4);
        assert_eq!(ok.total_bytes(), 480 * 480 * 4 * 2);
        assert!(ok.total_bytes() <= MAX_SHADOW_BYTES);
    }

    #[test]
    fn rgb565_converts_to_bgra_with_alpha() {
        // 0xF800 = pure red, 0x07E0 = pure green, 0x001F = pure blue.
        let src = [0x00u8, 0xF8, 0xE0, 0x07, 0x1F, 0x00];
        let mut dst = vec![0u8; 12];
        rgb565_to_bgra(&src, &mut dst);
        assert_eq!(&dst[0..4], &[0x00, 0x00, 0xFF, 0xFF]); // B G R A
        assert_eq!(&dst[4..8], &[0x00, 0xFF, 0x00, 0xFF]);
        assert_eq!(&dst[8..12], &[0xFF, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn leased_buffer_is_not_overwritten_and_busy_writes_are_dropped() {
        let mut fb = ShadowFramebuffer::new(2, 1).unwrap();
        let red = [0x00u8, 0xF8, 0x00, 0xF8];
        let green = [0xE0u8, 0x07, 0xE0, 0x07];
        let blue = [0x1Fu8, 0x00, 0x1F, 0x00];
        fb.present(&red).unwrap();
        // Hold the red front while a green frame is written.
        let held = fb.acquire().unwrap();
        assert_eq!(held.index(), 1);
        fb.present(&green).unwrap();
        // BGRA: index 0 is B, 2 is R. Held red must be untouched.
        assert_eq!(fb.frame_view(&held).pixels[2], 0xFF);
        assert_eq!(fb.frame_view(&held).pixels[5], 0x00);
        // The old front (red) is now the back and still leased: a third write
        // cannot reuse it.
        assert_eq!(fb.present(&blue), Err(MirrorError::Busy));
        assert_eq!(fb.busy_drops(), 1);
        // The newest front is green (G channel at index 5).
        let newest = fb.acquire().unwrap();
        assert_eq!(fb.frame_view(&newest).pixels[5], 0xFF);
        drop(newest);
        drop(held);
        // Once released, the back slot is reusable.
        assert!(fb.present(&blue).is_ok());
        assert_eq!(fb.published(), 3);
    }

    #[test]
    fn receiver_publishes_full_and_chunked_frames() {
        struct SharedSink(std::rc::Rc<std::cell::RefCell<MockObsSink>>);
        impl MirrorSink for SharedSink {
            fn on_frame(&mut self, frame: ShadowFrame<'_>) {
                self.0.borrow_mut().on_frame(frame);
            }
        }
        let observed = std::rc::Rc::new(std::cell::RefCell::new(MockObsSink::default()));
        let mut receiver =
            MirrorReceiver::new("main", 2, 1, "full-frame", SharedSink(observed)).unwrap();

        receiver
            .feed(&mirror_packet(
                encode_frame_payload("main", 1, 2, 1, &[0x00, 0xF8, 0x1F, 0x00]).unwrap(),
            ))
            .unwrap();
        assert_eq!(receiver.stats().frames_published, 1);
        assert_eq!(receiver.stats().last_frame_id, 1);

        // Chunked frame 2.
        receiver
            .feed(&mirror_packet(
                encode_frame_begin("main", 2, 2, 1, 4).unwrap(),
            ))
            .unwrap();
        receiver
            .feed(&mirror_packet(encode_frame_chunk(
                2,
                0,
                &[0xE0, 0x07, 0x1F, 0x00],
            )))
            .unwrap();
        receiver
            .feed(&mirror_packet(encode_frame_commit(2)))
            .unwrap();
        let stats = receiver.stats();
        assert_eq!(stats.frames_received, 2);
        assert_eq!(stats.frames_published, 2);
        assert_eq!(stats.last_frame_id, 2);
        assert_eq!(stats.frames_dropped, 0);
        assert_eq!(receiver.shadow_bytes(), 16);
    }

    #[test]
    fn receiver_drops_wrong_geometry_and_corrupt_payloads() {
        let mut receiver =
            MirrorReceiver::new("main", 2, 1, "full-frame", MockObsSink::default()).unwrap();
        // Wrong surface.
        receiver
            .feed(&mirror_packet(
                encode_frame_payload("other", 1, 2, 1, &[0, 0, 0, 0]).unwrap(),
            ))
            .unwrap();
        // Wrong geometry (4×1 vs 2×1).
        receiver
            .feed(&mirror_packet(
                encode_frame_payload("main", 1, 4, 1, &[0; 8]).unwrap(),
            ))
            .unwrap();
        // Corrupt checksum on an otherwise valid mirror packet.
        let mut bad = mirror_packet(encode_frame_payload("main", 2, 2, 1, &[0; 4]).unwrap());
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        receiver.feed(&bad).unwrap();
        let stats = receiver.stats();
        assert_eq!(stats.frames_received, 0);
        assert!(stats.frames_dropped >= 2);
        assert_eq!(stats.frames_published, 0);
    }

    #[test]
    fn dirty_region_mode_is_rejected() {
        assert_eq!(
            MirrorReceiver::new("main", 2, 1, "dirty-region", MockObsSink::default()).err(),
            Some(MirrorError::Mode("dirty-region".into()))
        );
    }

    #[test]
    fn pump_reads_a_memory_link_end_to_end() {
        let (mut host, mut device) = MemoryLink::pair();
        let mut receiver =
            MirrorReceiver::new("main", 2, 1, "full-frame", MockObsSink::default()).unwrap();
        device
            .write(&mirror_packet(
                encode_frame_payload("main", 9, 2, 1, &[0; 4]).unwrap(),
            ))
            .unwrap();
        receiver.pump(&mut host).unwrap();
        assert_eq!(receiver.stats().frames_published, 1);
        assert_eq!(receiver.stats().last_frame_id, 9);
        assert_eq!(receiver.sink().frames, vec![(9, 2, 1)]);
    }

    #[test]
    fn ppm_encodes_bgra_as_rgb() {
        let ppm = encode_ppm(2, 1, &[0, 0, 255, 255, 0, 255, 0, 255]);
        assert_eq!(&ppm[..11], b"P6\n2 1\n255\n");
        assert_eq!(&ppm[11..], &[255, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn ppm_sink_encodes_each_published_frame() {
        let mut receiver =
            MirrorReceiver::new("main", 2, 1, "full-frame", PpmSink::default()).unwrap();
        // red, green in RGB565 little-endian.
        receiver
            .feed(&mirror_packet(
                encode_frame_payload("main", 1, 2, 1, &[0x00, 0xF8, 0xE0, 0x07]).unwrap(),
            ))
            .unwrap();
        let sink = receiver.sink();
        assert_eq!(sink.frame_count, 1);
        assert_eq!(&sink.last_bytes()[11..], &[255, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn publish_queue_is_bounded_and_latest_wins() {
        let frame = |id: u64| QueuedFrame {
            frame_id: id,
            generation: id,
            width: 1,
            height: 1,
            pixels: vec![id as u8; 4],
        };
        let mut queue = PublishQueue::new(2, 64);
        queue.push(frame(1));
        queue.push(frame(2));
        queue.push(frame(3));
        assert_eq!(queue.len(), 2, "oldest is dropped to make room");
        assert_eq!(queue.dropped(), 1);
        assert_eq!(queue.bytes(), 8);
        assert_eq!(queue.latest().unwrap().frame_id, 3);
        assert_eq!(queue.pop().unwrap().frame_id, 2);
        assert_eq!(queue.pop().unwrap().frame_id, 3);
        assert!(queue.is_empty());
        assert_eq!(queue.bytes(), 0);
    }

    #[test]
    fn publish_queue_rejects_a_frame_larger_than_its_byte_cap() {
        let mut queue = PublishQueue::new(4, 2);
        queue.push(QueuedFrame {
            frame_id: 1,
            generation: 1,
            width: 1,
            height: 1,
            pixels: vec![0; 4],
        });
        assert!(queue.is_empty());
        assert_eq!(queue.dropped(), 1);
    }

    #[test]
    fn queue_sink_decouples_and_paces_the_consumer() {
        let sink = QueueSink::new(4, 1024);
        let queue = sink.queue();
        let mut receiver = MirrorReceiver::new("main", 2, 1, "full-frame", sink).unwrap();
        for id in 1..=2 {
            receiver
                .feed(&mirror_packet(
                    encode_frame_payload("main", id, 2, 1, &[0; 4]).unwrap(),
                ))
                .unwrap();
        }
        let mut guard = queue.lock().unwrap();
        assert_eq!(guard.len(), 2);
        // OBS reads the newest frame repeatedly while the device is slower.
        let first = guard.latest().unwrap().frame_id;
        let second = guard.latest().unwrap().frame_id;
        assert_eq!((first, second), (2, 2));
        assert_eq!(guard.pop().unwrap().frame_id, 1);
        assert_eq!(guard.len(), 1);
    }

    #[test]
    fn a_held_lease_keeps_the_frame_available_across_many_writes() {
        // Two slots: with one held, alternating writes must never grow memory
        // and must keep the held frame intact.
        let mut fb = ShadowFramebuffer::new(2, 2).unwrap();
        let first = vec![0x11u8; 8];
        let mut expected = vec![0u8; 16];
        rgb565_to_bgra(&first, &mut expected);
        fb.present(&first).unwrap();
        let held = fb.acquire().unwrap();
        let bytes = fb.total_bytes();
        for i in 0..50u8 {
            let _ = fb.present(&[i; 8]);
            assert_eq!(fb.total_bytes(), bytes, "shadow memory stays bounded");
        }
        // The very first frame is pinned while a consumer holds its lease.
        assert_eq!(fb.frame_view(&held).pixels, &expected[..]);
        assert!(fb.busy_drops() >= 1);
    }
}
