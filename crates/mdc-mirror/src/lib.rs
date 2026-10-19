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
mod jpeg;
pub use jpeg::encode_jpeg;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
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
    fn commit(self) -> Result<Commit, MirrorError> {
        if self.ranges.is_empty() {
            return Err(MirrorError::Geometry);
        }
        if self.ranges.len() == 1 && self.ranges[0] == (0, self.expected) {
            Ok(Commit::Full(self.staging))
        } else {
            Ok(Commit::Dirty {
                staging: self.staging,
                ranges: self.ranges,
            })
        }
    }
}

/// Result of a committed mirror transaction: a whole frame, or the changed byte
/// ranges of a dirty-region update.
enum Commit {
    Full(Vec<u8>),
    Dirty {
        staging: Vec<u8>,
        ranges: Vec<(usize, usize)>,
    },
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
    mode: String,
    /// Last complete RGB565 frame; dirty-region updates patch it in place.
    base: Vec<u8>,
    rx: Vec<u8>,
    assembler: Option<MirrorAssembler>,
    max_message: usize,
    max_chunk: usize,
    stats: MirrorStats,
}

impl<S: MirrorSink> MirrorReceiver<S> {
    /// Build a receiver for one surface. `full-frame` accepts only whole frames;
    /// `dirty-region` additionally accepts partial updates patched onto the last
    /// complete frame (a dirty-region device still accepts full frames).
    pub fn new(
        surface_id: &str,
        width: u16,
        height: u16,
        mode: &str,
        sink: S,
    ) -> Result<Self, MirrorError> {
        if mode != "full-frame" && mode != "dirty-region" {
            return Err(MirrorError::Mode(mode.to_string()));
        }
        Ok(Self {
            fb: ShadowFramebuffer::new(width, height)?,
            sink,
            surface_id: surface_id.to_string(),
            mode: mode.to_string(),
            base: Vec::new(),
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
            } => self.deliver_full(surface_id, frame_id, width, height, &bytes),
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
                    Some(assembler) if assembler.frame_id == frame_id => assembler.commit().ok(),
                    _ => None,
                };
                match finished {
                    Some(Commit::Full(bytes)) => {
                        let (w, h) = (self.fb.width(), self.fb.height());
                        self.deliver_full(self.surface_id.clone(), frame_id, w, h, &bytes);
                    }
                    Some(Commit::Dirty { staging, ranges }) => {
                        self.deliver_dirty(frame_id, &staging, &ranges);
                    }
                    None => {
                        self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
                    }
                }
            }
        }
    }

    fn deliver_full(
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
        self.base.clear();
        self.base.extend_from_slice(bytes);
        self.stats.frames_received = self.stats.frames_received.saturating_add(1);
        self.publish(frame_id);
    }

    /// Patch a dirty-region update onto the last complete frame. A device that
    /// only advertised `full-frame` cannot use this path; a misaligned range or
    /// an update before any base frame is dropped.
    fn deliver_dirty(&mut self, frame_id: u64, staging: &[u8], ranges: &[(usize, usize)]) {
        let expected = usize::from(self.fb.width()) * usize::from(self.fb.height()) * 2;
        if self.mode != "dirty-region"
            || self.base.len() != expected
            || expected > MAX_MIRROR_FRAME_BYTES
            || ranges.is_empty()
        {
            self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
            return;
        }
        for &(start, end) in ranges {
            // Dirty ranges must align to whole RGB565 pixels.
            if start % 2 != 0 || (end - start) % 2 != 0 || end > expected {
                self.stats.frames_dropped = self.stats.frames_dropped.saturating_add(1);
                return;
            }
        }
        for &(start, end) in ranges {
            self.base[start..end].copy_from_slice(&staging[start..end]);
        }
        self.stats.frames_received = self.stats.frames_received.saturating_add(1);
        self.publish(frame_id);
    }

    fn publish(&mut self, frame_id: u64) {
        match self.fb.present(&self.base) {
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

/// Reference OBS "image source" sink: writes each frame to a file with an
/// atomic temp+rename so a watcher never reads a half-written file. `ring == 0`
/// keeps one file; `ring == n` rotates `name_0000..name_(n-1)`.
pub struct PpmDirectorySink {
    dir: PathBuf,
    stem: String,
    ring: usize,
    frames: u64,
    last_len: usize,
    last_error: Option<String>,
}

impl PpmDirectorySink {
    pub fn new(dir: impl Into<PathBuf>, name: &str, ring: usize) -> Self {
        let dir = dir.into();
        let _ = std::fs::create_dir_all(&dir);
        let stem = name.trim_end_matches(".ppm").to_string();
        Self {
            dir,
            stem,
            ring,
            frames: 0,
            last_len: 0,
            last_error: None,
        }
    }
    pub fn frames_written(&self) -> u64 {
        self.frames
    }
    pub fn last_bytes(&self) -> usize {
        self.last_len
    }
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
    fn target_name(&self) -> String {
        if self.ring == 0 {
            format!("{}.ppm", self.stem)
        } else {
            format!("{}_{:04}.ppm", self.stem, self.frames % self.ring as u64)
        }
    }
}

impl MirrorSink for PpmDirectorySink {
    fn on_frame(&mut self, frame: ShadowFrame<'_>) {
        let bytes = encode_ppm(frame.width, frame.height, frame.pixels);
        let target = self.dir.join(self.target_name());
        let tmp = self.dir.join(format!("{}.tmp", self.stem));
        let result = std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, &target));
        match result {
            Ok(()) => {
                self.frames = self.frames.saturating_add(1);
                self.last_len = bytes.len();
                self.last_error = None;
            }
            Err(error) => {
                let _ = std::fs::remove_file(&tmp);
                self.last_error = Some(error.to_string());
            }
        }
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

/// A bounded TCP fan-out of mirrored frames (N30). A headless preview, an OBS
/// browser source, or a test can connect and read length-prefixed PPM frames.
/// At most one frame is queued per client: a consumer slower than the device
/// drops older frames (latest-wins) instead of growing memory.
pub struct MirrorServer {
    listener: TcpListener,
    clients: Vec<ServerClient>,
    max_clients: usize,
    frames: u64,
    dropped: u64,
}

struct ServerClient {
    stream: TcpStream,
    pending: Option<Vec<u8>>,
}

impl MirrorServer {
    pub fn bind(addr: &str) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            clients: Vec::new(),
            max_clients: 8,
            frames: 0,
            dropped: 0,
        })
    }
    pub fn with_max_clients(mut self, max_clients: usize) -> Self {
        self.max_clients = max_clients;
        self
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
    /// Accept any pending clients (non-blocking), capped at `max_clients`.
    pub fn accept_pending(&mut self) -> io::Result<usize> {
        let mut accepted = 0;
        while self.clients.len() < self.max_clients {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(true);
                    self.clients.push(ServerClient {
                        stream,
                        pending: None,
                    });
                    accepted += 1;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        Ok(accepted)
    }
    /// Queue a frame for every client. A client that still has an unwritten
    /// frame counts a drop and keeps only the newest.
    pub fn publish(&mut self, frame: &ShadowFrame<'_>) {
        let body = encode_ppm(frame.width, frame.height, frame.pixels);
        let mut packet = Vec::with_capacity(4 + body.len());
        packet.extend_from_slice(&(body.len() as u32).to_be_bytes());
        packet.extend_from_slice(&body);
        for client in &mut self.clients {
            if client.pending.is_some() {
                self.dropped = self.dropped.saturating_add(1);
            }
            client.pending = Some(packet.clone());
        }
        self.frames = self.frames.saturating_add(1);
    }
    /// Try to write each queued frame. A client that would block keeps its frame;
    /// a client that errors is dropped.
    pub fn flush(&mut self) {
        self.clients.retain_mut(|client| {
            if let Some(packet) = client.pending.take() {
                match client.stream.write_all(&packet) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        client.pending = Some(packet);
                    }
                    Err(_) => return false,
                }
            }
            true
        });
    }
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }
    pub fn frames_published(&self) -> u64 {
        self.frames
    }
    pub fn frames_dropped(&self) -> u64 {
        self.dropped
    }
}

/// N143: encode a BGRA frame as a 24-bit BMP (bottom-up rows, 4-byte padding).
/// BMP is chosen for the OBS browser source because it needs no JPEG encoder.
pub fn encode_bmp(width: u16, height: u16, bgra: &[u8]) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let row_bytes = w * 3;
    let padded = (row_bytes + 3) & !3;
    let image_size = padded * h;
    let file_size = 54 + image_size;
    let mut out = Vec::with_capacity(file_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(image_size as u32).to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    let mut row = vec![0u8; padded];
    for y in (0..h).rev() {
        row.iter_mut().for_each(|byte| *byte = 0);
        for x in 0..w {
            let src = (y * w + x) * 4;
            if let Some(pixel) = bgra.get(src..src + 4) {
                row[x * 3] = pixel[0];
                row[x * 3 + 1] = pixel[1];
                row[x * 3 + 2] = pixel[2];
            }
        }
        out.extend_from_slice(&row);
    }
    out
}

/// N328: encode a BGRA frame as a PNG (8-bit RGBA). No compression library is
/// used — the zlib stream is a series of *stored* deflate blocks, so the output
/// is valid but not compressed. Still much smaller than a bottom-up BMP for a
/// screenshot-like image with few distinct colours.
pub fn encode_png(width: u16, height: u16, bgra: &[u8]) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let Some(raw_len) = w
        .checked_mul(4)
        .and_then(|row| row.checked_add(1))
        .and_then(|row| h.checked_mul(row))
    else {
        return Vec::new();
    };
    if raw_len > MAX_SHADOW_BYTES {
        return Vec::new();
    }
    // Raw image: each scanline is a filter byte (0 = none) then RGBA pixels.
    let mut raw = Vec::with_capacity(raw_len);
    for y in 0..h {
        raw.push(0u8);
        for x in 0..w {
            let src = (y * w + x) * 4;
            match bgra.get(src..src + 4) {
                Some(pixel) => raw.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]),
                None => raw.extend_from_slice(&[0, 0, 0, 0]),
            }
        }
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&u32::from(width).to_be_bytes());
    ihdr.extend_from_slice(&u32::from(height).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit, RGBA, no interlace
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// N328: a zlib stream of stored deflate blocks (no compression).
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    if raw.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
    }
    let mut at = 0;
    while at < raw.len() {
        let len = (raw.len() - at).min(0xFFFF);
        let last = at + len >= raw.len();
        out.push(u8::from(last));
        out.extend_from_slice(&(len as u16).to_le_bytes());
        out.extend_from_slice(&(!(len as u16)).to_le_bytes());
        out.extend_from_slice(&raw[at..at + len]);
        at += len;
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in bytes {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// The most recent mirrored frame, cached in both served encodings.
struct ServedFrame {
    // N346/N356: the raw BGRA frame; every served encoding is produced on demand
    // (with `?scale=`/`?quality=`).
    width: u16,
    height: u16,
    bgra: Vec<u8>,
}

/// N144: a minimal HTTP/1.1 fan-out for an OBS browser source. It serves the
/// latest mirrored frame as `/frame.bmp`, `/frame.ppm`, or an auto-refreshing
/// page at `/`.
///
/// N313: `/stream` (and `/stream.bmp`) is a live `multipart/x-mixed-replace`
/// push. Instead of polling a still image, a browser source keeps one connection
/// open and receives a new BMP part per published frame. Each streaming client
/// buffers at most one part; if it cannot drain it before the next frame the
/// frame is dropped (`clipped`) so memory stays bounded.
///
/// One-shot connections are short-lived (one request, `Connection: close`); at
/// most `max_clients` are open, and a slow client never blocks `pump`.
pub struct HttpMirrorServer {
    listener: TcpListener,
    clients: Vec<HttpClient>,
    max_clients: usize,
    frames: u64,
    last: Option<ServedFrame>,
    /// N507: WebSocket ping/close frames received from peers.
    ws_pings: u64,
    ws_closes: u64,
    /// N517: server-initiated ping interval in published frames (0 = disabled).
    ws_heartbeat: u64,
    /// N517: drop a WebSocket peer after this many frames without a reply.
    ws_timeout: u64,
    /// N517: pings the server sent and peers it dropped for silence.
    ws_pings_sent: u64,
    ws_timeouts: u64,
    /// N527: fragment payload size for WebSocket messages (0 = one frame).
    ws_fragment: usize,
    /// N548: complete client -> server WebSocket messages reassembled.
    ws_messages: u64,
    /// N558: frames whose reserved (compression) bit was set (not negotiated).
    ws_reserved: u64,
    /// N567: peers that offered a WebSocket extension in the handshake.
    ws_offered: u64,
    /// N567: close (1002) on a reserved-bit frame instead of ignoring it.
    ws_reject_reserved: bool,
    /// N577: bounded log of why a WebSocket connection was declined/failed.
    ws_log: Vec<WebSocketNote>,
}

/// N607: a structured WebSocket log entry (`kind` + `detail`), so a consumer can
/// filter on the reason without parsing a string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSocketNote {
    pub kind: String,
    pub detail: String,
}

/// N577: the most recent WebSocket notes kept.
const WS_LOG_MAX: usize = 16;

/// N637: the longest `detail` a [`WebSocketNote`] keeps (bytes). Longer details
/// are truncated on a UTF-8 boundary, so the log stays bounded even when a peer
/// sends a very long reason string.
pub const WS_NOTE_DETAIL_MAX: usize = 256;

/// N617: the shape version of a [`WebSocketNote`]; a consumer can pin to it.
pub const WS_NOTE_VERSION: u8 = 1;

/// N657: escape a string for the hand-built `/health` object.
fn escape_json(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if (other as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", other as u32)),
            other => out.push(other),
        }
    }
    out
}

/// N637: clamp a note's detail to [`WS_NOTE_DETAIL_MAX`] bytes on a char boundary.
fn truncate_detail(detail: &str) -> String {
    if detail.len() <= WS_NOTE_DETAIL_MAX {
        return detail.to_string();
    }
    let mut end = WS_NOTE_DETAIL_MAX;
    while end > 0 && !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail[..end].to_string()
}

struct HttpClient {
    stream: TcpStream,
    request: Vec<u8>,
    /// One-shot response; unused once the client is streaming.
    response: Option<Vec<u8>>,
    /// Multipart preamble, sent before the first part.
    stream_headers: Vec<u8>,
    /// At most one complete multipart part waiting to be written.
    stream_part: Vec<u8>,
    /// A long-lived multipart consumer rather than a one-shot request.
    streaming: bool,
    /// N337/N453: the streamed part format.
    stream_format: StreamFormat,
    /// N356: integer downscale applied to each streamed part (1 = full size).
    stream_scale: u16,
    /// N500: a WebSocket consumer (`/ws`) rather than a multipart stream.
    websocket: bool,
    /// N507: buffered bytes read from a WebSocket peer (bounded).
    ws_in: Vec<u8>,
    /// N507: pending control frames (pong/close) queued for the peer.
    ws_control: Vec<u8>,
    /// N507: a close frame is queued; drop the client once it drains.
    ws_closing: bool,
    /// N517: the frame counter when the peer was last heard from.
    ws_last_seen: u64,
    /// N527: pending WebSocket fragments for the current message.
    ws_pending: std::collections::VecDeque<Vec<u8>>,
    /// N548: a client message being reassembled from fragments, and its opcode.
    ws_message: Vec<u8>,
    ws_message_opcode: u8,
    /// N548: a fragmented client message is in progress.
    ws_fragmenting: bool,
    /// The peer closed or errored; dropped on the next pump.
    dead: bool,
    /// Frames skipped because the previous part was still being written.
    clipped: u64,
}

/// N313: multipart boundary for the live stream.
const MULTIPART_BOUNDARY: &str = "mdcframe";

/// N537: the smallest WebSocket fragment payload the server will use.
pub const MIN_WS_FRAGMENT: usize = 64;

/// The path a request for a live stream uses.
const STREAM_PATHS: [&str; 4] = ["/stream", "/stream.bmp", "/stream.jpg", "/stream.png"];

/// N453: the image format of each multipart stream part.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamFormat {
    Bmp,
    Jpeg,
    Png,
}

fn multipart_headers() -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: multipart/x-mixed-replace; boundary={MULTIPART_BOUNDARY}\r\n\
         Cache-Control: no-store\r\nPragma: no-cache\r\n\r\n"
    )
    .into_bytes()
}

fn multipart_part(content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 96);
    out.extend_from_slice(
        format!(
            "--{MULTIPART_BOUNDARY}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(body);
    out.extend_from_slice(b"\r\n");
    out
}

/// What a request path resolves to.
enum Served {
    Bytes(Vec<u8>),
    Multipart {
        /// N337/N453: the part format (BMP, JPEG, or PNG).
        format: StreamFormat,
        /// N356: integer downscale for each streamed part (1..=8).
        scale: u16,
    },
}

impl HttpMirrorServer {
    pub fn bind(addr: &str) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            clients: Vec::new(),
            max_clients: 8,
            frames: 0,
            last: None,
            ws_pings: 0,
            ws_closes: 0,
            ws_heartbeat: 0,
            ws_timeout: 0,
            ws_pings_sent: 0,
            ws_timeouts: 0,
            ws_fragment: 0,
            ws_messages: 0,
            ws_reserved: 0,
            ws_offered: 0,
            ws_reject_reserved: false,
            ws_log: Vec::new(),
        })
    }

    pub fn with_max_clients(mut self, max_clients: usize) -> Self {
        self.max_clients = max_clients.max(1);
        self
    }

    /// N517: ping WebSocket peers every `interval` published frames and drop a
    /// silent peer after `timeout` frames without a reply (0 disables either).
    pub fn with_ws_heartbeat(mut self, interval: u64, timeout: u64) -> Self {
        self.ws_heartbeat = interval;
        self.ws_timeout = timeout;
        self
    }

    /// N527: split WebSocket messages into fragments of at most `bytes` payload
    /// (0 keeps one frame per message, the RFC 6455 default for small payloads).
    /// N537: a non-zero size is raised to at least [`MIN_WS_FRAGMENT`] so a tiny
    /// value cannot turn one message into thousands of frames.
    pub fn with_ws_fragment(mut self, bytes: usize) -> Self {
        self.ws_fragment = if bytes == 0 {
            0
        } else {
            bytes.max(MIN_WS_FRAGMENT)
        };
        self
    }

    /// N517: server-initiated pings sent.
    pub fn ws_pings_sent(&self) -> u64 {
        self.ws_pings_sent
    }

    /// N517: WebSocket peers dropped for silence.
    pub fn ws_timeouts(&self) -> u64 {
        self.ws_timeouts
    }

    /// N548: complete client -> server WebSocket messages reassembled.
    pub fn ws_messages(&self) -> u64 {
        self.ws_messages
    }

    /// N558: client frames with the reserved (compression) bit set. The server
    /// never negotiates `permessage-deflate`, so these are counted and ignored
    /// (or, with [`with_ws_reject_reserved`](Self::with_ws_reject_reserved), the
    /// peer is closed with code 1002).
    pub fn ws_reserved(&self) -> u64 {
        self.ws_reserved
    }

    /// N567: peers that offered a WebSocket extension (e.g. `permessage-deflate`)
    /// in the handshake. The extension is never echoed, so it is not negotiated.
    pub fn ws_offered(&self) -> u64 {
        self.ws_offered
    }

    /// N567: when set, a frame that sets the reserved (compression) bit fails the
    /// connection with a `1002` close (RFC 6455 protocol error) instead of being
    /// silently ignored. Off by default (N558 compatibility).
    pub fn with_ws_reject_reserved(mut self, reject: bool) -> Self {
        self.ws_reject_reserved = reject;
        self
    }

    /// N577/N607: the bounded log of WebSocket declines/failures (oldest first),
    /// each an entry with a `kind` and a redacted `detail`.
    pub fn ws_log(&self) -> &[WebSocketNote] {
        &self.ws_log
    }

    /// N617: the WebSocket notes of one `kind` (`"extension"` | `"reserved"`).
    /// N647: how many retained notes there are per kind. The log itself is
    /// bounded, so this summary is too.
    pub fn ws_log_counts(&self) -> std::collections::BTreeMap<String, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for note in &self.ws_log {
            *counts.entry(note.kind.clone()).or_insert(0) += 1;
        }
        counts
    }

    /// N657: the per-kind summary as a JSON object (the mirror crate has no
    /// serde_json dependency, so the small map is written directly).
    fn ws_log_kinds_json(&self) -> String {
        let counts = self.ws_log_counts();
        let mut out = String::from("{");
        for (index, (kind, count)) in counts.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&format!("\"{}\":{}", escape_json(kind), count));
        }
        out.push('}');
        out
    }

    pub fn ws_log_of_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a WebSocketNote> {
        self.ws_log.iter().filter(move |note| note.kind == kind)
    }

    /// N637: record a note in the bounded WebSocket log (the detail is clamped to
    /// [`WS_NOTE_DETAIL_MAX`]). Public so an embedding host can log its own notes.
    pub fn note_ws(&mut self, kind: impl Into<String>, detail: impl Into<String>) {
        if self.ws_log.len() >= WS_LOG_MAX {
            self.ws_log.remove(0);
        }
        self.ws_log.push(WebSocketNote {
            kind: kind.into(),
            detail: truncate_detail(&detail.into()),
        });
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Cache the newest frame; a slow streaming client keeps at most the part it
    /// is still writing, so a newer frame is dropped rather than queued.
    pub fn publish(&mut self, frame: &ShadowFrame<'_>) {
        let bgra = frame.pixels.to_vec();
        if bgra.len() <= MAX_SHADOW_BYTES {
            self.last = Some(ServedFrame {
                width: frame.width,
                height: frame.height,
                bgra,
            });
        }
        self.frames = self.frames.saturating_add(1);
        let heartbeat = self.ws_heartbeat > 0 && self.frames.is_multiple_of(self.ws_heartbeat);
        let mut pings_sent = 0u64;
        if let Some(frame) = self.last.as_ref() {
            for client in &mut self.clients {
                if !client.streaming {
                    continue;
                }
                // N517: a server ping keeps a quiet WebSocket peer alive.
                if client.websocket && heartbeat {
                    client
                        .ws_control
                        .extend_from_slice(&ws_frame_opcode(0x9, b""));
                    pings_sent += 1;
                }
                if client.websocket {
                    // N527: the previous message must drain before the next one.
                    if !client.ws_pending.is_empty() {
                        client.clipped = client.clipped.saturating_add(1);
                        continue;
                    }
                    let payload = encode_jpeg(frame.width, frame.height, &frame.bgra, 80);
                    // N500: one binary WebSocket message per published frame;
                    // N527: optionally split into fragments.
                    if self.ws_fragment > 0 && payload.len() > self.ws_fragment {
                        let mut chunks = payload.chunks(self.ws_fragment);
                        if let Some(first) = chunks.next() {
                            client
                                .ws_pending
                                .push_back(ws_frame_part(false, 0x2, first));
                        }
                        let rest: Vec<&[u8]> = chunks.collect();
                        let count = rest.len();
                        for (index, chunk) in rest.into_iter().enumerate() {
                            let last = index + 1 == count;
                            client.ws_pending.push_back(ws_frame_part(last, 0x0, chunk));
                        }
                    } else {
                        client.ws_pending.push_back(ws_frame(&payload));
                    }
                    continue;
                }
                if !client.stream_part.is_empty() {
                    client.clipped = client.clipped.saturating_add(1);
                    continue;
                }
                {
                    // N356: honour the client's `?scale=` on the live stream.
                    client.stream_part =
                        stream_part_for(frame, client.stream_format, client.stream_scale);
                }
            }
        }
        self.ws_pings_sent = self.ws_pings_sent.saturating_add(pings_sent);
    }

    fn accept_pending(&mut self) -> io::Result<()> {
        while self.clients.len() < self.max_clients {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(true);
                    self.clients.push(HttpClient {
                        stream,
                        request: Vec::new(),
                        response: None,
                        stream_headers: Vec::new(),
                        stream_part: Vec::new(),
                        streaming: false,
                        stream_format: StreamFormat::Bmp,
                        stream_scale: 1,
                        websocket: false,
                        ws_in: Vec::new(),
                        ws_control: Vec::new(),
                        ws_closing: false,
                        ws_last_seen: 0,
                        ws_pending: std::collections::VecDeque::new(),
                        ws_message: Vec::new(),
                        ws_message_opcode: 0,
                        ws_fragmenting: false,
                        dead: false,
                        clipped: 0,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Accept, read one request per client, queue the response, then try to
    /// write it. A one-shot client is dropped once its response is written; a
    /// streaming client stays until the peer closes.
    pub fn pump(&mut self) -> io::Result<()> {
        self.accept_pending()?;
        let last = self.last.as_ref();
        // N361: a bounded health snapshot for a monitoring consumer.
        let health = {
            // N587: a short, escaped summary of the most recent WS note.
            let last = self
                .ws_log
                .last()
                .map(|note| {
                    let rendered = format!("{}: {}", note.kind, note.detail);
                    let truncated: String = rendered.chars().take(48).collect();
                    let ellipsis = if rendered.chars().count() > 48 {
                        "…"
                    } else {
                        ""
                    };
                    format!("{}{}", json_escape(&truncated), ellipsis)
                })
                .unwrap_or_default();
            let body = format!(
                "{{\"frames\":{},\"clients\":{},\"streaming\":{},\"clipped\":{},\"ws\":{},\
                  \"ws_pings\":{},\"ws_closes\":{},\"ws_timeouts\":{},\"ws_messages\":{},\
                  \"ws_reserved\":{},\"ws_offered\":{},\"ws_log\":{},\
                  \"ws_log_kinds\":{},\"ws_log_last\":\"{}\"}}",
                self.frames,
                self.clients.len(),
                self.streaming_clients(),
                self.clipped_frames(),
                self.ws_clients(),
                self.ws_pings,
                self.ws_closes,
                self.ws_timeouts,
                self.ws_messages,
                self.ws_reserved,
                self.ws_offered,
                self.ws_log.len(),
                self.ws_log_kinds_json(),
                last
            );
            simple_response(200, "application/json", body.as_bytes())
        };
        // N567: extension offers counted in this pass. N577: reasons logged.
        let mut offered = 0u64;
        let mut notes: Vec<WebSocketNote> = Vec::new();
        for client in &mut self.clients {
            if client.streaming || client.response.is_some() {
                continue;
            }
            let mut buffer = [0u8; 1024];
            match client.stream.read(&mut buffer) {
                Ok(0) => {}
                Ok(read) => {
                    if client.request.len() + read > 8192 {
                        client.response =
                            Some(simple_response(431, "text/plain", b"header too large"));
                        continue;
                    }
                    client.request.extend_from_slice(&buffer[..read]);
                    if let Some(end) = find_header_end(&client.request) {
                        let (path, query) = request_target(&client.request[..end]);
                        if path == "/health" {
                            client.response = Some(health.clone());
                        } else if path == "/ws" {
                            // N500: upgrade to a WebSocket that pushes one binary
                            // JPEG message per published frame.
                            let headers = &client.request[..end];
                            let key = header_value(headers, "sec-websocket-key");
                            let upgraded = header_value(headers, "upgrade")
                                .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
                            match (key, upgraded) {
                                (Some(key), true) => {
                                    // N567: count (but never echo) extension
                                    // offers such as `permessage-deflate`, so the
                                    // handshake explicitly declines compression.
                                    if let Some(ext) =
                                        header_value(headers, "sec-websocket-extensions")
                                    {
                                        offered += 1;
                                        // N597: log only the extension token, never
                                        // its parameters (which may carry peer data).
                                        notes.push(WebSocketNote {
                                            kind: "extension".into(),
                                            detail: extension_name(ext),
                                        });
                                    }
                                    client.websocket = true;
                                    client.streaming = true;
                                    // N507: keep any bytes sent after the upgrade
                                    // (skip the request's terminating CRLFCRLF).
                                    client.ws_in = client.request
                                        [(end + 4).min(client.request.len())..]
                                        .to_vec();
                                    client.stream_headers = ws_handshake(key);
                                    client.stream_part = last
                                        .map(|frame| {
                                            ws_frame(&encode_jpeg(
                                                frame.width,
                                                frame.height,
                                                &frame.bgra,
                                                80,
                                            ))
                                        })
                                        .unwrap_or_default();
                                }
                                _ => {
                                    client.response =
                                        Some(simple_response(400, "text/plain", b"bad upgrade"));
                                }
                            }
                        } else {
                            match respond(last, &path, &query) {
                                Served::Bytes(bytes) => client.response = Some(bytes),
                                Served::Multipart { format, scale } => {
                                    begin_stream(client, last, format, scale)
                                }
                            }
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => {
                    client.response = Some(simple_response(500, "text/plain", b"error"));
                }
            }
        }
        // N507: service WebSocket control frames, then flush pending output.
        let (mut pings, mut closes) = (0u64, 0u64);
        let mut messages = 0u64;
        let mut reserved = 0u64;
        let mut timeouts = 0u64;
        for client in &mut self.clients {
            if client.dead || !client.streaming {
                continue;
            }
            if client.websocket {
                let (seen_pings, seen_closes, seen_messages, seen_reserved) =
                    pump_websocket(client, self.frames, self.ws_reject_reserved);
                pings += seen_pings;
                closes += seen_closes;
                messages += seen_messages;
                reserved += seen_reserved;
                // N517: drop a peer that has been silent past the timeout.
                if self.ws_timeout > 0
                    && self.frames.saturating_sub(client.ws_last_seen) > self.ws_timeout
                {
                    client.dead = true;
                    timeouts += 1;
                    continue;
                }
            }
            if !client.dead {
                flush_stream(client);
            }
        }
        self.ws_pings = self.ws_pings.saturating_add(pings);
        self.ws_closes = self.ws_closes.saturating_add(closes);
        self.ws_messages = self.ws_messages.saturating_add(messages);
        self.ws_reserved = self.ws_reserved.saturating_add(reserved);
        self.ws_offered = self.ws_offered.saturating_add(offered);
        self.ws_timeouts = self.ws_timeouts.saturating_add(timeouts);
        for mut note in notes {
            // N637: a peer-supplied reason is clamped before it is stored.
            note.detail = truncate_detail(&note.detail);
            if self.ws_log.len() >= WS_LOG_MAX {
                self.ws_log.remove(0);
            }
            self.ws_log.push(note);
        }
        if reserved > 0 {
            let detail = if self.ws_reject_reserved {
                "closed 1002"
            } else {
                "ignored"
            };
            self.note_ws("reserved", detail);
        }
        self.clients.retain_mut(|client| {
            if client.dead {
                return false;
            }
            if client.streaming {
                return true;
            }
            let Some(response) = client.response.as_ref() else {
                return true;
            };
            match client.stream.write_all(response) {
                Ok(()) => false,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => true,
                Err(_) => false,
            }
        });
        Ok(())
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// N313: clients currently consuming the live multipart stream.
    pub fn streaming_clients(&self) -> usize {
        self.clients
            .iter()
            .filter(|client| client.streaming)
            .count()
    }

    /// N500: clients consuming the WebSocket live stream.
    pub fn ws_clients(&self) -> usize {
        self.clients
            .iter()
            .filter(|client| client.websocket)
            .count()
    }

    /// N507: WebSocket ping frames answered.
    pub fn ws_pings(&self) -> u64 {
        self.ws_pings
    }

    /// N507: WebSocket close frames received.
    pub fn ws_closes(&self) -> u64 {
        self.ws_closes
    }

    /// N313: frames skipped because a streaming client was still writing.
    pub fn clipped_frames(&self) -> u64 {
        self.clients.iter().map(|client| client.clipped).sum()
    }

    pub fn frames_published(&self) -> u64 {
        self.frames
    }

    pub fn has_frame(&self) -> bool {
        self.last.is_some()
    }
}

/// N313: start a live multipart stream for a client, seeding it with the newest
/// frame if one is already cached. N337: `jpeg` selects JPEG parts.
fn begin_stream(
    client: &mut HttpClient,
    last: Option<&ServedFrame>,
    format: StreamFormat,
    scale: u16,
) {
    client.streaming = true;
    client.stream_format = format;
    client.stream_scale = scale.clamp(1, 8);
    client.stream_headers = multipart_headers();
    client.stream_part = last
        .map(|frame| stream_part_for(frame, format, client.stream_scale))
        .unwrap_or_default();
}

/// N356: build a multipart part from the raw frame at an integer downscale.
fn stream_part_for(frame: &ServedFrame, format: StreamFormat, scale: u16) -> Vec<u8> {
    let (width, height, raw) = if scale <= 1 {
        (
            frame.width,
            frame.height,
            std::borrow::Cow::Borrowed(&frame.bgra),
        )
    } else {
        let (w, h, raw) = downscale_bgra(frame.width, frame.height, &frame.bgra, scale);
        (w, h, std::borrow::Cow::Owned(raw))
    };
    match format {
        StreamFormat::Jpeg => multipart_part("image/jpeg", &encode_jpeg(width, height, &raw, 80)),
        StreamFormat::Png => multipart_part("image/png", &encode_png(width, height, &raw)),
        StreamFormat::Bmp => multipart_part("image/bmp", &encode_bmp(width, height, &raw)),
    }
}

/// Write the pending preamble and part. Never buffers more than one part; a
/// `WouldBlock` leaves the remainder for the next pump.
fn flush_stream(client: &mut HttpClient) {
    // N527: WebSocket message fragments are written in order.
    while let Some(fragment) = client.ws_pending.front_mut() {
        match client.stream.write(fragment) {
            Ok(0) => {
                client.dead = true;
                return;
            }
            Ok(written) => {
                fragment.drain(..written);
                if fragment.is_empty() {
                    client.ws_pending.pop_front();
                } else {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(_) => {
                client.dead = true;
                return;
            }
        }
    }
    // N507: control frames (pong/close) take priority.
    if !client.ws_control.is_empty() {
        match client.stream.write(&client.ws_control) {
            Ok(0) => {
                client.dead = true;
                return;
            }
            Ok(written) => {
                client.ws_control.drain(..written);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(_) => {
                client.dead = true;
                return;
            }
        }
    }
    if !client.stream_headers.is_empty() {
        match client.stream.write(&client.stream_headers) {
            Ok(0) => {
                client.dead = true;
                return;
            }
            Ok(written) => {
                client.stream_headers.drain(..written);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(_) => {
                client.dead = true;
                return;
            }
        }
    }
    if !client.stream_part.is_empty() {
        match client.stream.write(&client.stream_part) {
            Ok(0) => client.dead = true,
            Ok(written) => {
                client.stream_part.drain(..written);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => client.dead = true,
        }
    }
    // N507: once the queued close frame is gone, the client is done.
    if client.ws_closing && client.ws_control.is_empty() {
        client.dead = true;
    }
}

/// N507: read and answer WebSocket control frames (ping/pong/close).
/// Returns `(pings, closes, messages)` observed. N517: any bytes from the peer
/// refresh the heartbeat (`frames` is the server's current published-frame
/// counter). N548: data frames (including fragmentation/continuation) are
/// reassembled into complete messages.
fn pump_websocket(
    client: &mut HttpClient,
    frames: u64,
    reject_reserved: bool,
) -> (u64, u64, u64, u64) {
    let mut buffer = [0u8; 512];
    match client.stream.read(&mut buffer) {
        Ok(0) => {
            client.dead = true;
            return (0, 0, 0, 0);
        }
        Ok(read) => {
            // N517: any traffic from the peer counts as a heartbeat.
            client.ws_last_seen = frames;
            if client.ws_in.len() + read <= 4096 {
                client.ws_in.extend_from_slice(&buffer[..read]);
            } else {
                client.ws_in.clear();
            }
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return (0, 0, 0, 0),
        Err(_) => {
            client.dead = true;
            return (0, 0, 0, 0);
        }
    }
    let (mut pings, mut closes, mut messages, mut reserved) = (0u64, 0u64, 0u64, 0u64);
    loop {
        let Some((fin, rsv1, opcode, payload)) = ws_parse_client_frame(&mut client.ws_in) else {
            break;
        };
        // N558: `permessage-deflate` is never negotiated, so a frame that sets the
        // reserved bit is counted and dropped rather than mis-read as plain data.
        // N567: with `reject_reserved`, it instead fails the connection (1002).
        if rsv1 {
            reserved += 1;
            client.ws_message.clear();
            client.ws_fragmenting = false;
            if reject_reserved {
                client
                    .ws_control
                    .extend_from_slice(&ws_frame_opcode(0x8, &1002u16.to_be_bytes()));
                client.ws_closing = true;
                closes += 1;
            }
            continue;
        }
        match opcode {
            0x9 => {
                // Ping: answer with a pong echoing the payload.
                pings += 1;
                client
                    .ws_control
                    .extend_from_slice(&ws_frame_opcode(0xA, &payload));
            }
            0x8 => {
                closes += 1;
                client
                    .ws_control
                    .extend_from_slice(&ws_frame_opcode(0x8, &payload));
                client.ws_closing = true;
            }
            // N548: a data frame (text/binary) starts or is a whole message.
            0x1 | 0x2 => {
                if fin {
                    messages += 1;
                } else {
                    client.ws_fragmenting = true;
                    client.ws_message_opcode = opcode;
                    client.ws_message.clear();
                    ws_append_fragment(client, &payload);
                }
            }
            // N548: a continuation frame finishes the in-progress message.
            0x0 => {
                if client.ws_fragmenting {
                    ws_append_fragment(client, &payload);
                    if fin {
                        client.ws_fragmenting = false;
                        messages += 1;
                    }
                }
            }
            _ => {}
        }
    }
    (pings, closes, messages, reserved)
}

/// N548: append a continuation payload, keeping the buffer bounded. An overlong
/// message is dropped (the fragmenting state resets) rather than grown.
fn ws_append_fragment(client: &mut HttpClient, payload: &[u8]) {
    const MAX_WS_MESSAGE: usize = 64 * 1024;
    if client.ws_message.len() + payload.len() > MAX_WS_MESSAGE {
        client.ws_message.clear();
        client.ws_fragmenting = false;
        return;
    }
    client.ws_message.extend_from_slice(payload);
}

/// N507: parse one masked client frame from the front of `bytes`, consuming it.
/// Returns `(fin, rsv1, opcode, payload)`, or `None` when more bytes are needed.
/// N548: `fin` distinguishes fragmentation from close. N558: `rsv1` is the
/// reserved (compression) bit.
fn ws_parse_client_frame(bytes: &mut Vec<u8>) -> Option<(bool, bool, u8, Vec<u8>)> {
    if bytes.len() < 2 {
        return None;
    }
    let fin = bytes[0] & 0x80 != 0;
    let rsv1 = bytes[0] & 0x40 != 0;
    let opcode = bytes[0] & 0x0f;
    let masked = bytes[1] & 0x80 != 0;
    let (mut header, payload_len) = match bytes[1] & 0x7f {
        126 => {
            if bytes.len() < 4 {
                return None;
            }
            (
                4usize,
                usize::from(u16::from_be_bytes([bytes[2], bytes[3]])),
            )
        }
        127 => {
            if bytes.len() < 10 {
                return None;
            }
            (
                10usize,
                u64::from_be_bytes(bytes[2..10].try_into().ok()?) as usize,
            )
        }
        len => (2usize, usize::from(len)),
    };
    if masked {
        header += 4;
    }
    if payload_len > 1 << 20 || bytes.len() < header + payload_len {
        return None;
    }
    let mask: [u8; 4] = if masked {
        [
            bytes[header - 4],
            bytes[header - 3],
            bytes[header - 2],
            bytes[header - 1],
        ]
    } else {
        [0, 0, 0, 0]
    };
    let mut payload = bytes[header..header + payload_len].to_vec();
    if masked {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i % 4];
        }
    }
    bytes.drain(..header + payload_len);
    Some((fin, rsv1, opcode, payload))
}

/// N597: the extension token from a `Sec-WebSocket-Extensions` value (before any
/// `;`/`,` parameter), restricted to name characters and capped. This keeps peer
/// data (addresses, window bits) out of the log.
fn extension_name(value: &str) -> String {
    let token = value.split([';', ',']).next().unwrap_or("").trim();
    token
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(32)
        .collect()
}

/// N587: escape a string for inclusion in the hand-built `/health` JSON.
fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn find_header_end(request: &[u8]) -> Option<usize> {
    request.windows(4).position(|w| w == b"\r\n\r\n")
}

/// The path of the first request line (`GET /x HTTP/1.1`), defaults to `/`.
/// N346: the request target split into `(path, query)`.
fn request_target(request: &[u8]) -> (String, String) {
    let line = request.split(|b| *b == b'\n').next().unwrap_or(&[]);
    let line = String::from_utf8_lossy(line);
    let target = line.split_whitespace().nth(1).unwrap_or("/");
    match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target.to_string(), String::new()),
    }
}

/// N346: a numeric query parameter.
fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then_some(value)
    })
}

/// N346: nearest-neighbour integer downscale of a BGRA frame.
fn downscale_bgra(width: u16, height: u16, bgra: &[u8], scale: u16) -> (u16, u16, Vec<u8>) {
    let scale = scale.max(1);
    if scale == 1 {
        return (width, height, bgra.to_vec());
    }
    let w = (width / scale).max(1);
    let h = (height / scale).max(1);
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h {
        for x in 0..w {
            let sx = x as usize * scale as usize;
            let sy = y as usize * scale as usize;
            let src = (sy * width as usize + sx) * 4;
            match bgra.get(src..src + 4) {
                Some(pixel) => out.extend_from_slice(pixel),
                None => out.extend_from_slice(&[0, 0, 0, 255]),
            }
        }
    }
    (w, h, out)
}

/// N346: the frame downscaled by the `?scale=` query (1..=8, default 1).
fn scaled_frame<'a>(frame: &'a ServedFrame, query: &str) -> (u16, u16, std::borrow::Cow<'a, [u8]>) {
    let scale = query_param(query, "scale")
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(1)
        .clamp(1, 8);
    let (w, h, raw): (u16, u16, std::borrow::Cow<'_, [u8]>) = if scale == 1 {
        (
            frame.width,
            frame.height,
            std::borrow::Cow::Borrowed(&frame.bgra),
        )
    } else {
        let (w, h, raw) = downscale_bgra(frame.width, frame.height, &frame.bgra, scale);
        (w, h, std::borrow::Cow::Owned(raw))
    };
    // N459: `?rotate=90|180|270` rotates the served frame.
    let degrees = ((query_param(query, "rotate")
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(0)
        / 90)
        % 4)
        * 90;
    let (w, h, raw) = if degrees == 0 {
        (w, h, raw)
    } else {
        let (rw, rh, rotated) = rotate_bgra(w, h, &raw, degrees);
        (rw, rh, std::borrow::Cow::Owned(rotated))
    };
    // N469: `?crop=x,y,w,h` selects a sub-rectangle of the (scaled/rotated) frame.
    let (w, h, raw) = match query_crop(query) {
        Some((cx, cy, cw, ch)) => {
            let (cw, ch, cropped) = crop_bgra(w, h, &raw, cx, cy, cw, ch);
            (cw, ch, std::borrow::Cow::Owned(cropped))
        }
        None => (w, h, raw),
    };
    // N486: `?flip=h|v|hv` mirrors the frame.
    let flip = query_param(query, "flip").unwrap_or_default();
    let (horizontal, vertical) = (flip.contains('h'), flip.contains('v'));
    if horizontal || vertical {
        return (
            w,
            h,
            std::borrow::Cow::Owned(flip_bgra(w, h, &raw, horizontal, vertical)),
        );
    }
    (w, h, raw)
}

/// N486: mirror a BGRA frame horizontally and/or vertically.
pub fn flip_bgra(
    width: u16,
    height: u16,
    bytes: &[u8],
    horizontal: bool,
    vertical: bool,
) -> Vec<u8> {
    let (w, h) = (usize::from(width), usize::from(height));
    let mut out = vec![0u8; bytes.len()];
    for y in 0..h {
        for x in 0..w {
            let src = (y * w + x) * 4;
            let dx = if horizontal { w - 1 - x } else { x };
            let dy = if vertical { h - 1 - y } else { y };
            let dst = (dy * w + dx) * 4;
            if let (Some(s), Some(d)) = (bytes.get(src..src + 4), out.get_mut(dst..dst + 4)) {
                d.copy_from_slice(s);
            }
        }
    }
    out
}

/// N469: parse `crop=x,y,w,h`.
fn query_crop(query: &str) -> Option<(u16, u16, u16, u16)> {
    let value = query_param(query, "crop")?;
    let parts: Vec<u16> = value
        .split(',')
        .filter_map(|p| p.trim().parse::<u16>().ok())
        .collect();
    (parts.len() == 4 && parts[2] > 0 && parts[3] > 0)
        .then(|| (parts[0], parts[1], parts[2], parts[3]))
}

/// N469: copy a BGRA sub-rectangle (clamped to the source).
fn crop_bgra(
    width: u16,
    height: u16,
    bytes: &[u8],
    x: u16,
    y: u16,
    w: u16,
    h: u16,
) -> (u16, u16, Vec<u8>) {
    let (cw, ch) = (
        w.min(width.saturating_sub(x)),
        h.min(height.saturating_sub(y)),
    );
    if cw == 0 || ch == 0 {
        return (width, height, bytes.to_vec());
    }
    let mut out = vec![0u8; usize::from(cw) * usize::from(ch) * 4];
    for row in 0..ch {
        let src = (usize::from(y + row) * usize::from(width) + usize::from(x)) * 4;
        let dst = usize::from(row) * usize::from(cw) * 4;
        let len = usize::from(cw) * 4;
        if let (Some(s), Some(d)) = (bytes.get(src..src + len), out.get_mut(dst..dst + len)) {
            d.copy_from_slice(s);
        }
    }
    (cw, ch, out)
}

/// N459: rotate a BGRA frame by 90/180/270 degrees clockwise.
fn rotate_bgra(width: u16, height: u16, bytes: &[u8], degrees: u16) -> (u16, u16, Vec<u8>) {
    let (w, h) = (usize::from(width), usize::from(height));
    match degrees % 360 {
        90 => {
            let mut out = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 4;
                    // (x, y) -> (h - 1 - y, x) in the h x w output.
                    let dst = (x * h + (h - 1 - y)) * 4;
                    if let (Some(s), Some(d)) = (bytes.get(src..src + 4), out.get_mut(dst..dst + 4))
                    {
                        d.copy_from_slice(s);
                    }
                }
            }
            (height, width, out)
        }
        180 => {
            let mut out = bytes.to_vec();
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 4;
                    let dst = ((h - 1 - y) * w + (w - 1 - x)) * 4;
                    if let (Some(s), Some(d)) = (bytes.get(src..src + 4), out.get_mut(dst..dst + 4))
                    {
                        d.copy_from_slice(s);
                    }
                }
            }
            (width, height, out)
        }
        270 => {
            let mut out = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 4;
                    // (x, y) -> (y, w - 1 - x) in the h x w output.
                    let dst = ((w - 1 - x) * h + y) * 4;
                    if let (Some(s), Some(d)) = (bytes.get(src..src + 4), out.get_mut(dst..dst + 4))
                    {
                        d.copy_from_slice(s);
                    }
                }
            }
            (height, width, out)
        }
        _ => (width, height, bytes.to_vec()),
    }
}

/// N346: the JPEG `?quality=` query (1..=100, default 80).
fn query_quality(query: &str) -> u8 {
    query_param(query, "quality")
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(80)
        .clamp(1, 100)
}

/// N500: the case-insensitive value of a request header.
fn header_value<'a>(request: &'a [u8], name: &str) -> Option<&'a str> {
    let text = std::str::from_utf8(request).ok()?;
    for line in text.split("\r\n").skip(1) {
        let (key, value) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case(name) {
            return Some(value.trim());
        }
    }
    None
}

/// N500: the RFC 6455 `101 Switching Protocols` response for a client key.
fn ws_handshake(key: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        ws_accept(key)
    )
    .into_bytes()
}

/// N500: the `Sec-WebSocket-Accept` value (base64 of SHA-1(key + GUID)).
fn ws_accept(key: &str) -> String {
    let mut input = key.trim().as_bytes().to_vec();
    input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64_encode(&sha1(&input))
}

/// N500: wrap a payload in an unmasked binary WebSocket frame (server -> client).
fn ws_frame(payload: &[u8]) -> Vec<u8> {
    ws_frame_opcode(0x2, payload)
}

/// N507: wrap a payload in an unmasked WebSocket frame with an explicit opcode.
fn ws_frame_opcode(opcode: u8, payload: &[u8]) -> Vec<u8> {
    ws_frame_part(true, opcode, payload)
}

/// N527: wrap a payload in an unmasked WebSocket frame with an explicit FIN and
/// opcode (used for continuation fragments).
fn ws_frame_part(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(if fin { 0x80 } else { 0x00 } | (opcode & 0x0f));
    match payload.len() {
        len if len < 126 => out.push(len as u8),
        len if len <= usize::from(u16::MAX) => {
            out.push(126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            out.push(127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

/// N500: SHA-1 (FIPS 180-1). Only used for the WebSocket handshake.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, slot) in w.iter_mut().take(16).enumerate() {
            *slot = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, value) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    out
}

/// N500: standard base64 (with padding).
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn respond(last: Option<&ServedFrame>, path: &str, query: &str) -> Served {
    if STREAM_PATHS.contains(&path) {
        // N337: `/stream.jpg` serves JPEG parts; the others serve BMP.
        // N356: `?scale=N` downscales each streamed part.
        let scale = query_param(query, "scale")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(1)
            .clamp(1, 8);
        let format = match path {
            "/stream.jpg" => StreamFormat::Jpeg,
            "/stream.png" => StreamFormat::Png,
            _ => StreamFormat::Bmp,
        };
        return Served::Multipart { format, scale };
    }
    Served::Bytes(match path {
        "/" | "/index.html" => simple_response(
            200,
            "text/html; charset=utf-8",
            b"<!doctype html><title>MyDeviceCanvas mirror</title><h1>mirror</h1>\
              <img src=\"/stream\"><p>live: <code>/ws</code> (WebSocket), <code>/stream</code> (MJPEG)</p>",
        ),
        "/frame.bmp" => match last {
            Some(frame) => {
                let (w, h, raw) = scaled_frame(frame, query);
                simple_response(200, "image/bmp", &encode_bmp(w, h, &raw))
            }
            None => simple_response(503, "text/plain", b"no frame yet"),
        },
        "/frame.ppm" => match last {
            Some(frame) => {
                let (w, h, raw) = scaled_frame(frame, query);
                simple_response(200, "image/x-portable-pixmap", &encode_ppm(w, h, &raw))
            }
            None => simple_response(503, "text/plain", b"no frame yet"),
        },
        "/frame.png" => match last {
            Some(frame) => {
                let (w, h, raw) = scaled_frame(frame, query);
                simple_response(200, "image/png", &encode_png(w, h, &raw))
            }
            None => simple_response(503, "text/plain", b"no frame yet"),
        },
        "/frame.jpg" => match last {
            Some(frame) => {
                let (w, h, raw) = scaled_frame(frame, query);
                simple_response(
                    200,
                    "image/jpeg",
                    &encode_jpeg(w, h, &raw, query_quality(query)),
                )
            }
            None => simple_response(503, "text/plain", b"no frame yet"),
        },
        _ => simple_response(404, "text/plain", b"not found"),
    })
}

fn simple_response(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let mut out = Vec::with_capacity(64 + body.len());
    out.extend_from_slice(
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n\
             Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdc_protocol::{
        encode_frame_begin, encode_frame_chunk, encode_frame_commit, encode_frame_payload, Packet,
        VERSION,
    };
    use mdc_transport::MemoryLink;
    use std::io::Read;
    use std::time::Duration;

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
    fn unknown_mode_is_rejected() {
        assert_eq!(
            MirrorReceiver::new("main", 2, 1, "tiled", MockObsSink::default()).err(),
            Some(MirrorError::Mode("tiled".into()))
        );
    }

    fn begin_chunk_commit(
        receiver: &mut MirrorReceiver<MockObsSink>,
        id: u64,
        offset: usize,
        data: &[u8],
    ) {
        receiver
            .feed(&mirror_packet(
                encode_frame_begin("main", id, 2, 1, 4).unwrap(),
            ))
            .unwrap();
        receiver
            .feed(&mirror_packet(encode_frame_chunk(id, offset as u32, data)))
            .unwrap();
        receiver
            .feed(&mirror_packet(encode_frame_commit(id)))
            .unwrap();
    }

    #[test]
    fn dirty_region_patches_only_the_changed_pixels() {
        let mut receiver =
            MirrorReceiver::new("main", 2, 1, "dirty-region", MockObsSink::default()).unwrap();
        // Base frame: red, green (RGB565 LE).
        receiver
            .feed(&mirror_packet(
                encode_frame_payload("main", 1, 2, 1, &[0x00, 0xF8, 0xE0, 0x07]).unwrap(),
            ))
            .unwrap();
        // Dirty update: patch the second pixel to blue.
        begin_chunk_commit(&mut receiver, 2, 2, &[0x1F, 0x00]);

        let stats = receiver.stats();
        assert_eq!(stats.frames_published, 2);
        assert_eq!(stats.frames_received, 2);
        assert_eq!(stats.frames_dropped, 0);
        // BGRA of the patched frame: red then blue (B at 4, G at 5).
        let last = &receiver.sink().last;
        assert_eq!(
            &last[0..4],
            &[0x00, 0x00, 0xFF, 0xFF],
            "first pixel stays red"
        );
        assert_eq!(
            &last[4..8],
            &[0xFF, 0x00, 0x00, 0xFF],
            "second pixel is blue"
        );
    }

    #[test]
    fn full_frame_mode_drops_a_dirty_commit() {
        let mut receiver =
            MirrorReceiver::new("main", 2, 1, "full-frame", MockObsSink::default()).unwrap();
        receiver
            .feed(&mirror_packet(
                encode_frame_payload("main", 1, 2, 1, &[0x00, 0xF8, 0xE0, 0x07]).unwrap(),
            ))
            .unwrap();
        begin_chunk_commit(&mut receiver, 2, 2, &[0x1F, 0x00]);
        let stats = receiver.stats();
        assert_eq!(stats.frames_published, 1, "the dirty update is refused");
        assert_eq!(stats.frames_dropped, 1);
        assert_eq!(
            &receiver.sink().last[4..8],
            &[0x00, 0xFF, 0x00, 0xFF],
            "pixels are unchanged"
        );
    }

    #[test]
    fn dirty_before_base_and_misaligned_ranges_are_dropped() {
        let mut receiver =
            MirrorReceiver::new("main", 2, 1, "dirty-region", MockObsSink::default()).unwrap();
        // No base frame yet → dropped.
        begin_chunk_commit(&mut receiver, 1, 2, &[0x1F, 0x00]);
        assert_eq!(receiver.stats().frames_published, 0);
        // Establish a base.
        receiver
            .feed(&mirror_packet(
                encode_frame_payload("main", 2, 2, 1, &[0x00, 0xF8, 0xE0, 0x07]).unwrap(),
            ))
            .unwrap();
        // A one-byte range splits a pixel → dropped.
        begin_chunk_commit(&mut receiver, 3, 1, &[0x1F]);
        let stats = receiver.stats();
        assert_eq!(stats.frames_published, 1);
        assert_eq!(stats.frames_dropped, 2);
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
    fn ppm_directory_sink_writes_atomically_and_rotates() {
        let dir = std::env::temp_dir().join(format!(
            "mdc-ppm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pixels = vec![0, 0, 255, 255, 0, 255, 0, 255]; // 2x1 BGRA
        fn frame<'a>(id: u64, pixels: &'a [u8]) -> ShadowFrame<'a> {
            ShadowFrame {
                width: 2,
                height: 1,
                generation: id,
                frame_id: id,
                pixels,
            }
        }
        let mut sink = PpmDirectorySink::new(&dir, "frame.ppm", 0);
        sink.on_frame(frame(1, &pixels));
        assert_eq!(sink.frames_written(), 1);
        assert!(sink.last_error().is_none());
        let written = std::fs::read(dir.join("frame.ppm")).unwrap();
        assert_eq!(&written[..11], b"P6\n2 1\n255\n");
        assert!(!dir.join("frame.tmp").exists(), "temp file is renamed");

        let mut ring = PpmDirectorySink::new(&dir, "ring.ppm", 2);
        for id in 1..=3 {
            ring.on_frame(frame(id, &pixels));
        }
        assert_eq!(ring.frames_written(), 3);
        assert!(dir.join("ring_0000.ppm").exists());
        assert!(dir.join("ring_0001.ppm").exists());
        let _ = std::fs::remove_dir_all(&dir);
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

    fn accept_one(server: &mut MirrorServer) {
        for _ in 0..200 {
            if server.accept_pending().unwrap() >= 1 {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("no loopback client was accepted");
    }

    #[test]
    fn mirror_server_fans_out_to_a_loopback_client() {
        let mut server = MirrorServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        accept_one(&mut server);
        assert_eq!(server.client_count(), 1);

        let pixels = vec![0u8, 0, 255, 255]; // 1x1 BGRA red
        server.publish(&ShadowFrame {
            width: 1,
            height: 1,
            generation: 1,
            frame_id: 1,
            pixels: &pixels,
        });
        server.flush();

        let mut header = [0u8; 4];
        client.read_exact(&mut header).unwrap();
        let len = u32::from_be_bytes(header) as usize;
        let mut body = vec![0u8; len];
        client.read_exact(&mut body).unwrap();
        assert_eq!(&body[..11], b"P6\n1 1\n255\n");
        assert_eq!(server.frames_published(), 1);
        assert_eq!(server.frames_dropped(), 0);
    }

    #[test]
    fn mirror_server_keeps_only_the_newest_frame_for_a_slow_client() {
        let mut server = MirrorServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let _client = std::net::TcpStream::connect(addr).unwrap();
        accept_one(&mut server);

        let pixels = vec![0u8, 0, 0, 255];
        let frame = ShadowFrame {
            width: 1,
            height: 1,
            generation: 1,
            frame_id: 1,
            pixels: &pixels,
        };
        server.publish(&frame);
        server.publish(&frame); // not flushed yet → replaces the pending frame
        assert_eq!(server.frames_published(), 2);
        assert_eq!(server.frames_dropped(), 1);
        server.flush();
    }

    #[test]
    fn bmp_encoder_writes_a_valid_header_and_rows() {
        let pixels = vec![0u8, 0, 255, 255]; // 1x1 BGRA red
        let bmp = encode_bmp(1, 1, &pixels);
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()), 54 + 4);
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);
        assert_eq!(i32::from_le_bytes(bmp[18..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(bmp[28..30].try_into().unwrap()), 24);
        // 1 pixel row padded to 4 bytes; red -> BGR [0, 0, 255].
        assert_eq!(&bmp[54..57], &[0, 0, 255]);
        assert_eq!(bmp.len(), 58);
    }

    /// Send one request and return the raw response, pumping the server.
    fn http_get(server: &mut HttpMirrorServer, path: &str) -> Vec<u8> {
        let addr = server.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .unwrap();
        let mut response = Vec::new();
        for _ in 0..400 {
            let _ = server.pump();
            let mut buffer = [0u8; 4096];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => response.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        response
    }

    #[test]
    fn http_server_serves_the_latest_frame_and_routes() {
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let pixels = vec![0u8, 0, 255, 255]; // 1x1 BGRA red
        server.publish(&ShadowFrame {
            width: 1,
            height: 1,
            generation: 1,
            frame_id: 1,
            pixels: &pixels,
        });
        assert!(server.has_frame());

        let bmp = http_get(&mut server, "/frame.bmp");
        let head = String::from_utf8_lossy(&bmp);
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
        assert!(head.contains("Content-Type: image/bmp"));
        let body = bmp.windows(2).position(|w| w == b"BM").expect("BMP body");
        assert_eq!(&bmp[body..body + 2], b"BM");

        let page = String::from_utf8_lossy(&http_get(&mut server, "/")).to_string();
        assert!(page.contains("text/html"));
        let missing = String::from_utf8_lossy(&http_get(&mut server, "/nope")).to_string();
        assert!(missing.contains("404 Not Found"), "{missing}");
        assert_eq!(server.frames_published(), 1);

        // N328: PNG is served too.
        let png = http_get(&mut server, "/frame.png");
        let png_head = String::from_utf8_lossy(&png);
        assert!(png_head.starts_with("HTTP/1.1 200 OK"), "{png_head}");
        assert!(png_head.contains("Content-Type: image/png"));
        assert!(png
            .windows(8)
            .any(|w| w == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]));

        // N336: JPEG is served for the smallest OBS delivery.
        let jpg = http_get(&mut server, "/frame.jpg");
        let jpg_head = String::from_utf8_lossy(&jpg);
        assert!(jpg_head.starts_with("HTTP/1.1 200 OK"), "{jpg_head}");
        assert!(jpg_head.contains("Content-Type: image/jpeg"));
        assert!(
            jpg.windows(2).any(|w| w == [0xFF, 0xD8]),
            "JPEG body present"
        );
    }

    #[test]
    fn http_health_reports_counters() {
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let pixels = vec![0u8; 4];
        server.publish(&ShadowFrame {
            width: 1,
            height: 1,
            generation: 1,
            frame_id: 1,
            pixels: &pixels,
        });
        let health = String::from_utf8_lossy(&http_get(&mut server, "/health")).to_string();
        assert!(health.starts_with("HTTP/1.1 200 OK"), "{health}");
        assert!(health.contains("application/json"));
        assert!(health.contains("\"frames\":1"), "{health}");
        // N500: the WebSocket consumer count is reported too.
        assert!(health.contains("\"ws\":0"), "{health}");
    }

    #[test]
    fn http_scale_query_downscales_and_quality_parses() {
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let pixels = vec![0x40u8; 4 * 4 * 4]; // 4x4 BGRA
        server.publish(&ShadowFrame {
            width: 4,
            height: 4,
            generation: 1,
            frame_id: 1,
            pixels: &pixels,
        });
        // scale=2 -> a 2x2 BMP.
        let response = http_get(&mut server, "/frame.bmp?scale=2");
        let start = response
            .windows(2)
            .position(|w| w == b"BM")
            .expect("BMP body");
        let width = u32::from_le_bytes(response[start + 18..start + 22].try_into().unwrap());
        let height = u32::from_le_bytes(response[start + 22..start + 26].try_into().unwrap());
        assert_eq!((width, height), (2, 2));
        // Query parsing.
        assert_eq!(query_param("a=1&scale=3&quality=50", "scale"), Some("3"));
        assert_eq!(query_param("a=1&quality=50", "quality"), Some("50"));
        assert_eq!(query_param("a=1", "scale"), None);
        // The JPEG endpoint still serves with a quality hint.
        let jpg = http_get(&mut server, "/frame.jpg?quality=20");
        assert!(jpg.windows(2).any(|w| w == [0xFF, 0xD8]), "JPEG body");
    }

    #[test]
    fn sha1_and_base64_match_the_rfc_vectors() {
        // N500: the WebSocket handshake primitives.
        assert_eq!(base64_encode(b"abc"), "YWJj");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        let digest = sha1(b"abc");
        assert_eq!(
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        // RFC 6455 section 1.3 example.
        assert_eq!(
            ws_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        // The binary frame header: FIN + opcode 0x82, then the length.
        assert_eq!(ws_frame(b"hi"), vec![0x82, 0x02, b'h', b'i']);
        let long = ws_frame(&[0u8; 200]);
        assert_eq!(&long[..4], &[0x82, 126, 0x00, 0xc8]);
    }

    #[test]
    fn websocket_fragments_a_large_message() {
        // N527: a message over the fragment size is split into a first binary
        // frame (FIN=0) plus continuation frames, the last with FIN=1.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0")
            .unwrap()
            .with_ws_fragment(64);
        let addr = server.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        client
            .write_all(
                b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .unwrap();
        let mut seen = Vec::new();
        for _ in 0..100 {
            let _ = server.pump();
            let mut buffer = [0u8; 4096];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => seen.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
            if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        // A noisy 32x32 frame compresses to well over 64 bytes.
        let pixels: Vec<u8> = (0..32 * 32 * 4).map(|i| ((i * 53) % 251) as u8).collect();
        server.publish(&ShadowFrame {
            width: 32,
            height: 32,
            generation: 1,
            frame_id: 1,
            pixels: &pixels,
        });
        // Read until a complete message (a final fragment) has arrived.
        let parse = |body: &[u8]| -> (Vec<(bool, u8, Vec<u8>)>, bool) {
            let mut offset = 0usize;
            let mut fragments = Vec::new();
            let mut complete = false;
            while offset + 2 <= body.len() {
                let fin = body[offset] & 0x80 != 0;
                let opcode = body[offset] & 0x0f;
                let (header, len) = match body[offset + 1] {
                    126 if offset + 4 <= body.len() => (
                        4,
                        usize::from(u16::from_be_bytes([body[offset + 2], body[offset + 3]])),
                    ),
                    126 => break,
                    len => (2, usize::from(len)),
                };
                if offset + header + len > body.len() {
                    break;
                }
                fragments.push((
                    fin,
                    opcode,
                    body[offset + header..offset + header + len].to_vec(),
                ));
                offset += header + len;
                if fin {
                    complete = true;
                    break;
                }
            }
            (fragments, complete)
        };
        let mut body = seen[seen.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4..].to_vec();
        let mut complete = parse(&body).1;
        for _ in 0..400 {
            if complete {
                break;
            }
            let _ = server.pump();
            let mut buffer = [0u8; 8192];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
            complete = parse(&body).1;
        }
        assert!(complete, "the whole message arrived");
        let (fragments, _) = parse(&body);
        assert!(fragments.len() >= 2, "the message was split");
        assert_eq!(fragments[0].1, 0x2, "first fragment is binary");
        assert!(!fragments[0].0, "the first fragment is not final");
        for (_, opcode, payload) in &fragments[1..] {
            assert_eq!(*opcode, 0x0, "continuation opcode");
            assert!(payload.len() <= 64, "fragments respect the size");
        }
        assert!(fragments.last().unwrap().0, "the last fragment is final");
        // Reassembled, it is the JPEG the server published.
        let mut reassembled = Vec::new();
        for (_, _, payload) in &fragments {
            reassembled.extend_from_slice(payload);
        }
        assert_eq!(&reassembled[..2], &[0xFF, 0xD8], "a JPEG was reassembled");
        // Without a fragment size the whole message is one frame.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        server.publish(&ShadowFrame {
            width: 4,
            height: 4,
            generation: 1,
            frame_id: 1,
            pixels: &pixels[..64],
        });
        assert_eq!(server.frames_published(), 1);
    }

    #[test]
    fn health_reports_the_note_kinds() {
        // N657: `/health` carries the per-kind summary as a JSON object.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        server.note_ws("reserved", "ignored");
        server.note_ws("reserved", "ignored");
        server.note_ws("fragment", "continued");
        let health = String::from_utf8_lossy(&http_get(&mut server, "/health")).to_string();
        assert!(
            health.contains("\"ws_log_kinds\":{\"fragment\":1,\"reserved\":2}"),
            "{health}"
        );
        // The entry-count field stays a number.
        assert!(health.contains("\"ws_log\":3"), "{health}");
        // Escaping: a hostile kind cannot break the JSON.
        server.note_ws("we\"ird\\kind", "x");
        let health = String::from_utf8_lossy(&http_get(&mut server, "/health")).to_string();
        assert!(health.contains("\"we\\\"ird\\\\kind\":1"), "{health}");
    }

    #[test]
    fn websocket_log_counts_summarize_each_kind() {
        // N647: the per-kind summary covers exactly the retained notes.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        for _ in 0..3 {
            server.note_ws("reserved", "ignored");
        }
        for _ in 0..2 {
            server.note_ws("fragment", "continued");
        }
        let counts = server.ws_log_counts();
        assert_eq!(counts.get("reserved"), Some(&3));
        assert_eq!(counts.get("fragment"), Some(&2));
        assert_eq!(
            counts.values().sum::<usize>(),
            server.ws_log().len(),
            "the summary matches the log"
        );
        assert!(!server.ws_log_counts().contains_key("timeout"));
    }

    #[test]
    fn websocket_note_details_are_bounded() {
        // N637: a very long detail is truncated to the exported cap.
        assert_eq!(WS_NOTE_DETAIL_MAX, 256);
        let long = "x".repeat(1000);
        let clamped = truncate_detail(&long);
        assert_eq!(clamped.len(), WS_NOTE_DETAIL_MAX);
        // A multi-byte string is cut on a char boundary, not mid-codepoint.
        let multibyte = "€".repeat(200); // 3 bytes each
        let clamped = truncate_detail(&multibyte);
        assert!(clamped.len() <= WS_NOTE_DETAIL_MAX);
        assert!(clamped.is_char_boundary(clamped.len()));
        assert!(multibyte.starts_with(&clamped));
        // Short details pass through untouched.
        assert_eq!(truncate_detail("closed 1002"), "closed 1002");
        // The log itself stays bounded.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        for i in 0..40 {
            server.note_ws("reserved", format!("{i}:{}", "y".repeat(500)));
        }
        assert!(server.ws_log().len() <= 16);
        assert!(server
            .ws_log()
            .iter()
            .all(|note| note.detail.len() <= WS_NOTE_DETAIL_MAX));
    }

    #[test]
    fn websocket_fragments_and_heartbeat_do_not_interleave() {
        // N537: a heartbeat must not inject a control frame inside a fragmented
        // message, and a tiny fragment size is raised to the floor.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0")
            .unwrap()
            .with_ws_heartbeat(1, 1000)
            .with_ws_fragment(1);
        let addr = server.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        client
            .write_all(
                b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .unwrap();
        let mut seen = Vec::new();
        for _ in 0..100 {
            let _ = server.pump();
            let mut buffer = [0u8; 4096];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => seen.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
            if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let pixels: Vec<u8> = (0..64 * 64 * 4).map(|i| ((i * 41) % 251) as u8).collect();
        server.publish(&ShadowFrame {
            width: 64,
            height: 64,
            generation: 1,
            frame_id: 1,
            pixels: &pixels,
        });
        // Read until the message's final fragment.
        let parse = |body: &[u8]| -> (Vec<(bool, u8, Vec<u8>)>, bool) {
            let mut offset = 0usize;
            let mut fragments = Vec::new();
            let mut complete = false;
            while offset + 2 <= body.len() {
                let fin = body[offset] & 0x80 != 0;
                let opcode = body[offset] & 0x0f;
                let (header, len) = match body[offset + 1] {
                    126 if offset + 4 <= body.len() => (
                        4,
                        usize::from(u16::from_be_bytes([body[offset + 2], body[offset + 3]])),
                    ),
                    126 => break,
                    len => (2, usize::from(len)),
                };
                if offset + header + len > body.len() {
                    break;
                }
                fragments.push((
                    fin,
                    opcode,
                    body[offset + header..offset + header + len].to_vec(),
                ));
                offset += header + len;
                if fin {
                    complete = true;
                    break;
                }
            }
            (fragments, complete)
        };
        let mut body = seen[seen.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4..].to_vec();
        let mut complete = parse(&body).1;
        for _ in 0..400 {
            if complete {
                break;
            }
            let _ = server.pump();
            let mut buffer = [0u8; 8192];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
            complete = parse(&body).1;
        }
        assert!(complete, "the fragmented message completed");
        let (fragments, _) = parse(&body);
        // The floor was applied: every non-final fragment is at least
        // MIN_WS_FRAGMENT (the last one is the remainder).
        let last = fragments.len().saturating_sub(1);
        for (index, (_, opcode, payload)) in fragments.iter().enumerate() {
            if (*opcode == 0x2 || *opcode == 0x0) && index < last {
                assert!(
                    payload.len() >= MIN_WS_FRAGMENT,
                    "fragment below the floor: {}",
                    payload.len()
                );
            }
        }
        let mut reassembled = Vec::new();
        for (_, opcode, payload) in &fragments {
            if *opcode == 0x2 || *opcode == 0x0 {
                reassembled.extend_from_slice(payload);
            }
        }
        assert_eq!(&reassembled[..2], &[0xFF, 0xD8], "a clean JPEG message");
        assert!(server.ws_pings_sent() >= 1, "a heartbeat was queued");
    }

    #[test]
    fn websocket_heartbeat_pings_and_drops_a_silent_peer() {
        // N517: the server pings on an interval and drops a silent peer.
        fn upgrade(server: &mut HttpMirrorServer) -> std::net::TcpStream {
            let addr = server.local_addr().unwrap();
            let mut client = std::net::TcpStream::connect(addr).unwrap();
            client
                .set_read_timeout(Some(Duration::from_millis(20)))
                .unwrap();
            client
                .write_all(
                    b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                      Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
                )
                .unwrap();
            for _ in 0..100 {
                let _ = server.pump();
                let mut buffer = [0u8; 4096];
                match client.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
                if server.ws_clients() == 1 {
                    break;
                }
            }
            client
        }
        let pixels = vec![0x40u8; 4 * 4 * 4];
        let publish = |server: &mut HttpMirrorServer, frame_id: u64| {
            server.publish(&ShadowFrame {
                width: 4,
                height: 4,
                generation: frame_id,
                frame_id,
                pixels: &pixels,
            });
            let _ = server.pump();
        };

        // A silent peer: pings are sent, then it is dropped.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0")
            .unwrap()
            .with_ws_heartbeat(2, 4);
        let _client = upgrade(&mut server);
        for frame in 0..20 {
            publish(&mut server, frame);
        }
        assert!(
            server.ws_pings_sent() >= 2,
            "pings sent: {}",
            server.ws_pings_sent()
        );
        assert!(server.ws_timeouts() >= 1, "silent peer dropped");
        assert_eq!(server.ws_clients(), 0);

        // A peer that answers keeps its connection.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0")
            .unwrap()
            .with_ws_heartbeat(2, 4);
        let mut client = upgrade(&mut server);
        let pong = {
            let mask = [0x01u8, 0x02, 0x03, 0x04];
            let payload = b"p";
            let mut frame = vec![0x8A, 0x80 | payload.len() as u8];
            frame.extend_from_slice(&mask);
            frame.extend(
                payload
                    .iter()
                    .enumerate()
                    .map(|(i, byte)| byte ^ mask[i % 4]),
            );
            frame
        };
        for frame in 0..20 {
            publish(&mut server, frame);
            // Read whatever the server sent (ping/frame) and answer with a pong.
            let mut buffer = [0u8; 4096];
            let _ = client.read(&mut buffer);
            client.write_all(&pong).unwrap();
            let _ = server.pump();
        }
        assert_eq!(server.ws_timeouts(), 0, "an answering peer is kept");
        assert_eq!(server.ws_clients(), 1);
    }

    #[test]
    fn websocket_answers_ping_and_close() {
        // N507: a client ping is answered with a pong; a close drops the client.
        fn client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
            let mask = [0x12u8, 0x34, 0x56, 0x78];
            let mut out = vec![0x80 | opcode, 0x80 | payload.len() as u8];
            out.extend_from_slice(&mask);
            out.extend(
                payload
                    .iter()
                    .enumerate()
                    .map(|(i, byte)| byte ^ mask[i % 4]),
            );
            out
        }
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        client
            .write_all(
                b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .unwrap();
        let mut seen = Vec::new();
        for _ in 0..100 {
            let _ = server.pump();
            let mut buffer = [0u8; 4096];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => seen.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
            if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        // A ping (with a payload) is answered by a pong that echoes it.
        client.write_all(&client_frame(0x9, b"hi")).unwrap();
        let mut reply = Vec::new();
        for _ in 0..100 {
            let _ = server.pump();
            let mut buffer = [0u8; 256];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => reply.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
            if reply.len() >= 4 {
                break;
            }
        }
        assert_eq!(server.ws_pings(), 1);
        assert_eq!(reply[0], 0x8A, "pong opcode");
        assert_eq!(&reply[2..4], b"hi", "pong echoes the ping payload");
        // A close frame ends the connection.
        client.write_all(&client_frame(0x8, &[])).unwrap();
        for _ in 0..100 {
            let _ = server.pump();
            if server.ws_clients() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(server.ws_closes(), 1);
        assert_eq!(server.ws_clients(), 0, "the client was dropped");
    }

    #[test]
    fn websocket_reassembles_fragmented_client_messages() {
        // N548: a client text message split across a first frame and a
        // continuation is reassembled and counted once.
        fn client_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
            let mask = [0x21u8, 0x43, 0x65, 0x87];
            let first = if fin { 0x80 } else { 0x00 } | opcode;
            let mut out = vec![first, 0x80 | payload.len() as u8];
            out.extend_from_slice(&mask);
            out.extend(
                payload
                    .iter()
                    .enumerate()
                    .map(|(i, byte)| byte ^ mask[i % 4]),
            );
            out
        }
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        client
            .write_all(
                b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .unwrap();
        let mut seen = Vec::new();
        for _ in 0..100 {
            let _ = server.pump();
            let mut buffer = [0u8; 4096];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => seen.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
            if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        assert_eq!(server.ws_clients(), 1);

        // A fragmented text message reassembles into one complete message.
        client.write_all(&client_frame(false, 0x1, b"hel")).unwrap();
        client.write_all(&client_frame(true, 0x0, b"lo")).unwrap();
        for _ in 0..100 {
            let _ = server.pump();
            if server.ws_messages() == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(server.ws_messages(), 1, "one reassembled message");

        // A ping interleaved between the fragments is answered and does not break
        // reassembly; the follow-up message counts too.
        client.write_all(&client_frame(false, 0x2, b"AB")).unwrap();
        client.write_all(&client_frame(true, 0x9, b"x")).unwrap();
        client.write_all(&client_frame(true, 0x0, b"CD")).unwrap();
        for _ in 0..100 {
            let _ = server.pump();
            if server.ws_messages() == 2 && server.ws_pings() == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(server.ws_messages(), 2);
        assert_eq!(
            server.ws_pings(),
            1,
            "the ping between fragments was answered"
        );
    }

    #[test]
    fn websocket_ignores_reserved_compression_bit() {
        // N558: a frame with RSV1 set (permessage-deflate) is counted, not read
        // as plain data, since the server never negotiates compression.
        fn upgrade(server: &mut HttpMirrorServer) -> std::net::TcpStream {
            let addr = server.local_addr().unwrap();
            let mut client = std::net::TcpStream::connect(addr).unwrap();
            client
                .set_read_timeout(Some(Duration::from_millis(20)))
                .unwrap();
            client
                .write_all(
                    b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                      Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
                )
                .unwrap();
            for _ in 0..100 {
                let _ = server.pump();
                let mut buffer = [0u8; 4096];
                match client.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
                if server.ws_clients() == 1 {
                    break;
                }
            }
            client
        }
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let mut client = upgrade(&mut server);
        // fin + rsv1 + binary, masked, two bytes "hi".
        let mask = [0x01u8, 0x02, 0x03, 0x04];
        let payload = b"hi";
        let mut frame = vec![0x80 | 0x40 | 0x02, 0x80 | payload.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(i, byte)| byte ^ mask[i % 4]),
        );
        client.write_all(&frame).unwrap();
        for _ in 0..100 {
            let _ = server.pump();
            if server.ws_reserved() == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(server.ws_reserved(), 1, "the reserved bit was recorded");
        assert_eq!(
            server.ws_messages(),
            0,
            "a compressed frame is not a message"
        );
        assert_eq!(server.ws_clients(), 1, "the peer stays connected");
    }

    #[test]
    fn websocket_declines_extensions_and_can_reject_reserved_bit() {
        // N567: an extension offer is counted but never echoed; with
        // `with_ws_reject_reserved` a reserved-bit frame closes with 1002.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0")
            .unwrap()
            .with_ws_reject_reserved(true);
        let addr = server.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        client
            .write_all(
                b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                  Sec-WebSocket-Extensions: permessage-deflate\r\n\r\n",
            )
            .unwrap();
        let mut handshake = Vec::new();
        for _ in 0..100 {
            let _ = server.pump();
            let mut buffer = [0u8; 4096];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    handshake.extend_from_slice(&buffer[..n]);
                    if handshake.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        let text = String::from_utf8_lossy(&handshake);
        assert!(text.starts_with("HTTP/1.1 101"), "upgraded: {text}");
        assert!(
            !text.to_ascii_lowercase().contains("permessage-deflate"),
            "the extension is not echoed"
        );
        assert_eq!(server.ws_offered(), 1, "the offer was counted");
        // A reserved-bit frame now fails the connection with 1002.
        let mask = [0x11u8, 0x22, 0x33, 0x44];
        let payload = b"x";
        let mut frame = vec![0x80 | 0x40 | 0x02, 0x80 | payload.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(i, byte)| byte ^ mask[i % 4]),
        );
        client.write_all(&frame).unwrap();
        let mut close = Vec::new();
        for _ in 0..100 {
            let _ = server.pump();
            let mut buffer = [0u8; 64];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => close.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
            if close.len() >= 4 {
                break;
            }
        }
        assert_eq!(server.ws_reserved(), 1);
        assert!(server.ws_closes() >= 1, "a close was sent");
        assert_eq!(close[0], 0x88, "close opcode");
        assert_eq!(u16::from_be_bytes([close[2], close[3]]), 1002);
        // N577/N607: the decline reason is logged, structured by kind.
        assert!(server
            .ws_log()
            .iter()
            .any(|note| note.kind == "extension" && note.detail == "permessage-deflate"));
        assert!(server
            .ws_log()
            .iter()
            .any(|note| note.kind == "reserved" && note.detail == "closed 1002"));

        // N587: `/health` carries an escaped summary of the latest note.
        let mut health = std::net::TcpStream::connect(addr).unwrap();
        health
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        health
            .write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut body = Vec::new();
        for _ in 0..100 {
            let _ = server.pump();
            let mut buffer = [0u8; 4096];
            match health.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
            if body.windows(4).any(|w| w == b"\r\n\r\n") && body.len() > 200 {
                break;
            }
        }
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("\"ws_log_last\""), "{text}");
        assert!(text.contains("1002"), "{text}");
    }

    #[test]
    fn websocket_log_redacts_extension_parameters() {
        // N597: only the extension token is logged, never its parameters.
        assert_eq!(
            extension_name("permessage-deflate; server_max_window_bits=15"),
            "permessage-deflate"
        );
        assert_eq!(extension_name("x-custom; peer=1.2.3.4:9999"), "x-custom");
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        client
            .write_all(
                b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                  Sec-WebSocket-Extensions: permessage-deflate; peer=10.0.0.7:4242\r\n\r\n",
            )
            .unwrap();
        for _ in 0..100 {
            let _ = server.pump();
            if !server.ws_log().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // N607: the entry is structured and its detail is the bare token.
        let note = &server.ws_log()[0];
        assert_eq!(note.kind, "extension");
        assert_eq!(note.detail, "permessage-deflate");
        // N617: the log is filterable by kind and carries a shape version.
        assert_eq!(WS_NOTE_VERSION, 1);
        assert_eq!(server.ws_log_of_kind("extension").count(), 1);
        assert_eq!(server.ws_log_of_kind("reserved").count(), 0);
        assert!(
            !note.detail.contains("10.0.0.7"),
            "the peer address is not logged"
        );
        assert!(
            !note.detail.contains("4242"),
            "extension parameters are not logged"
        );
    }

    #[test]
    fn websocket_endpoint_streams_binary_frames() {
        // N500: `/ws` upgrades and then pushes one binary message per frame.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .write_all(
                b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .unwrap();
        // Drive the handshake.
        let mut handshake = Vec::new();
        for _ in 0..200 {
            let _ = server.pump();
            let mut buffer = [0u8; 4096];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    handshake.extend_from_slice(&buffer[..n]);
                    if handshake.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        let text = String::from_utf8_lossy(&handshake);
        assert!(text.starts_with("HTTP/1.1 101"), "upgraded: {text}");
        assert!(text.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
        assert_eq!(server.ws_clients(), 1);
        // Publish a frame; the client receives a binary message.
        let pixels = vec![0x40u8; 4 * 4 * 4];
        server.publish(&ShadowFrame {
            width: 4,
            height: 4,
            generation: 1,
            frame_id: 1,
            pixels: &pixels,
        });
        // Read a complete WebSocket frame: header length then payload.
        let frame_header = |message: &[u8]| -> Option<usize> {
            if message.len() < 2 {
                return None;
            }
            let (header, payload) = match message[1] {
                126 if message.len() >= 4 => {
                    (4, usize::from(u16::from_be_bytes([message[2], message[3]])))
                }
                127 if message.len() >= 10 => (
                    10,
                    u64::from_be_bytes(message[2..10].try_into().unwrap()) as usize,
                ),
                126 | 127 => return None,
                len => (2, usize::from(len)),
            };
            (message.len() >= header + payload).then_some(header)
        };
        let mut message = Vec::new();
        let mut header = None;
        for _ in 0..400 {
            let _ = server.pump();
            let mut buffer = [0u8; 8192];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => message.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
            if let Some(found) = frame_header(&message) {
                header = Some(found);
                break;
            }
        }
        let header = header.expect("a complete WebSocket frame");
        assert_eq!(message[0], 0x82, "binary frame");
        assert_eq!(
            &message[header..header + 2],
            &[0xFF, 0xD8],
            "the payload is a JPEG"
        );
    }

    #[test]
    fn http_flip_query_mirrors_the_frame() {
        // N486: `?flip=h` swaps the columns of the served frame.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let pixels = vec![1u8, 2, 3, 255, 4, 5, 6, 255]; // 2x1 BGRA
        server.publish(&ShadowFrame {
            width: 2,
            height: 1,
            generation: 1,
            frame_id: 1,
            pixels: &pixels,
        });
        let body = |response: &[u8]| {
            let start = response
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .expect("HTTP body")
                + 4;
            response[start..].to_vec()
        };
        let plain = body(&http_get(&mut server, "/frame.ppm"));
        let flipped = body(&http_get(&mut server, "/frame.ppm?flip=h"));
        let header = b"P6\n2 1\n255\n".len();
        assert_eq!(&flipped[header..header + 3], &plain[header + 3..header + 6]);
        assert_eq!(&flipped[header + 3..header + 6], &plain[header..header + 3]);
        // A vertical flip of a single row is a no-op.
        let vflip = body(&http_get(&mut server, "/frame.ppm?flip=v"));
        assert_eq!(&vflip[header..], &plain[header..]);
    }

    #[test]
    fn png_encoder_emits_a_valid_structure() {
        let bgra = vec![0u8, 0, 255, 255]; // 1x1 BGRA red
        let png = encode_png(1, 1, &bgra);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert!(png.windows(4).any(|w| w == b"IHDR"));
        assert!(png.windows(4).any(|w| w == b"IDAT"));
        assert!(
            png.ends_with(b"IEND\xae\x42\x60\x82"),
            "ends with a valid IEND"
        );
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 1);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 1);
        assert_eq!(png[24], 8, "8-bit depth");
        assert_eq!(png[25], 6, "RGBA colour type");
        // Zero-size input is refused rather than panicking.
        assert!(encode_png(0, 4, &[]).is_empty());
    }

    /// N313: pump the server and drain whatever the streaming client can read.
    fn drain(server: &mut HttpMirrorServer, client: &mut TcpStream, out: &mut Vec<u8>) {
        for _ in 0..30 {
            let _ = server.pump();
            let mut buffer = [0u8; 8192];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buffer[..n]),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }

    #[test]
    fn http_live_stream_pushes_multipart_parts() {
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        client
            .write_all(b"GET /stream HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        // The client connects before any frame: it still gets the preamble.
        std::thread::sleep(Duration::from_millis(20));
        let mut response = Vec::new();
        drain(&mut server, &mut client, &mut response);
        assert_eq!(server.streaming_clients(), 1);
        let head = String::from_utf8_lossy(&response);
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
        assert!(head.contains("multipart/x-mixed-replace; boundary=mdcframe"));

        let red = vec![0u8, 0, 255, 255]; // 1x1 BGRA red
        let green = vec![0u8, 255, 0, 255];
        let blue = vec![255u8, 0, 0, 255];
        for (frame_id, pixels) in [(1u64, &red), (2, &green), (3, &blue)] {
            server.publish(&ShadowFrame {
                width: 1,
                height: 1,
                generation: frame_id,
                frame_id,
                pixels,
            });
            drain(&mut server, &mut client, &mut response);
        }
        // Each published frame is a complete multipart part ending in a boundary.
        let boundaries = response.windows(10).filter(|w| *w == b"--mdcframe").count();
        assert!(boundaries >= 3, "expected >=3 parts, got {boundaries}");
        assert!(
            response.windows(2).any(|w| w == b"BM"),
            "a BMP body must be streamed"
        );
        assert_eq!(server.frames_published(), 3);
    }

    #[test]
    fn http_jpeg_stream_pushes_jpeg_parts() {
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        client
            .write_all(b"GET /stream.jpg HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let red = vec![0u8, 0, 255, 255]; // 1x1 BGRA red
        server.publish(&ShadowFrame {
            width: 1,
            height: 1,
            generation: 1,
            frame_id: 1,
            pixels: &red,
        });
        let mut response = Vec::new();
        drain(&mut server, &mut client, &mut response);
        let head = String::from_utf8_lossy(&response);
        assert!(
            head.contains("multipart/x-mixed-replace; boundary=mdcframe"),
            "{head}"
        );
        assert!(head.contains("Content-Type: image/jpeg"));
        assert!(
            response.windows(2).any(|w| w == [0xFF, 0xD8]),
            "a JPEG SOI is streamed"
        );
    }

    #[test]
    fn http_stream_scale_downscales_parts() {
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        client
            .write_all(b"GET /stream.bmp?scale=2 HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let pixels = vec![0x40u8; 2 * 2 * 4]; // 2x2 BGRA
        server.publish(&ShadowFrame {
            width: 2,
            height: 2,
            generation: 1,
            frame_id: 1,
            pixels: &pixels,
        });
        let mut response = Vec::new();
        drain(&mut server, &mut client, &mut response);
        let start = response
            .windows(2)
            .position(|w| w == b"BM")
            .expect("BMP part");
        let width = u32::from_le_bytes(response[start + 18..start + 22].try_into().unwrap());
        assert_eq!(width, 1, "the streamed part is downscaled");
    }

    #[test]
    fn flip_bgra_mirrors_axes() {
        // N486: horizontal flip swaps the columns; vertical swaps the rows.
        let bytes: Vec<u8> = (1u8..=16).collect();
        let h_flip = flip_bgra(2, 2, &bytes, true, false);
        assert_eq!(
            h_flip,
            vec![5, 6, 7, 8, 1, 2, 3, 4, 13, 14, 15, 16, 9, 10, 11, 12]
        );
        let v_flip = flip_bgra(2, 2, &bytes, false, true);
        assert_eq!(&v_flip[0..8], &[9, 10, 11, 12, 13, 14, 15, 16]);
        // Both flips is a 180-degree rotation: bottom-right moves to top-left.
        let both = flip_bgra(2, 2, &bytes, true, true);
        assert_eq!(&both[0..4], &[13, 14, 15, 16]);
    }

    #[test]
    fn crop_bgra_selects_a_sub_rectangle() {
        // N469: crop 1,0,1,2 from a 2x2 frame returns the right column.
        let bytes: Vec<u8> = (1u8..=16).collect();
        // 2x2 BGRA: the right column is [5..8] over [13..16].
        let (w, h, out) = crop_bgra(2, 2, &bytes, 1, 0, 1, 2);
        assert_eq!((w, h), (1, 2));
        assert_eq!(out, vec![5, 6, 7, 8, 13, 14, 15, 16]);
        // An out-of-range crop falls back to the source.
        assert_eq!(crop_bgra(2, 2, &bytes, 9, 9, 1, 1), (2, 2, bytes.clone()));
    }

    #[test]
    fn rotate_bgra_swaps_axes_for_90_degrees() {
        // N459: a 2x1 frame becomes 1x2 and moves the right pixel down.
        let (w, h, out) = rotate_bgra(2, 1, &[1, 0, 0, 255, 2, 0, 0, 255], 90);
        assert_eq!((w, h), (1, 2));
        // Clockwise: the left pixel stays on top, the right pixel moves below.
        assert_eq!(&out[0..4], &[1, 0, 0, 255]);
        assert_eq!(&out[4..8], &[2, 0, 0, 255]);
    }

    #[test]
    fn http_stream_png_serves_png_parts() {
        // N453: `/stream.png` streams PNG parts.
        let mut server = HttpMirrorServer::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        client
            .write_all(b"GET /stream.png HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let pixels = vec![0x40u8; 2 * 2 * 4];
        server.publish(&ShadowFrame {
            width: 2,
            height: 2,
            generation: 1,
            frame_id: 1,
            pixels: &pixels,
        });
        let mut response = Vec::new();
        drain(&mut server, &mut client, &mut response);
        let text = String::from_utf8_lossy(&response);
        assert!(text.contains("Content-Type: image/png"), "PNG part header");
        assert!(
            response.windows(8).any(|w| w == b"\x89PNG\r\n\x1a\n"),
            "PNG signature present"
        );
    }
}
