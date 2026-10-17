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
    /// N337: this stream serves JPEG parts.
    stream_jpeg: bool,
    /// N356: integer downscale applied to each streamed part (1 = full size).
    stream_scale: u16,
    /// The peer closed or errored; dropped on the next pump.
    dead: bool,
    /// Frames skipped because the previous part was still being written.
    clipped: u64,
}

/// N313: multipart boundary for the live stream.
const MULTIPART_BOUNDARY: &str = "mdcframe";

/// The path a request for a live stream uses.
const STREAM_PATHS: [&str; 3] = ["/stream", "/stream.bmp", "/stream.jpg"];

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
        /// N337: serve JPEG parts instead of BMP.
        jpeg: bool,
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
        })
    }

    pub fn with_max_clients(mut self, max_clients: usize) -> Self {
        self.max_clients = max_clients.max(1);
        self
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
        if let Some(frame) = self.last.as_ref() {
            let jpeg_parts = stream_part_for(frame, true, 1);
            let bmp_parts = stream_part_for(frame, false, 1);
            for client in &mut self.clients {
                if !client.streaming {
                    continue;
                }
                if client.stream_part.is_empty() {
                    // N356: honour the client's `?scale=` on the live stream.
                    client.stream_part = if client.stream_scale <= 1 {
                        if client.stream_jpeg {
                            jpeg_parts.clone()
                        } else {
                            bmp_parts.clone()
                        }
                    } else {
                        stream_part_for(frame, client.stream_jpeg, client.stream_scale)
                    };
                } else {
                    client.clipped = client.clipped.saturating_add(1);
                }
            }
        }
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
                        stream_jpeg: false,
                        stream_scale: 1,
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
                        match respond(last, &path, &query) {
                            Served::Bytes(bytes) => client.response = Some(bytes),
                            Served::Multipart { jpeg, scale } => {
                                begin_stream(client, last, jpeg, scale)
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
        for client in &mut self.clients {
            if client.streaming && !client.dead {
                flush_stream(client);
            }
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
fn begin_stream(client: &mut HttpClient, last: Option<&ServedFrame>, jpeg: bool, scale: u16) {
    client.streaming = true;
    client.stream_jpeg = jpeg;
    client.stream_scale = scale.clamp(1, 8);
    client.stream_headers = multipart_headers();
    client.stream_part = last
        .map(|frame| stream_part_for(frame, jpeg, client.stream_scale))
        .unwrap_or_default();
}

/// N356: build a multipart part from the raw frame at an integer downscale.
fn stream_part_for(frame: &ServedFrame, jpeg: bool, scale: u16) -> Vec<u8> {
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
    if jpeg {
        multipart_part("image/jpeg", &encode_jpeg(width, height, &raw, 80))
    } else {
        multipart_part("image/bmp", &encode_bmp(width, height, &raw))
    }
}

/// Write the pending preamble and part. Never buffers more than one part; a
/// `WouldBlock` leaves the remainder for the next pump.
fn flush_stream(client: &mut HttpClient) {
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
    if scale == 1 {
        (
            frame.width,
            frame.height,
            std::borrow::Cow::Borrowed(&frame.bgra),
        )
    } else {
        let (w, h, raw) = downscale_bgra(frame.width, frame.height, &frame.bgra, scale);
        (w, h, std::borrow::Cow::Owned(raw))
    }
}

/// N346: the JPEG `?quality=` query (1..=100, default 80).
fn query_quality(query: &str) -> u8 {
    query_param(query, "quality")
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(80)
        .clamp(1, 100)
}

fn respond(last: Option<&ServedFrame>, path: &str, query: &str) -> Served {
    if STREAM_PATHS.contains(&path) {
        // N337: `/stream.jpg` serves JPEG parts; the others serve BMP.
        // N356: `?scale=N` downscales each streamed part.
        let scale = query_param(query, "scale")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(1)
            .clamp(1, 8);
        return Served::Multipart {
            jpeg: path == "/stream.jpg",
            scale,
        };
    }
    Served::Bytes(match path {
        "/" | "/index.html" => simple_response(
            200,
            "text/html; charset=utf-8",
            b"<!doctype html><title>MyDeviceCanvas mirror</title><h1>mirror</h1>\
              <img src=\"/stream\">",
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
}
