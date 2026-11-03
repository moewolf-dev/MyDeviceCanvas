//! Session state machine, transport pumping, transactions and queues.
//!
//! The coordinator: it owns deadlines, retries, frame/tile queues and the
//! device/manager references, but delegates encoding to `mdc-protocol`.
use super::*;

pub struct Session<T: Transport> {
    pub(crate) transport: T,
    pub state: ConnectionState,
    pub device: Option<Device>,
    pub(crate) rx: Vec<u8>,
    pub(crate) next_request_id: u32,
    pub(crate) max_message: usize,
    pub(crate) max_chunk: usize,
    pub(crate) pending_requests: std::collections::BTreeSet<u32>,
    /// SIM-R04: per-request virtual deadline (milliseconds).
    pub(crate) request_deadlines: BTreeMap<u32, u64>,
    /// Control messages only. Frame and Tile stay on the surface queue.
    pub(crate) pending_control: std::collections::BTreeSet<u32>,
    pub(crate) frame_sent_at: BTreeMap<u32, std::time::Instant>,
    pub(crate) frame_received_at: BTreeMap<u32, std::time::Instant>,
    pub(crate) displayed_timings: Vec<DisplayTiming>,
    pub(crate) dropped_frames: u64,
    /// request_id -> (surface_id, frame_id) for in-flight Frame/Tile acks.
    pub(crate) pending_frames: BTreeMap<u32, (String, u64)>,
    /// ACKs and errors below this id belong to a previous link.
    pub(crate) request_floor: u32,
    /// Displayed acknowledgements already applied. A replay must not launch again.
    pub(crate) finished_acks: std::collections::BTreeSet<u32>,
    /// Tail of a packet that a short write did not accept.
    pub(crate) unsent: Option<Vec<u8>>,
    /// SIM-R04: request id of the current HELLO; only its Capabilities reply may
    /// move the session to Ready.
    pub(crate) hello_request: Option<u32>,
    /// SIM-R03: resumable frame transaction.
    pub(crate) frame_tx: Option<FrameTransaction>,
    pub(crate) copy_limit: usize,
    pub(crate) copy_budget: HostCopyBudget,
    /// (surface id, frame id) -> pixel bytes held in the copy budget.
    pub(crate) held_frames: BTreeMap<(String, u64), usize>,
    pub reconnect_policy: ReconnectPolicy,
    pub(crate) reconnect_attempt: u32,
    pub(crate) reconnect_enabled: bool,
    pub config: SessionConfig,
    pub(crate) virtual_ms: u64,
    pub(crate) connect_started_ms: u64,
    pub(crate) last_send_ms: u64,
    pub(crate) last_heartbeat_ms: u64,
    /// Ping sent and not yet answered. Three misses start reconnect.
    pub(crate) missed_heartbeats: u32,
    pub(crate) heartbeat_outstanding: bool,
    /// SIM-R04: request id of the outstanding Ping; only that Pong counts.
    pub(crate) heartbeat_request: Option<u32>,
    pub(crate) reconnect_ready_ms: Option<u64>,
    /// After reconnect backoff, host must supply a fresh transport via
    /// [`provide_transport`](Self::provide_transport) before HELLO is resent.
    pub awaiting_transport: bool,
    /// After reconnect / endpoint switch, tiles are blocked until a full frame is ACKed.
    pub needs_full_frame: bool,
    pub(crate) manager: Option<Arc<Mutex<DeviceManager>>>,
    /// Latest complete mirror frame. Bounded to one; a newer frame replaces it.
    pub(crate) mirror_inbox: Option<MirrorFrame>,
    /// In-progress chunked mirror frame: (surface, w, h, assembler).
    pub(crate) mirror_assembler: Option<(String, u16, u16, FrameAssembler)>,
    /// N264: the negotiated mirror mode ("full-frame" | "dirty-region").
    pub(crate) mirror_mode: String,
    /// N264: the last complete mirror frame, patched by dirty-region updates.
    pub(crate) mirror_last: Option<(String, u16, u16, Vec<u8>)>,
    pub(crate) mirror_frames: u64,
    pub(crate) mirror_dropped: u64,
    pub(crate) mirror_last_frame_id: u64,
    pub(crate) packages_sent: u64,
    /// N40: configuration values learned from device replies.
    pub(crate) config_cache: BTreeMap<String, String>,
    /// SIM-R04: key -> request id for config replies that were actually asked for.
    pub(crate) pending_configs: BTreeMap<String, u32>,
    /// N304: host boot identity advertised in Hello (0 = unset).
    pub(crate) host_boot_id: u64,
    /// N304: the peer's last reported boot identity (None before the first
    /// Capabilities reply). A change means the device rebooted.
    pub(crate) device_boot_id: Option<u64>,
    /// N304: the peer's last reported run id.
    pub(crate) device_run_id: u64,
    /// N316: bounded diagnostics received on the separate channel.
    pub(crate) diagnostics: std::collections::VecDeque<DiagnosticRecord>,
    /// N329: opt-in host frame pacing (fps clamp + byte budget).
    pub(crate) pacing: Option<mdc_schedule::Scheduler>,
    /// N329: frames refused by the pacing budget.
    pub(crate) pacing_dropped: u64,
    /// N398: host-side session counters.
    pub(crate) stats: SessionStats,
    /// N547: the last `tiles_received` count reported by the device.
    pub(crate) device_tiles_received: u64,
    /// N419: the highest OTA phase sent (0 none, 1 begin, 2 chunk, 3 confirm).
    pub(crate) ota_phase: u8,
    /// N732: `(virtual_ms, bytes_sent)` baseline for the measured bitrate.
    pub(crate) throughput_mark: (u64, u64),
    /// N732: the last measured bitrate (reused when no new sample exists).
    pub(crate) last_bitrate: Option<u64>,
    /// N473: derives gestures from incoming input events.
    pub(crate) gestures: GestureTracker,
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
            request_deadlines: BTreeMap::new(),
            pending_control: std::collections::BTreeSet::new(),
            frame_sent_at: BTreeMap::new(),
            frame_received_at: BTreeMap::new(),
            displayed_timings: Vec::new(),
            dropped_frames: 0,
            pending_frames: BTreeMap::new(),
            request_floor: 0,
            finished_acks: std::collections::BTreeSet::new(),
            unsent: None,
            hello_request: None,
            frame_tx: None,
            copy_limit: MAX_FRAME_BYTES * 3,
            copy_budget: HostCopyBudget::new(MAX_FRAME_BYTES * 3),
            held_frames: BTreeMap::new(),
            reconnect_policy: ReconnectPolicy::default(),
            reconnect_attempt: 0,
            reconnect_enabled: config.reconnect,
            config,
            virtual_ms: 0,
            connect_started_ms: 0,
            last_send_ms: 0,
            last_heartbeat_ms: 0,
            missed_heartbeats: 0,
            heartbeat_outstanding: false,
            heartbeat_request: None,
            reconnect_ready_ms: None,
            awaiting_transport: false,
            needs_full_frame: false,
            manager: None,
            mirror_inbox: None,
            mirror_assembler: None,
            mirror_mode: String::new(),
            mirror_last: None,
            mirror_frames: 0,
            mirror_dropped: 0,
            mirror_last_frame_id: 0,
            packages_sent: 0,
            config_cache: BTreeMap::new(),
            pending_configs: BTreeMap::new(),
            host_boot_id: 0,
            device_boot_id: None,
            device_run_id: 0,
            diagnostics: std::collections::VecDeque::new(),
            pacing: None,
            pacing_dropped: 0,
            stats: SessionStats::default(),
            device_tiles_received: 0,
            ota_phase: 0,
            throughput_mark: (0, 0),
            last_bitrate: None,
            gestures: GestureTracker::default(),
        }
    }
    pub fn set_manager(&mut self, manager: Arc<Mutex<DeviceManager>>) {
        self.manager = Some(manager);
    }
    /// N329: pace `send_frame` with an fps clamp and a byte budget. `0` for either
    /// limit disables that limit (`max_fps = 0` = no clamp, `bytes_per_second = 0`
    /// = unlimited). A paced frame that does not fit returns [`CoreError::Throttled`].
    pub fn set_frame_pacing(&mut self, max_fps: u16, bytes_per_second: u64) {
        self.pacing = Some(mdc_schedule::Scheduler::new(max_fps, bytes_per_second));
    }
    pub fn clear_frame_pacing(&mut self) {
        self.pacing = None;
    }
    /// N329: frames refused by the pacing budget since the last reset.
    pub fn pacing_dropped(&self) -> u64 {
        self.pacing_dropped
    }
    /// N304: set the host boot identity advertised in Hello.
    pub fn set_host_boot_id(&mut self, boot_id: u64) {
        self.host_boot_id = boot_id;
    }
    pub fn host_boot_id(&self) -> u64 {
        self.host_boot_id
    }
    /// N304: the peer boot identity from the last Capabilities reply.
    pub fn device_boot_id(&self) -> Option<u64> {
        self.device_boot_id
    }
    pub fn device_run_id(&self) -> u64 {
        self.device_run_id
    }
    /// N304: adopt a Capabilities reply's run identity. Returns true when the
    /// device reported a new boot id (the previous run's pending ACKs are dropped).
    fn adopt_device_identity(&mut self, caps: &Capabilities) -> bool {
        self.note_run_identity(caps.boot_id, caps.run_id)
    }
    /// N304/N316: fold a peer-reported `(boot_id, run_id)` into the run
    /// bookkeeping. A changed boot id discards the previous run.
    fn note_run_identity(&mut self, boot_id: u64, run_id: u64) -> bool {
        if boot_id == 0 && run_id == 0 {
            return false;
        }
        let restarted = matches!(
            (self.device_boot_id, boot_id),
            (Some(old), new) if new != 0 && old != new
        );
        if boot_id != 0 {
            self.device_boot_id = Some(boot_id);
        }
        if run_id != 0 {
            self.device_run_id = run_id;
        }
        if restarted {
            self.discard_run();
        }
        restarted
    }
    /// N316: drain the bounded diagnostic queue (metrics + boot info). This is a
    /// separate channel: it never mixes with frame/ack results.
    pub fn take_diagnostics(&mut self) -> Vec<DiagnosticRecord> {
        self.diagnostics.drain(..).collect()
    }
    pub fn diagnostic_count(&self) -> usize {
        self.diagnostics.len()
    }
    fn push_diagnostic(&mut self, record: DiagnosticRecord) {
        while self.diagnostics.len() >= MAX_DIAGNOSTICS {
            self.diagnostics.pop_front();
        }
        self.diagnostics.push_back(record);
    }
    /// SIM-R06: every path that abandons a link clears the same state. Keeping
    /// this in one place prevents a reconnect from inheriting a stale request
    /// deadline, frame transaction, mirror base or gesture chain.
    fn clear_link_state(&mut self) {
        self.rx.clear();
        self.pending_requests.clear();
        self.request_deadlines.clear();
        self.pending_control.clear();
        self.frame_sent_at.clear();
        self.frame_received_at.clear();
        self.pending_frames.clear();
        self.unsent = None;
        self.frame_tx = None;
        self.hello_request = None;
        self.finished_acks.clear();
        self.mirror_inbox = None;
        self.mirror_assembler = None;
        self.mirror_last = None;
        self.mirror_last_frame_id = 0;
        self.config_cache.clear();
        self.pending_configs.clear();
        self.gestures.reset();
        self.missed_heartbeats = 0;
        self.heartbeat_outstanding = false;
        self.heartbeat_request = None;
        self.copy_budget = HostCopyBudget::new(self.copy_limit);
    }

    /// Drop every outstanding request/held frame from the previous device run.
    fn discard_run(&mut self) {
        self.held_frames.clear();
        self.clear_link_state();
        self.request_floor = self.next_request_id;
        self.missed_heartbeats = 0;
        self.heartbeat_outstanding = false;
        self.needs_full_frame = true;
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
            boot_id: self.host_boot_id,
        })
        .map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.hello_request = Some(self.write_packet(MessageType::Hello, hello)?);
        Ok(())
    }
    /// N398/N436: the host-side session counters.
    pub fn stats(&self) -> SessionStats {
        let mut stats = SessionStats {
            dropped_frames: self.dropped_frames,
            packages_sent: self.packages_sent,
            mirror_frames: self.mirror_frames,
            pacing_dropped: self.pacing_dropped,
            // N547: the device's reported applied-tile count rides along, and the
            // outstanding gap is derived in one place.
            device_tiles_received: self.device_tiles_received,
            ..self.stats
        };
        stats.tiles_missing = stats.tiles_sent.saturating_sub(stats.device_tiles_received);
        stats
    }

    /// N732: measure the outbound bitrate since the previous call using the
    /// virtual clock. Returns `None` when no virtual time has elapsed.
    pub fn measure_bitrate(&mut self) -> Option<u64> {
        let now = self.virtual_ms;
        let bytes = self.stats.bytes_sent;
        let (last_ms, last_bytes) = self.throughput_mark;
        self.throughput_mark = (now, bytes);
        let elapsed = now.saturating_sub(last_ms);
        if elapsed == 0 {
            return None;
        }
        let rate = bytes.saturating_sub(last_bytes).saturating_mul(1000) / elapsed;
        self.last_bitrate = Some(rate);
        Some(rate)
    }

    /// N732: adapt a dirty tracker to the measured outbound bitrate. `frame_bytes`
    /// is one full RGB565 frame; a link below ~1 FPS forces full frames for any
    /// meaningful dirty area.
    pub fn adapt_dirty_threshold(
        &mut self,
        tracker: &mut DirtyTracker,
        frame_bytes: u64,
    ) -> Option<f64> {
        let rate = self.measure_bitrate().or(self.last_bitrate)?;
        Some(tracker.adapt_to_bandwidth(rate, frame_bytes))
    }

    /// N750: one tuning pass for both streaming knobs. Measures the outbound
    /// rate once, adapts the active scheduler (if any) and the dirty tracker.
    pub fn tune_streaming(
        &mut self,
        tracker: &mut DirtyTracker,
        frame_bytes: u64,
    ) -> Option<AdaptiveTuning> {
        let bitrate = self.measure_bitrate().or(self.last_bitrate)?;
        let pacing = self
            .pacing
            .as_mut()
            .map(|scheduler| scheduler.adapt_to_bitrate(bitrate, frame_bytes));
        let dirty_threshold = Some(tracker.adapt_to_bandwidth(bitrate, frame_bytes));
        Some(AdaptiveTuning {
            bitrate,
            pacing,
            dirty_threshold,
        })
    }

    /// N758: plan every surface against one shared budget (from the active
    /// pacing scheduler, or unlimited when pacing is off). Each tile is billed at
    /// 1 KiB, matching `send_frame`, and surfaces take turns so a low-bandwidth
    /// link still advances each display.
    pub fn plan_shared_surface_tiles(&self) -> Option<mdc_schedule::SharedDecision> {
        let device = self.device.as_ref()?;
        let budget = self
            .pacing
            .as_ref()
            .map(|scheduler| scheduler.budget_bytes())
            .unwrap_or(0);
        Some(device.plan_shared_tiles(budget))
    }

    /// N767: plan with per-surface weights (missing entries mean 1). The first
    /// tile of every surface is still served when the budget allows.
    pub fn plan_shared_surface_tiles_weighted(
        &self,
        weights: &[u16],
    ) -> Option<mdc_schedule::SharedDecision> {
        let device = self.device.as_ref()?;
        let budget = self
            .pacing
            .as_ref()
            .map(|scheduler| scheduler.budget_bytes())
            .unwrap_or(0);
        Some(device.plan_shared_tiles_weighted(budget, weights))
    }

    /// N777: the device's advertised surface weights (`surface_weights` config),
    /// or `None` when unset/invalid.
    pub fn surface_weights(&self) -> Option<Vec<u16>> {
        self.config_cache
            .get("surface_weights")
            .and_then(|value| parse_surface_weights(value))
    }

    /// N777: plan with the device's configured weights (falling back to 1 each).
    pub fn plan_shared_surface_tiles_from_config(&self) -> Option<mdc_schedule::SharedDecision> {
        let weights = self.surface_weights().unwrap_or_default();
        self.plan_shared_surface_tiles_weighted(&weights)
    }

    /// N741: adapt an active frame-pacing scheduler to the measured outbound
    /// bitrate. Returns the new configuration (None without a scheduler/rate).
    pub fn adapt_frame_pacing(
        &mut self,
        frame_bytes: u64,
    ) -> Option<mdc_schedule::SchedulerConfig> {
        let rate = self.measure_bitrate().or(self.last_bitrate)?;
        let scheduler = self.pacing.as_mut()?;
        Some(scheduler.adapt_to_bitrate(rate, frame_bytes))
    }

    /// N547: reconcile the host's sent tiles against the device's reported count.
    pub fn tile_reconciliation(&self) -> TileReconciliation {
        reconcile_tiles(&self.stats(), self.device_tiles_received)
    }

    /// N557: if the device has not acknowledged more than `max_missing` tiles,
    /// set `needs_full_frame` so the next update resynchronises. Returns the
    /// reconciliation that was evaluated.
    pub fn enforce_tile_reconciliation(&mut self, max_missing: u64) -> TileReconciliation {
        let report = self.tile_reconciliation();
        if report.needs_full_frame(max_missing) {
            self.needs_full_frame = true;
        }
        report
    }

    /// N616: the factory default a device advertised for `key` in its
    /// `Capabilities.config.defaults`, if any.
    pub fn advertised_config_default(&self, key: &str) -> Option<&str> {
        self.device
            .as_ref()
            .and_then(|device| device.capabilities.config.default_for(key))
    }

    /// N586/N616: the `tile_missing_threshold`. A value read via `get_config`
    /// wins; otherwise the device's advertised factory default is used. `None`
    /// means neither is known.
    pub fn tile_missing_threshold(&self) -> Option<u64> {
        self.config(TILES_MISSING_THRESHOLD_CONFIG)
            .and_then(|value| value.trim().parse().ok())
            .or_else(|| {
                self.advertised_config_default(TILES_MISSING_THRESHOLD_CONFIG)
                    .and_then(|value| value.trim().parse().ok())
            })
    }

    /// N586: enforce the reconciled threshold from the device configuration. With
    /// no advertised key the check is a no-op (returns the report unchanged).
    pub fn enforce_tile_reconciliation_from_config(&mut self) -> TileReconciliation {
        match self.tile_missing_threshold() {
            Some(threshold) => self.enforce_tile_reconciliation(threshold),
            None => self.tile_reconciliation(),
        }
    }

    pub fn poll(&mut self) -> Result<ConnectionState, CoreError> {
        // SIM-R07: bound both the read and the parse per call. A peer that
        // floods data must not make one poll unbounded or delay cancellation.
        let mut read_bytes = 0usize;
        while read_bytes < MAX_POLL_BYTES && self.rx.len() < MAX_RX_BYTES {
            // 已经进缓冲区的包先处理。对端先写下 cancel 再关闭时，不能让关闭错误吞掉这些包。
            match self.transport.read() {
                Ok(Some(bytes)) => {
                    // N436: count inbound bytes.
                    read_bytes = read_bytes.saturating_add(bytes.len());
                    self.stats.bytes_received =
                        self.stats.bytes_received.saturating_add(bytes.len() as u64);
                    self.rx.extend(bytes)
                }
                Ok(None) => break,
                Err(error) => {
                    if self.rx.is_empty() {
                        let _ = self.mark_transport_failure();
                        return Err(CoreError::Transport(error.to_string()));
                    }
                    break;
                }
            }
        }
        let mut packets = 0usize;
        let mut parsed_bytes = 0usize;
        while packets < MAX_POLL_PACKETS && parsed_bytes < MAX_POLL_BYTES {
            match mdc_protocol::decode_stream(&self.rx, self.max_message) {
                mdc_protocol::StreamStep::Packet(packet, consumed) => {
                    self.rx.drain(..consumed);
                    parsed_bytes = parsed_bytes.saturating_add(consumed);
                    packets = packets.saturating_add(1);
                    self.handle_packet(packet)?;
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
                    return Err(CoreError::Protocol(error.to_string()));
                }
            }
        }
        Ok(self.state)
    }
    /// Advance virtual clock; drives heartbeat, ack timeout, and reconnect edges.
    pub fn tick(&mut self, virtual_ms: u64) -> Result<ConnectionState, CoreError> {
        self.virtual_ms = virtual_ms;
        // N473: held pointers may become long-presses on the virtual clock.
        while let Some(gesture) = self.gestures.tick(virtual_ms) {
            self.emit_gesture(gesture);
        }
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
                    && self
                        .request_deadlines
                        .values()
                        .any(|deadline| *deadline <= virtual_ms)
                {
                    let _ = self.mark_transport_failure();
                    return Err(CoreError::AckTimeout);
                }
                if self.config.heartbeat_ms > 0
                    && virtual_ms.saturating_sub(self.last_heartbeat_ms) >= self.config.heartbeat_ms
                {
                    if self.heartbeat_outstanding {
                        self.missed_heartbeats = self.missed_heartbeats.saturating_add(1);
                        if self.missed_heartbeats >= 3 {
                            let _ = self.mark_transport_failure();
                            return Err(CoreError::HeartbeatMissed);
                        }
                    }
                    let id = self.alloc_request_id()?;
                    self.write_packet_with_id(MessageType::Ping, Vec::new(), id, false)?;
                    self.heartbeat_outstanding = true;
                    self.heartbeat_request = Some(id);
                    self.last_heartbeat_ms = virtual_ms;
                }
            }
            ConnectionState::Reconnecting => {
                if let Some(ready_at) = self.reconnect_ready_ms {
                    if virtual_ms >= ready_at {
                        self.prepare_transport_replace();
                    }
                }
            }
            _ => {}
        }
        Ok(self.state)
    }

    /// Close the dead link and wait for [`provide_transport`](Self::provide_transport).
    fn prepare_transport_replace(&mut self) {
        self.transport.close();
        self.rx.clear();
        self.pending_requests.clear();
        self.pending_control.clear();
        self.frame_sent_at.clear();
        self.frame_received_at.clear();
        self.pending_frames.clear();
        self.unsent = None;
        self.frame_tx = None;
        self.hello_request = None;
        self.finished_acks.clear();
        self.held_frames.clear();
        self.copy_budget = HostCopyBudget::new(self.copy_limit);
        self.mirror_assembler = None;
        self.mirror_last = None;
        self.mirror_last_frame_id = 0;
        self.config_cache.clear();
        self.gestures.reset();
        self.device = None;
        self.max_message = MAX_FRAME_BYTES;
        self.max_chunk = mdc_protocol::DEFAULT_MAX_CHUNK;
        self.reconnect_ready_ms = None;
        self.awaiting_transport = true;
        self.state = ConnectionState::Connecting;
    }

    /// Supply a new transport after reconnect backoff and resend HELLO.
    pub fn provide_transport(&mut self, transport: T) -> Result<(), CoreError> {
        if !self.awaiting_transport
            && self.state != ConnectionState::Disconnected
            && self.state != ConnectionState::Connecting
        {
            return Err(CoreError::Transport(
                "session is not awaiting a replacement transport".into(),
            ));
        }
        self.transport = transport;
        self.awaiting_transport = false;
        self.rx.clear();
        self.pending_requests.clear();
        self.pending_control.clear();
        self.frame_sent_at.clear();
        self.frame_received_at.clear();
        self.pending_frames.clear();
        self.unsent = None;
        self.finished_acks.clear();
        self.held_frames.clear();
        self.copy_budget = HostCopyBudget::new(self.copy_limit);
        self.request_floor = self.next_request_id;
        self.device = None;
        self.needs_full_frame = true;
        self.missed_heartbeats = 0;
        self.heartbeat_outstanding = false;
        self.state = ConnectionState::Disconnected;
        self.connect()
    }

    /// Cancel the current Session and attach a different endpoint (I03).
    /// Old ACKs are dropped; after handshake the host must send a full frame
    /// before any Tile (`needs_full_frame`).
    pub fn switch_transport(&mut self, transport: T) -> Result<(), CoreError> {
        let reconnect = self.reconnect_enabled;
        self.transport.close();
        self.held_frames.clear();
        self.clear_link_state();
        self.request_floor = self.next_request_id;
        self.device = None;
        self.gestures.reset();
        self.awaiting_transport = false;
        self.needs_full_frame = true;
        self.reconnect_ready_ms = None;
        self.state = ConnectionState::Disconnected;
        self.reconnect_enabled = reconnect;
        self.transport = transport;
        self.connect()
    }
    pub fn disconnect(&mut self) {
        self.reconnect_enabled = false;
        self.awaiting_transport = false;
        self.state = ConnectionState::Closing;
        self.transport.close();
        self.state = ConnectionState::Disconnected;
        self.device = None;
        self.held_frames.clear();
        self.clear_link_state();
    }
    pub fn enable_reconnect(&mut self, enabled: bool) {
        self.reconnect_enabled = enabled;
        self.config.reconnect = enabled;
    }
    pub fn mark_transport_failure(&mut self) -> Option<u64> {
        if !self.reconnect_enabled {
            self.state = ConnectionState::Disconnected;
            self.transport.close();
            self.device = None;
            self.held_frames.clear();
            self.clear_link_state();
            return None;
        }
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        self.state = ConnectionState::Reconnecting;
        self.device = None;
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
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }
    pub fn copy_outstanding(&self) -> usize {
        self.copy_budget.outstanding()
    }
    pub fn copy_peak(&self) -> usize {
        self.copy_budget.peak()
    }
    /// Test and board profiles can tighten the host copy limit. Outstanding bytes are cleared.
    pub fn set_copy_limit(&mut self, limit: usize) {
        self.copy_limit = limit;
        self.held_frames.clear();
        self.copy_budget = HostCopyBudget::new(limit);
    }
    pub fn displayed_timings(&self) -> &[DisplayTiming] {
        &self.displayed_timings
    }
    /// SIM-R09: take a cheap snapshot of every growing container. Locks the
    /// manager only for the event/device counts.
    pub fn container_counts(&self) -> ContainerCounts {
        let (manager_events, devices) = self
            .manager
            .as_ref()
            .and_then(|manager| manager.lock().ok())
            .map(|manager| (manager.event_count(), manager.devices().count()))
            .unwrap_or((0, 0));
        ContainerCounts {
            rx_bytes: self.rx.len(),
            pending_requests: self.pending_requests.len(),
            pending_control: self.pending_control.len(),
            pending_frames: self.pending_frames.len(),
            request_deadlines: self.request_deadlines.len(),
            finished_acks: self.finished_acks.len(),
            displayed_timings: self.displayed_timings.len(),
            held_frames: self.held_frames.len(),
            diagnostics: self.diagnostics.len(),
            config_cache: self.config_cache.len(),
            pending_configs: self.pending_configs.len(),
            active_pointers: self.gestures.pointers.len(),
            mirror_inbox_frames: usize::from(self.mirror_inbox.is_some()),
            mirror_base_frames: usize::from(self.mirror_last.is_some()),
            frame_transactions: usize::from(self.frame_tx.is_some()),
            manager_events,
            devices,
        }
    }
    /// SIM-R07: remember a displayed ACK in a fixed-size window. An evicted id
    /// is harmless: its request and frame were already released, so a replay
    /// cannot launch anything again.
    pub(crate) fn record_finished_ack(&mut self, id: u32) {
        if self.finished_acks.len() >= MAX_FINISHED_ACKS {
            self.finished_acks.pop_first();
        }
        self.finished_acks.insert(id);
    }
    fn release_request(&mut self, id: u32) {
        self.pending_requests.remove(&id);
        self.request_deadlines.remove(&id);
        self.pending_control.remove(&id);
    }
    fn release_held(&mut self, surface_id: &str, frame_id: u64) {
        if let Some(pixels) = self.held_frames.remove(&(surface_id.to_string(), frame_id)) {
            self.copy_budget.release_frame(pixels);
        }
    }
    pub(crate) fn note_displayed(&mut self, id: u32) {
        let Some(sent) = self.frame_sent_at.remove(&id) else {
            return;
        };
        let now = std::time::Instant::now();
        let received = self.frame_received_at.remove(&id).unwrap_or(now);
        let transport = received.saturating_duration_since(sent);
        let e2e = now.saturating_duration_since(sent);
        let render = e2e.saturating_sub(transport);
        if self.displayed_timings.len() >= MAX_DISPLAY_TIMINGS {
            self.displayed_timings.remove(0);
        }
        self.displayed_timings.push(DisplayTiming {
            transport_us: transport.as_micros(),
            render_us: render.as_micros(),
            e2e_us: e2e.as_micros(),
        });
    }
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }
    pub fn resume_write(&mut self) -> Result<(), CoreError> {
        if let Some(rest) = self.unsent.take() {
            self.write_bytes(&rest)?;
        }
        if self.frame_tx.is_some() {
            self.drive_frame_tx()?;
        }
        Ok(())
    }

    fn drive_frame_tx(&mut self) -> Result<(), CoreError> {
        let Some(mut tx) = self.frame_tx.take() else {
            return Ok(());
        };
        loop {
            match tx.stage {
                FrameTxStage::Begin => {
                    let begin = encode_frame_begin(
                        &tx.surface_id,
                        tx.frame_id,
                        tx.width,
                        tx.height,
                        tx.pixels.len() as u32,
                    )
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                    // Mark the stage before writing: a short write leaves the
                    // BEGIN tail in `unsent`, so resume must continue with chunks.
                    tx.stage = FrameTxStage::Chunks;
                    tx.next_offset = 0;
                    match self.write_packet_with_id(MessageType::Frame, begin, tx.request_id, true)
                    {
                        Ok(()) => {}
                        Err(CoreError::ShortWrite) => {
                            self.frame_tx = Some(tx);
                            return Err(CoreError::ShortWrite);
                        }
                        Err(error) => {
                            self.pending_requests.remove(&tx.request_id);
                            self.pending_frames.remove(&tx.request_id);
                            return Err(error);
                        }
                    }
                }
                FrameTxStage::Chunks => {
                    if tx.next_offset >= tx.pixels.len() {
                        tx.stage = FrameTxStage::Commit;
                        continue;
                    }
                    let end = (tx.next_offset + tx.max_chunk).min(tx.pixels.len());
                    let chunk = encode_frame_chunk(
                        tx.frame_id,
                        tx.next_offset as u32,
                        &tx.pixels[tx.next_offset..end],
                    );
                    // Advance before writing so a short chunk is not re-sent.
                    tx.next_offset = end;
                    match self.write_packet_with_id(MessageType::Frame, chunk, tx.request_id, false)
                    {
                        Ok(()) => {}
                        Err(CoreError::ShortWrite) => {
                            self.frame_tx = Some(tx);
                            return Err(CoreError::ShortWrite);
                        }
                        Err(error) => {
                            self.pending_requests.remove(&tx.request_id);
                            self.pending_frames.remove(&tx.request_id);
                            return Err(error);
                        }
                    }
                }
                FrameTxStage::Commit => {
                    let commit = encode_frame_commit(tx.frame_id);
                    tx.stage = FrameTxStage::Done;
                    match self.write_packet_with_id(
                        MessageType::Frame,
                        commit,
                        tx.request_id,
                        false,
                    ) {
                        Ok(()) => return Ok(()),
                        Err(CoreError::ShortWrite) => {
                            self.frame_tx = Some(tx);
                            return Err(CoreError::ShortWrite);
                        }
                        Err(error) => {
                            self.pending_requests.remove(&tx.request_id);
                            self.pending_frames.remove(&tx.request_id);
                            return Err(error);
                        }
                    }
                }
                FrameTxStage::Done => return Ok(()),
            }
        }
    }
    pub fn send_frame(&mut self, frame: Frame) -> Result<u32, CoreError> {
        self.stats.frames_sent = self.stats.frames_sent.saturating_add(1);
        if self.unsent.is_some() {
            return Err(CoreError::ShortWrite);
        }
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        {
            let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
            device.validate_frame(&frame)?;
        }
        // N329: opt-in pacing refuses a frame that is too soon or over budget.
        if self.pacing.is_some() {
            let cost_tile = 1024u32;
            let count = frame.bytes.len().div_ceil(cost_tile as usize) as u32;
            let tiles: Vec<mdc_schedule::TileCost> = (0..count)
                .map(|i| mdc_schedule::TileCost::new(i, cost_tile, false))
                .collect();
            let now = self.virtual_ms;
            let decision = self
                .pacing
                .as_ref()
                .map(|scheduler| scheduler.plan(now, &tiles));
            match decision {
                Some(mdc_schedule::FrameDecision::Defer { .. }) => {
                    return Err(CoreError::Throttled);
                }
                Some(mdc_schedule::FrameDecision::Send { deferred, .. })
                    if !deferred.is_empty() =>
                {
                    self.pacing_dropped = self.pacing_dropped.saturating_add(1);
                    return Err(CoreError::Throttled);
                }
                Some(mdc_schedule::FrameDecision::Send { .. }) => {
                    if let Some(scheduler) = self.pacing.as_mut() {
                        scheduler.commit(now);
                    }
                }
                None => {}
            }
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
        let pixel_len = pixels.len();
        self.copy_budget
            .hold_frame(pixel_len)
            .map_err(|_| CoreError::CopyBudget)?;
        let queued = (|| {
            let device = self.device.as_mut().ok_or(CoreError::NotReady)?;
            let surface = device
                .surfaces
                .iter_mut()
                .find(|s| s.info.id == surface_id)
                .ok_or(CoreError::Dimensions)?;
            let old_pending = surface.pending_frame_id().zip(surface.pending_pixels());
            let before = surface.replaced_pending();
            let id = surface.enqueue_frame(frame)?;
            Ok::<_, CoreError>((
                id,
                surface.replaced_pending().saturating_sub(before),
                old_pending,
            ))
        })();
        let (frame_id, replaced, old_pending) = match queued {
            Ok(value) => value,
            Err(error) => {
                self.copy_budget.release_frame(pixel_len);
                return Err(error);
            }
        };
        if replaced > 0 {
            if let Some((old_id, old_len)) = old_pending {
                self.held_frames.remove(&(surface_id.clone(), old_id));
                self.copy_budget.release_frame(old_len);
            }
        }
        self.held_frames
            .insert((surface_id.clone(), frame_id), pixel_len);
        self.dropped_frames = self.dropped_frames.saturating_add(replaced);
        let send_now = self
            .device
            .as_ref()
            .and_then(|device| device.surfaces.iter().find(|s| s.info.id == surface_id))
            .map(|surface| surface.in_flight() == Some(frame_id))
            .unwrap_or(false);
        if !send_now {
            // 0 表示这一帧只占着待发槽，还没有请求号。在途帧 ACK 之后才会写到链路上。
            return Ok(0);
        }
        self.write_frame_bytes(&surface_id, frame_id, width, height, &pixels, max_chunk)
    }
    fn write_frame_bytes(
        &mut self,
        surface_id: &str,
        frame_id: u64,
        width: u16,
        height: u16,
        pixels: &[u8],
        max_chunk: usize,
    ) -> Result<u32, CoreError> {
        let legacy = encode_frame_payload(surface_id, frame_id, width, height, pixels)
            .map_err(|e| CoreError::Protocol(e.to_string()))?;
        let request_id = self.alloc_request_id()?;
        let write_result = if legacy.len() <= max_chunk {
            self.write_packet_with_id(MessageType::Frame, legacy, request_id, true)
        } else {
            self.pending_frames
                .insert(request_id, (surface_id.to_string(), frame_id));
            self.frame_sent_at
                .insert(request_id, std::time::Instant::now());
            self.pending_requests.insert(request_id);
            if self.config.ack_timeout_ms > 0 {
                self.request_deadlines.insert(
                    request_id,
                    self.virtual_ms.saturating_add(self.config.ack_timeout_ms),
                );
            }
            self.frame_tx = Some(FrameTransaction {
                surface_id: surface_id.to_string(),
                frame_id,
                width,
                height,
                pixels: pixels.to_vec(),
                max_chunk,
                request_id,
                stage: FrameTxStage::Begin,
                next_offset: 0,
            });
            self.drive_frame_tx()
        };
        if matches!(write_result, Ok(()) | Err(CoreError::ShortWrite)) {
            // Legacy path: chunked path already recorded the transaction.
            self.pending_frames
                .entry(request_id)
                .or_insert_with(|| (surface_id.to_string(), frame_id));
            self.frame_sent_at
                .entry(request_id)
                .or_insert_with(std::time::Instant::now);
        }
        write_result?;
        Ok(request_id)
    }
    fn launch_pending_frame(&mut self, surface_id: &str, frame_id: u64) -> Result<(), CoreError> {
        let follow = {
            let device = self.device.as_mut().ok_or(CoreError::NotReady)?;
            let surface = device
                .surfaces
                .iter_mut()
                .find(|surface| surface.info.id == surface_id);
            match surface {
                Some(surface) => surface.ack(frame_id),
                None => None,
            }
        };
        let Some((next_id, frame)) = follow else {
            return Ok(());
        };
        let max_chunk = self.negotiated_max_chunk();
        self.write_frame_bytes(
            &frame.surface_id,
            next_id,
            frame.width,
            frame.height,
            &frame.bytes,
            max_chunk,
        )?;
        Ok(())
    }
    fn negotiated_max_chunk(&self) -> usize {
        let device_max = self
            .device
            .as_ref()
            .map(|device| device.capabilities.max_chunk as usize)
            .unwrap_or(self.max_chunk);
        self.max_chunk.min(device_max).max(1)
    }
    pub fn send_tile(&mut self, tile: Tile) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        if self.needs_full_frame {
            return Err(CoreError::NeedFullFrame);
        }
        let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
        if !device.capabilities.tile {
            return Err(CoreError::Unsupported);
        }
        device.send_tile(&tile)?;
        if device
            .surfaces
            .iter()
            .find(|surface| surface.info.id == tile.surface_id)
            .map(|surface| surface.rejects_tile(tile.base_frame_id))
            .unwrap_or(false)
        {
            return Err(CoreError::QueueFull);
        }
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
        // N506: count the partial update and its payload bytes.
        self.stats.tiles_sent = self.stats.tiles_sent.saturating_add(1);
        self.stats.tile_bytes_sent = self
            .stats
            .tile_bytes_sent
            .saturating_add(tile.bytes.len() as u64);
        self.pending_frames
            .insert(request_id, (tile.surface_id, tile.base_frame_id));
        Ok(request_id)
    }
    /// N516: send only the tiles covering `rects` of a tightly packed RGB565
    /// frame, and return the number of tiles sent. Duplicate/overlapping rects
    /// collapse into one tile.
    #[allow(clippy::too_many_arguments)]
    pub fn send_dirty_tiles(
        &mut self,
        surface_id: &str,
        base_frame_id: u64,
        frame: &[u8],
        frame_size: (u16, u16),
        tile_size: u16,
        rects: &[DirtyRect],
    ) -> Result<usize, CoreError> {
        let tiles = plan_dirty_tiles(frame_size, tile_size, rects);
        for tile in &tiles {
            let (x, y, w, h) = *tile;
            self.send_tile(Tile {
                surface_id: surface_id.to_string(),
                base_frame_id,
                x,
                y,
                width: w,
                height: h,
                bytes: extract_tile(frame, frame_size, *tile),
            })?;
        }
        Ok(tiles.len())
    }

    /// N526: send the accumulated dirty regions of `frame`, or a full frame when
    /// the tracker decides the partial update is not worth it. The frame must be
    /// tightly packed RGB565 at `tracker`'s surface size.
    pub fn send_dirty_update(
        &mut self,
        surface_id: &str,
        base_frame_id: u64,
        frame: &[u8],
        tracker: &mut DirtyTracker,
    ) -> Result<UpdateDecision, CoreError> {
        let decision = tracker.decide();
        let tiles = tracker.tiles();
        let (surface, tile_size) = (tracker.surface, tracker.tile_size);
        tracker.take();
        match decision {
            UpdateDecision::Full => {
                self.send_frame(Frame {
                    surface_id: surface_id.to_string(),
                    width: surface.0,
                    height: surface.1,
                    bytes: frame.to_vec(),
                })?;
                Ok(UpdateDecision::Full)
            }
            UpdateDecision::Partial(_) => {
                let sent = self.send_dirty_tiles(
                    surface_id,
                    base_frame_id,
                    frame,
                    surface,
                    tile_size,
                    &tiles,
                )?;
                Ok(UpdateDecision::Partial(sent))
            }
        }
    }

    /// Send an OTA control command; gated by device `ota` capability.
    pub fn send_ota(&mut self, command: OtaCommand) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
        device.ota()?;
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        let request_id = self.write_packet(MessageType::Ota, payload)?;
        // N419: checkpoint the phase so a driver can resume after a reconnect.
        self.ota_phase = match command.action.as_str() {
            "begin" => 1,
            "chunk" => 2,
            "confirm" => 3,
            "abort" => 0,
            _ => self.ota_phase,
        };
        Ok(request_id)
    }

    /// N473: push a recognised gesture to the manager (if attached).
    fn emit_gesture(&mut self, gesture: Gesture) {
        let device_id = self
            .device
            .as_ref()
            .map(|device| device.capabilities.device_id.clone())
            .unwrap_or_default();
        if let Some(manager) = &self.manager {
            if let Ok(mut manager) = manager.lock() {
                manager.push_gesture(device_id, gesture);
            }
        }
    }

    /// N488: tune gesture recognition (tap/long-press/drag thresholds).
    pub fn set_gesture_config(&mut self, config: GestureConfig) {
        self.gestures.set_config(config);
    }

    /// N488: the current gesture thresholds.
    pub fn gesture_config(&self) -> GestureConfig {
        self.gestures.config()
    }

    /// N419: the highest OTA phase sent (0 none, 1 begin, 2 chunk, 3 confirm).
    pub fn ota_phase(&self) -> u8 {
        self.ota_phase
    }

    /// N728: the OTA commands still required after a reconnect, derived from the
    /// checkpoint. The device keeps its OTA state across a transport reconnect,
    /// so begin/chunk that already reached it are not resent; phase 3 means the
    /// update is already confirmed and the plan is empty.
    pub fn ota_resume_commands(
        &self,
        version: &str,
        size: Option<u64>,
        sha256: Option<String>,
    ) -> Vec<mdc_protocol::OtaCommand> {
        let begin = mdc_protocol::OtaCommand {
            action: "begin".into(),
            version: Some(version.to_string()),
            size,
            sha256: None,
            offset: None,
            len: None,
        };
        let chunk = mdc_protocol::OtaCommand {
            action: "chunk".into(),
            version: None,
            size: None,
            sha256,
            offset: None,
            len: None,
        };
        let confirm = mdc_protocol::OtaCommand {
            action: "confirm".into(),
            version: None,
            size: None,
            sha256: None,
            offset: None,
            len: None,
        };
        match self.ota_phase {
            0 => vec![begin, chunk, confirm],
            1 => vec![chunk, confirm],
            2 => vec![confirm],
            _ => Vec::new(),
        }
    }

    /// N737: the chunk ranges of an image (`chunk_size` clamped to at least 1),
    /// with the final range shortened to the image length.
    pub fn ota_chunk_ranges(total_bytes: u64, chunk_size: u32) -> Vec<(u32, u32)> {
        let total = total_bytes.min(u64::from(u32::MAX)) as u32;
        let chunk = chunk_size.max(1);
        let mut out = Vec::new();
        let mut offset = 0u32;
        while offset < total {
            let len = chunk.min(total - offset);
            out.push((offset, len));
            offset = offset.saturating_add(len);
        }
        out
    }

    /// N737: the chunk ranges not fully covered by `acknowledged`.
    pub fn ota_missing_chunks(
        total_bytes: u64,
        chunk_size: u32,
        acknowledged: &[(u32, u32)],
    ) -> Vec<(u32, u32)> {
        Self::ota_chunk_ranges(total_bytes, chunk_size)
            .into_iter()
            .filter(|(offset, len)| {
                !acknowledged.iter().any(|(ack_offset, ack_len)| {
                    *ack_offset <= *offset
                        && u64::from(*ack_offset) + u64::from(*ack_len)
                            >= u64::from(*offset) + u64::from(*len)
                })
            })
            .collect()
    }

    /// N737: like [`Self::ota_resume_commands`] but chunk-aware. The caller
    /// supplies the image size, chunk size and the ranges the device confirmed;
    /// only missing chunks are emitted (begin is skipped once phase >= 1).
    pub fn ota_resume_commands_chunked(
        &self,
        version: &str,
        total_bytes: u64,
        chunk_size: u32,
        acknowledged: &[(u32, u32)],
    ) -> Vec<mdc_protocol::OtaCommand> {
        if self.ota_phase >= 3 {
            return Vec::new();
        }
        let mut commands = Vec::new();
        if self.ota_phase == 0 {
            commands.push(mdc_protocol::OtaCommand {
                action: "begin".into(),
                version: Some(version.to_string()),
                size: Some(total_bytes),
                sha256: None,
                offset: None,
                len: None,
            });
        }
        for (offset, len) in Self::ota_missing_chunks(total_bytes, chunk_size, acknowledged) {
            commands.push(mdc_protocol::OtaCommand {
                action: "chunk".into(),
                version: None,
                size: None,
                sha256: None,
                offset: Some(offset),
                len: Some(len),
            });
        }
        commands.push(mdc_protocol::OtaCommand {
            action: "confirm".into(),
            version: None,
            size: None,
            sha256: None,
            offset: None,
            len: None,
        });
        commands
    }

    /// N419: clear the OTA checkpoint (a fresh update or an abort).
    pub fn reset_ota(&mut self) {
        self.ota_phase = 0;
    }
    /// 在同一条连接上再发 HELLO，用对端当前 Capabilities 覆盖缓存。
    /// OTA 失败后用它读回固件版本；会重置 surface 队列，调用前不要留在途帧。
    pub fn refresh_capabilities(&mut self) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let hello = encode_control(&Hello {
            major: VERSION.major,
            minor: VERSION.minor,
            max_message: MAX_FRAME_BYTES as u32,
            max_chunk: mdc_protocol::DEFAULT_MAX_CHUNK as u32,
            boot_id: self.host_boot_id,
        })
        .map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.write_packet(MessageType::Hello, hello)
    }
    /// v1 raw Scene payload is not accepted; use [`Session::set_scene`] with a
    /// structured node. Kept for ABI stability.
    pub fn send_scene(&mut self, _payload: &[u8]) -> Result<u32, CoreError> {
        let _ = (self.state, _payload);
        Err(CoreError::Unsupported)
    }
    /// N400: send a pointer/touch event to the device, gated on its `touch`
    /// capability.
    pub fn send_input(&mut self, event: mdc_protocol::InputEvent) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::Unsupported);
        }
        let touch = self
            .device
            .as_ref()
            .is_some_and(|device| device.capabilities.touch);
        if !touch {
            return Err(CoreError::Unsupported);
        }
        let payload =
            mdc_protocol::encode_control(&event).map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.write_packet(MessageType::Input, payload)
    }
    /// N54: set a vector scene, gated on the device's advertised scene limits.
    pub fn set_scene(&mut self, scene: mdc_protocol::SceneNode) -> Result<u32, CoreError> {
        let caps = self.scene_gate()?;
        if scene.count() > caps.max_nodes
            || scene.depth() > caps.max_depth
            || scene.anim_keys() > caps.max_anim_keys
        {
            return Err(CoreError::TooLarge);
        }
        let command = mdc_protocol::SceneCommand {
            action: "set".into(),
            scene: Some(scene),
        };
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.write_packet(MessageType::Scene, payload)
    }
    /// N54: clear the vector scene.
    pub fn clear_scene(&mut self) -> Result<u32, CoreError> {
        self.scene_gate()?;
        let command = mdc_protocol::SceneCommand {
            action: "clear".into(),
            scene: None,
        };
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.write_packet(MessageType::Scene, payload)
    }
    fn scene_gate(&self) -> Result<mdc_protocol::SceneCapability, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
        if device.capabilities.scene.is_disabled() {
            return Err(CoreError::Unsupported);
        }
        Ok(device.capabilities.scene.clone())
    }
    /// Form B: true when the negotiated device advertises a usable mirror mode.
    pub fn supports_mirror(&self) -> bool {
        self.device
            .as_ref()
            .map(|device| device.capabilities.mirror.supports())
            .unwrap_or(false)
    }
    /// Form B: ask the device to start streaming mirrored frames.
    /// Rejected with `Unsupported` when the device did not advertise mirror.
    pub fn start_mirror(
        &mut self,
        surface_id: &str,
        mode: &str,
        max_fps: u16,
    ) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let command = {
            let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
            negotiate_mirror(&device.capabilities, surface_id, mode, max_fps, "RGB565")
                .ok_or(CoreError::Unsupported)?
        };
        // N264: remember the negotiated mode so dirty-region frames are patched.
        self.mirror_mode = command.mode.clone();
        self.mirror_last = None;
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.write_packet(MessageType::MirrorCtrl, payload)
    }
    /// Form B: ask the device to stop streaming mirrored frames.
    pub fn stop_mirror(&mut self, surface_id: &str) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
        if !device.capabilities.mirror.supports() {
            return Err(CoreError::Unsupported);
        }
        let command = MirrorCommand {
            action: "stop".into(),
            surface_id: surface_id.to_string(),
            mode: "full-frame".into(),
            max_fps: 0,
            format: "RGB565".into(),
        };
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.mirror_mode.clear();
        self.mirror_last = None;
        self.write_packet(MessageType::MirrorCtrl, payload)
    }
    /// Form B: upload a compiled runtime package to the device (N23). The bytes
    /// are chunked into PACKAGE begin/chunk/commit messages. Rejected with
    /// `Unsupported` when the device advertised no renderer capability.
    pub fn install_package(&mut self, bytes: &[u8]) -> Result<u32, CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let renderer_ready = self
            .device
            .as_ref()
            .map(|device| device.capabilities.renderer.is_some())
            .unwrap_or(false);
        if !renderer_ready {
            return Err(CoreError::Unsupported);
        }
        if bytes.is_empty() {
            return Err(CoreError::Protocol("empty package".into()));
        }
        const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;
        if bytes.len() > MAX_PACKAGE_BYTES {
            return Err(CoreError::TooLarge);
        }
        // Check the window before BEGIN: bailing out after streaming chunks
        // would leave a half-written package in the device's staging buffer.
        if self.pending_control.len() >= MAX_CONTROL_IN_FLIGHT {
            return Err(CoreError::ControlQueueFull);
        }
        let request_id = self.alloc_request_id()?;
        if let Err(error) = self.send_package_bytes(bytes, request_id) {
            // A package is atomic. A short write or I/O error leaves an unknown
            // written prefix, so abandon the link instead of retrying chunks
            // against a device that may already hold part of the package.
            self.unsent = None;
            let _ = self.mark_transport_failure();
            return Err(error);
        }
        self.pending_control.insert(request_id);
        self.last_send_ms = self.virtual_ms;
        self.packages_sent = self.packages_sent.saturating_add(1);
        Ok(request_id)
    }

    fn send_package_bytes(&mut self, bytes: &[u8], request_id: u32) -> Result<(), CoreError> {
        let total = u32::try_from(bytes.len()).map_err(|_| CoreError::TooLarge)?;
        let checksum = package_checksum(bytes);
        // SIM-R03: BEGIN/chunks are not individual control requests. Only the
        // final COMMIT occupies the bounded control window; the device ACKs the
        // package after commit, not every chunk.
        self.write_packet_with_id(
            MessageType::Package,
            encode_package_begin(total, checksum),
            request_id,
            false,
        )?;
        let chunk_size = self
            .negotiated_max_chunk()
            .min(self.max_message.saturating_sub(32))
            .max(1);
        let mut offset: u32 = 0;
        for chunk in bytes.chunks(chunk_size) {
            self.write_packet_with_id(
                MessageType::Package,
                encode_package_chunk(offset, chunk),
                request_id,
                false,
            )?;
            offset = offset
                .checked_add(chunk.len() as u32)
                .ok_or(CoreError::TooLarge)?;
        }
        self.write_packet_with_id(
            MessageType::Package,
            encode_package_commit(),
            request_id,
            true,
        )
    }
    pub fn packages_sent(&self) -> u64 {
        self.packages_sent
    }

    /// N40: set a configuration value. Refused when the device did not advertise
    /// the key or the value is too large.
    pub fn set_config(&mut self, key: &str, value: &str) -> Result<u32, CoreError> {
        self.config_gate(key)?;
        if value.len() > MAX_CONFIG_VALUE_BYTES {
            return Err(CoreError::TooLarge);
        }
        let command = ConfigCommand {
            action: "set".into(),
            key: key.to_string(),
            value: Some(value.to_string()),
        };
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        let request_id = self.write_packet(MessageType::Config, payload)?;
        self.pending_configs.insert(key.to_string(), request_id);
        Ok(request_id)
    }

    /// N40: request a configuration value; the reply lands in `config`.
    pub fn get_config(&mut self, key: &str) -> Result<u32, CoreError> {
        self.config_gate(key)?;
        let command = ConfigCommand {
            action: "get".into(),
            key: key.to_string(),
            value: None,
        };
        let payload = encode_control(&command).map_err(|e| CoreError::Protocol(e.to_string()))?;
        let request_id = self.write_packet(MessageType::Config, payload)?;
        self.pending_configs.insert(key.to_string(), request_id);
        Ok(request_id)
    }

    /// N40: a configuration value learned from the device, if any.
    pub fn config(&self, key: &str) -> Option<&str> {
        self.config_cache.get(key).map(String::as_str)
    }

    pub fn config_values(&self) -> &BTreeMap<String, String> {
        &self.config_cache
    }

    /// N73: play a baked-atlas clip on the device (via the `animation` setting).
    pub fn set_animation(&mut self, clip: &str) -> Result<u32, CoreError> {
        self.set_config("animation", clip)
    }

    /// N73: stop the device-side animation.
    pub fn stop_animation(&mut self) -> Result<u32, CoreError> {
        self.set_config("animation", "off")
    }

    /// N73: the clip the device reported as active, if any.
    pub fn active_animation(&self) -> Option<&str> {
        self.config("animation").filter(|value| *value != "off")
    }

    /// N833: blend up to four clips (`"idle:700,wave:300"`). The device replaces
    /// the single clip; the cache follows the accepted reply.
    pub fn set_animation_blend(&mut self, blend: &str) -> Result<u32, CoreError> {
        self.set_config("animation_blend", blend)
    }

    /// N833: stop blending and leave the device with no running clip.
    pub fn stop_animation_blend(&mut self) -> Result<u32, CoreError> {
        self.set_config("animation_blend", "off")
    }

    /// N833: the blend string the device reported, if any (`off` is hidden).
    pub fn active_animation_blend(&self) -> Option<&str> {
        self.config("animation_blend")
            .filter(|value| *value != "off")
    }

    /// N83: stop any running animation so the device presents its scene (if one
    /// was set). The scene itself is unchanged.
    pub fn show_scene(&mut self) -> Result<u32, CoreError> {
        self.stop_animation()
    }

    fn config_gate(&self, key: &str) -> Result<(), CoreError> {
        if self.state != ConnectionState::Ready {
            return Err(CoreError::NotReady);
        }
        let device = self.device.as_ref().ok_or(CoreError::NotReady)?;
        if !device.capabilities.config.supports(key) {
            return Err(CoreError::Unsupported);
        }
        Ok(())
    }

    fn handle_config(&mut self, packet: &Packet) -> Result<(), CoreError> {
        if self.state != ConnectionState::Ready {
            return Ok(());
        }
        let reply: ConfigReply = match mdc_protocol::decode_control(&packet.payload) {
            Ok(reply) => reply,
            Err(_) => return Ok(()),
        };
        // SIM-R04: the device replies with request_id 0, so bind the reply to
        // the pending key instead of trusting any Config packet.
        let Some(request_id) = self.pending_configs.remove(&reply.key) else {
            return Ok(());
        };
        self.release_request(request_id);
        if reply.ok {
            if let Some(value) = reply.value {
                // N833: a successful animation reply means the device replaced
                // the other selection; mirror that in the cache. A rejected set
                // produces no reply here, so the old value survives.
                if reply.key == "animation" {
                    self.config_cache
                        .insert("animation_blend".to_string(), "off".to_string());
                } else if reply.key == "animation_blend" {
                    self.config_cache
                        .insert("animation".to_string(), "off".to_string());
                }
                self.config_cache.insert(reply.key, value);
            }
        }
        Ok(())
    }
    /// Take the latest complete mirrored frame, if any. The inbox holds at most
    /// one frame; a newer frame replaces an unread one (latest-frame-wins).
    pub fn take_mirror_frame(&mut self) -> Option<MirrorFrame> {
        self.mirror_inbox.take()
    }
    /// N559: the negotiated mirror mode (`"full-frame"` | `"dirty-region"`; empty
    /// before the device confirms a `MIRRORCTRL start`).
    pub fn mirror_mode(&self) -> &str {
        &self.mirror_mode
    }
    /// N24: mirror counters for a host/inspector without draining the inbox.
    pub fn mirror_stats(&self) -> MirrorStats {
        MirrorStats {
            frames_received: self.mirror_frames,
            frames_dropped: self.mirror_dropped,
            last_frame_id: self.mirror_last_frame_id,
        }
    }
    pub fn mirror_frames_received(&self) -> u64 {
        self.mirror_frames
    }
    pub fn mirror_frames_dropped(&self) -> u64 {
        self.mirror_dropped
    }
    fn handle_mirror_frame(&mut self, packet: Packet) -> Result<(), CoreError> {
        if self.state != ConnectionState::Ready {
            return Ok(());
        }
        let decoded = match decode_frame_payload(&packet.payload) {
            Ok(value) => value,
            Err(_) => {
                self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                return Ok(());
            }
        };
        match decoded {
            FramePayload::Legacy {
                surface_id,
                frame_id,
                width,
                height,
                bytes,
            } => {
                self.store_mirror_frame(surface_id, frame_id, width, height, bytes)?;
            }
            FramePayload::Begin {
                surface_id,
                frame_id,
                width,
                height,
                total_bytes,
            } => {
                let max_chunk = self.negotiated_max_chunk();
                match FrameAssembler::begin(frame_id, total_bytes as usize, max_chunk) {
                    Ok(assembler) => {
                        self.mirror_assembler = Some((surface_id, width, height, assembler));
                    }
                    Err(_) => {
                        self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                        self.mirror_assembler = None;
                    }
                }
            }
            FramePayload::Chunk {
                frame_id,
                offset,
                data,
            } => {
                let ok = match self.mirror_assembler.as_mut() {
                    Some((_, _, _, assembler)) if assembler.frame_id == frame_id => {
                        assembler.chunk(offset as usize, &data).is_ok()
                    }
                    _ => false,
                };
                if !ok {
                    self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                }
            }
            FramePayload::Commit { frame_id } => {
                let taken = match self.mirror_assembler.take() {
                    Some((surface_id, width, height, assembler))
                        if assembler.frame_id == frame_id =>
                    {
                        Some((surface_id, width, height, assembler))
                    }
                    _ => None,
                };
                match taken {
                    Some((surface_id, width, height, assembler)) => {
                        let expected = assembler.expected_len;
                        let (ranges, staging) = assembler.into_ranges();
                        let complete = ranges.len() == 1 && ranges[0] == (0, expected);
                        if self.mirror_mode == "dirty-region" {
                            let base = self.mirror_last.as_ref().filter(|(s, lw, lh, last)| {
                                *s == surface_id
                                    && *lw == width
                                    && *lh == height
                                    && last.len() == expected
                            });
                            match base {
                                Some((_, _, _, last)) => {
                                    let mut buffer = last.clone();
                                    for (start, end) in &ranges {
                                        if *end <= buffer.len() && *end <= staging.len() {
                                            buffer[*start..*end]
                                                .copy_from_slice(&staging[*start..*end]);
                                        }
                                    }
                                    self.store_mirror_frame(
                                        surface_id, frame_id, width, height, buffer,
                                    )?;
                                }
                                None if complete => {
                                    self.store_mirror_frame(
                                        surface_id, frame_id, width, height, staging,
                                    )?;
                                }
                                None => {
                                    self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                                }
                            }
                        } else if complete {
                            self.store_mirror_frame(surface_id, frame_id, width, height, staging)?;
                        } else {
                            self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                        }
                    }
                    None => {
                        self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                    }
                }
            }
        }
        Ok(())
    }
    fn store_mirror_frame(
        &mut self,
        surface_id: String,
        frame_id: u64,
        width: u16,
        height: u16,
        bytes: Vec<u8>,
    ) -> Result<(), CoreError> {
        let expected = match mdc_protocol::rgb565_bytes(width, height) {
            Ok(value) => value,
            Err(_) => {
                self.mirror_dropped = self.mirror_dropped.saturating_add(1);
                return Ok(());
            }
        };
        let known_surface = self
            .device
            .as_ref()
            .map(|device| {
                device
                    .capabilities
                    .surfaces
                    .iter()
                    .any(|s| s.id == surface_id && s.width == width && s.height == height)
            })
            .unwrap_or(false);
        if bytes.len() != expected || expected > MAX_FRAME_BYTES || !known_surface {
            self.mirror_dropped = self.mirror_dropped.saturating_add(1);
            return Ok(());
        }
        if self.mirror_inbox.is_some() {
            // Latest-frame-wins: an unread older frame is dropped, not queued.
            self.mirror_dropped = self.mirror_dropped.saturating_add(1);
        }
        self.mirror_frames = self.mirror_frames.saturating_add(1);
        self.mirror_last_frame_id = frame_id;
        // N264: keep the frame so a later dirty-region update can patch it.
        self.mirror_last = Some((surface_id.clone(), width, height, bytes.clone()));
        let notify_surface = surface_id.clone();
        self.mirror_inbox = Some(MirrorFrame {
            surface_id,
            frame_id,
            width,
            height,
            bytes,
        });
        if let Some(manager) = &self.manager {
            let device_id = self
                .device
                .as_ref()
                .map(|device| device.capabilities.device_id.clone())
                .unwrap_or_default();
            if let Ok(mut manager) = manager.lock() {
                manager.push_mirror(
                    device_id,
                    notify_surface,
                    frame_id,
                    width,
                    height,
                    self.mirror_frames,
                    self.mirror_dropped,
                );
            }
        }
        Ok(())
    }
    fn alloc_request_id(&mut self) -> Result<u32, CoreError> {
        let id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(CoreError::Protocol("request ID exhausted".into()))?;
        Ok(id)
    }
    pub(crate) fn write_packet(
        &mut self,
        kind: MessageType,
        payload: Vec<u8>,
    ) -> Result<u32, CoreError> {
        if self.pending_control.len() >= MAX_CONTROL_IN_FLIGHT {
            return Err(CoreError::ControlQueueFull);
        }
        let id = self.alloc_request_id()?;
        self.write_packet_with_id(kind, payload, id, true)?;
        self.pending_control.insert(id);
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
        // N398: count outbound packets/bytes.
        self.stats.packets_sent = self.stats.packets_sent.saturating_add(1);
        self.stats.bytes_sent = self.stats.bytes_sent.saturating_add(bytes.len() as u64);
        let write_result = self.write_bytes(&bytes);
        if matches!(write_result, Ok(()) | Err(CoreError::ShortWrite)) && track {
            self.pending_requests.insert(id);
            if self.config.ack_timeout_ms > 0 {
                self.request_deadlines.insert(
                    id,
                    self.virtual_ms.saturating_add(self.config.ack_timeout_ms),
                );
            }
            self.last_send_ms = self.virtual_ms;
        }
        write_result
    }
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), CoreError> {
        match self.transport.write(bytes) {
            Ok(()) => Ok(()),
            Err(mdc_transport::TransportError::ShortWrite { accepted }) => {
                let accepted = accepted.min(bytes.len());
                if accepted < bytes.len() {
                    self.unsent = Some(bytes[accepted..].to_vec());
                }
                Err(CoreError::ShortWrite)
            }
            Err(error) => Err(CoreError::Transport(error.to_string())),
        }
    }
    fn handle_packet(&mut self, packet: Packet) -> Result<(), CoreError> {
        match packet.kind {
            MessageType::Capabilities => {
                if self.state == ConnectionState::Handshaking {
                    if self.hello_request != Some(packet.request_id) {
                        return Err(CoreError::Protocol("unexpected capabilities reply".into()));
                    }
                } else if self.state == ConnectionState::Ready {
                    // A device may refresh capabilities after the handshake; a
                    // stale run's packet cannot update the current session.
                    if packet.request_id < self.request_floor {
                        return Ok(());
                    }
                } else {
                    return Err(CoreError::Protocol("capabilities before handshake".into()));
                }
                self.release_request(packet.request_id);
                self.hello_request = None;
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
                self.missed_heartbeats = 0;
                self.heartbeat_outstanding = false;
                self.heartbeat_request = None;
                // N304: a changed device boot id discards the previous run's
                // pending ACKs before the new run is adopted.
                self.adopt_device_identity(&caps);
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
            MessageType::Pong => {
                if packet.request_id < self.request_floor {
                    return Ok(());
                }
                // SIM-R04: an unsolicited Pong must not prove a dead peer alive.
                if !self.heartbeat_outstanding || self.heartbeat_request != Some(packet.request_id)
                {
                    return Ok(());
                }
                self.heartbeat_outstanding = false;
                self.heartbeat_request = None;
                self.missed_heartbeats = 0;
                Ok(())
            }
            MessageType::Ack => {
                if packet.request_id < self.request_floor
                    || self.finished_acks.contains(&packet.request_id)
                {
                    return Ok(());
                }
                let ack: Ack = mdc_protocol::decode_control(&packet.payload)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                // N304: ignore an ACK stamped with a previous run id.
                if ack.run_id != 0 && self.device_run_id != 0 && ack.run_id != self.device_run_id {
                    return Ok(());
                }
                if self.pending_requests.contains(&packet.request_id)
                    && (ack.received || ack.displayed)
                {
                    let waiting_frame = self.pending_frames.contains_key(&packet.request_id);
                    if waiting_frame && !ack.displayed {
                        // received 只表示对端收过。待发帧和 needs_full_frame 都要等 displayed。
                        self.frame_received_at
                            .entry(packet.request_id)
                            .or_insert_with(std::time::Instant::now);
                        return Ok(());
                    }
                    self.record_finished_ack(packet.request_id);
                    self.stats.acks_received = self.stats.acks_received.saturating_add(1);
                    self.note_displayed(packet.request_id);
                    self.release_request(packet.request_id);
                    let was_frame = self.pending_frames.contains_key(&packet.request_id);
                    if let Some((surface_id, frame_id)) =
                        self.pending_frames.remove(&packet.request_id)
                    {
                        self.release_held(&surface_id, frame_id);
                        self.launch_pending_frame(&surface_id, frame_id)?;
                    } else if let Some(device) = self.device.as_mut() {
                        let ids: Vec<_> = device
                            .surfaces
                            .iter()
                            .map(|surface| surface.info.id.clone())
                            .collect();
                        for surface_id in ids {
                            self.launch_pending_frame(&surface_id, ack.frame_id)?;
                        }
                    }
                    if was_frame && ack.displayed {
                        self.needs_full_frame = false;
                    }
                }
                Ok(())
            }
            MessageType::Error => {
                if packet.request_id < self.request_floor {
                    return Ok(());
                }
                self.release_request(packet.request_id);
                if let Some((surface_id, frame_id)) = self.pending_frames.remove(&packet.request_id)
                {
                    self.release_held(&surface_id, frame_id);
                    self.launch_pending_frame(&surface_id, frame_id)?;
                }
                let detail = mdc_protocol::decode_control::<ErrorPayload>(&packet.payload)
                    .map(|err| format!("{}: {}", err.code, err.message))
                    .unwrap_or_else(|_| "device returned error".into());
                Err(CoreError::Protocol(detail))
            }
            MessageType::Input => {
                if self.state != ConnectionState::Ready {
                    return Ok(());
                }
                if packet.request_id != 0 && packet.request_id < self.request_floor {
                    return Ok(());
                }
                let event: InputEvent = mdc_protocol::decode_control(&packet.payload)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                let device_id = self
                    .device
                    .as_ref()
                    .map(|d| d.capabilities.device_id.clone())
                    .unwrap_or_default();
                // N473/N735: feed the gesture tracker before forwarding the raw
                // event; a two-finger move may emit both a pan and a pinch.
                let gestures = self.gestures.on_events(&event, self.virtual_ms);
                if let Some(mgr) = &self.manager {
                    mgr.lock()
                        .map_err(|e| CoreError::Transport(e.to_string()))?
                        .push_input(device_id, event);
                }
                for gesture in gestures {
                    self.emit_gesture(gesture);
                }
                Ok(())
            }
            MessageType::Ota => {
                // Device-originated OTA status replies are acknowledged silently;
                // host-initiated OTA uses send_ota + Ack/Error.
                Ok(())
            }
            MessageType::MirrorFrame => self.handle_mirror_frame(packet),
            MessageType::Config => self.handle_config(&packet),
            MessageType::Metric => {
                // N316: a metric is a diagnostic record, never an ACK/error for a
                // frame transaction.
                let metric: MetricReport = mdc_protocol::decode_control(&packet.payload)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                self.note_run_identity(metric.boot_id, metric.run_id);
                // N547: fold the device's applied-tile counter into the snapshot.
                if metric.name == TILES_RECEIVED_METRIC {
                    self.device_tiles_received = metric.value.max(0.0) as u64;
                }
                self.push_diagnostic(DiagnosticRecord::Metric(metric));
                Ok(())
            }
            MessageType::BootInfo => {
                let boot: BootInfo = mdc_protocol::decode_control(&packet.payload)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                self.note_run_identity(boot.boot_id, boot.run_id);
                self.push_diagnostic(DiagnosticRecord::BootInfo(boot));
                Ok(())
            }
            MessageType::Telemetry => {
                // N335: the reserved telemetry type now carries a structured event.
                let event: mdc_protocol::TelemetryEvent =
                    mdc_protocol::decode_control(&packet.payload)
                        .map_err(|e| CoreError::Protocol(e.to_string()))?;
                self.note_run_identity(event.boot_id, event.run_id);
                // N357: also surface a metadata-only manager event.
                if let Some(mgr) = &self.manager {
                    let device_id = self
                        .device
                        .as_ref()
                        .map(|d| d.capabilities.device_id.clone())
                        .unwrap_or_default();
                    if let Ok(mut manager) = mgr.lock() {
                        manager.push_telemetry(device_id, event.name.clone(), event.level.clone());
                    }
                }
                self.push_diagnostic(DiagnosticRecord::Telemetry(event));
                Ok(())
            }
            MessageType::Crash => {
                // N367: the reserved crash type now carries a fault report.
                let report: mdc_protocol::CrashReport =
                    mdc_protocol::decode_control(&packet.payload)
                        .map_err(|e| CoreError::Protocol(e.to_string()))?;
                self.note_run_identity(report.boot_id, report.run_id);
                self.push_diagnostic(DiagnosticRecord::Crash(report));
                Ok(())
            }
            MessageType::MirrorCtrl => Ok(()),
            // Reserved diagnostics without a payload (N303) are ignored.
            _ => Ok(()),
        }
    }
}
