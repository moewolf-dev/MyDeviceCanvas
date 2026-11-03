//! Session/state helpers shared by the N-API manager.
//!
//! Owns the in-process FakeDevice pairing, handshake pumping and manager event
//! draining; contains no DTO definition.
use super::*;

pub(crate) struct SimSession {
    pub(crate) session: Session<Box<dyn Transport>>,
    pub(crate) device: Option<FakeDevice>,
    pub(crate) manager: Arc<Mutex<DeviceManager>>,
    /// SIM-R05: set by `dispose`; long handshakes must stop promptly.
    pub(crate) cancelled: Arc<AtomicBool>,
    /// SIM-R04: real-clock origin for the Session tick owner.
    pub(crate) created: Instant,
}

/// SIM-R04/R08: Node drives deadlines from a monotonic clock instead of a
/// caller-supplied tick. Reconnect is disabled so a dead peer reaches a
/// terminal error rather than waiting for a transport the SDK cannot supply.
pub(crate) fn node_session_config() -> SessionConfig {
    SessionConfig {
        reconnect: false,
        ..SessionConfig::default()
    }
}

pub(crate) fn tick_session(sim: &mut SimSession) -> Result<()> {
    let now_ms = sim.created.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    sim.session
        .tick(now_ms)
        .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(())
}

/// A peer that never answers HELLO is rejected after this budget instead of
/// looping for tens of seconds.
pub(crate) const HANDSHAKE_BUDGET: Duration = Duration::from_millis(2_000);

pub(crate) fn pump_link(sim: &mut SimSession) -> Result<()> {
    tick_session(sim)?;
    let rounds = if sim.device.is_some() { 1 } else { 30 };
    let started = Instant::now();
    for _ in 0..rounds {
        if sim.cancelled.load(AtomicOrdering::Acquire) {
            return Err(Error::from_reason("operation cancelled"));
        }
        if let Some(device) = sim.device.as_mut() {
            let device_result = device.poll();
            sim.session
                .poll()
                .map_err(|e| Error::from_reason(e.to_string()))?;
            if let Err(error) = device_result {
                if !matches!(error, SimError::Disconnected) {
                    return Err(Error::from_reason(error.to_string()));
                }
            }
        } else {
            sim.session
                .poll()
                .map_err(|e| Error::from_reason(e.to_string()))?;
        }
        if sim.device.is_none()
            && sim.session.state == ConnectionState::Ready
            && sim.session.pending_request_count() == 0
        {
            break;
        }
        if sim.device.is_none() {
            if started.elapsed() >= HANDSHAKE_BUDGET {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) disposed: bool,
    /// SIM-R05: once a real endpoint was requested, a failed HELLO must not
    /// silently fall back to an in-process FakeDevice.
    pub(crate) connection_attempted: bool,
    /// SIM-R04: incremented for every new Session; shared with NativeManager.
    pub(crate) generation: Arc<AtomicU32>,
    pub(crate) board_id: Option<String>,
    pub(crate) sim: Option<SimSession>,
    pub(crate) last_frame: Option<Vec<u8>>,
    /// N528: mirrored frames dropped because the caller's cap was too small.
    pub(crate) mirror_skips: u64,
    /// CORE-R06: the serial path this manager holds, and the matching process
    /// lease. Both are released when the session is replaced or disposed.
    pub(crate) serial_path: Option<String>,
    pub(crate) serial_lease: Option<PortLease>,
}

/// CORE-R06: reject JS numbers that cannot name a real image length.
pub(crate) fn ensure_sim(state: &mut State) -> Result<()> {
    if state.sim.is_some() {
        return Ok(());
    }
    if state.connection_attempted {
        return Err(Error::from_reason(
            "no active connection; the previous endpoint failed",
        ));
    }
    state.generation.fetch_add(1, AtomicOrdering::AcqRel);
    let profile = BoardProfile::from_board_id(state.board_id.as_deref().unwrap_or("default"));
    let (host, mut device) = FakeDevice::pair(profile);
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session =
        Session::with_config(Box::new(host) as Box<dyn Transport>, node_session_config());
    session.set_manager(manager.clone());
    session
        .connect()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    device
        .poll()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    session
        .poll()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    if session.state != ConnectionState::Ready {
        return Err(Error::from_reason("sim session not ready"));
    }
    if let Some(dev) = session.device.as_ref() {
        let mut mgr = manager
            .lock()
            .map_err(|_| Error::from_reason("manager lock poisoned"))?;
        let _ = mgr.upsert(dev.capabilities.clone());
        mgr.connect_session(&dev.capabilities.device_id, "memory:sim");
    }
    state.sim = Some(SimSession {
        session,
        device: Some(device),
        manager,
        cancelled: Arc::new(AtomicBool::new(false)),
        created: Instant::now(),
    });
    Ok(())
}

/// N340: wire a fresh transport into a session, handshake it, and register the endpoint.
/// `serial` carries the path plus the process lease when this is a serial link.
pub(crate) fn connect_transport(
    state: &mut State,
    transport: Box<dyn Transport>,
    endpoint: &str,
    label: &str,
    cancelled: &Arc<AtomicBool>,
    serial: Option<(String, PortLease)>,
) -> Result<()> {
    if let Some(mut previous) = state.sim.take() {
        previous.session.disconnect();
    }
    state.serial_path = None;
    state.serial_lease = None;
    let manager = Arc::new(Mutex::new(DeviceManager::new()));
    let mut session = Session::with_config(transport, node_session_config());
    session.set_manager(manager.clone());
    session
        .connect()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    let mut live = SimSession {
        session,
        device: None,
        manager,
        cancelled: cancelled.clone(),
        created: Instant::now(),
    };
    pump_link(&mut live)?;
    if live.session.state != ConnectionState::Ready {
        return Err(Error::from_reason(format!(
            "{label} peer did not finish HELLO"
        )));
    }
    if let Some(device) = live.session.device.as_ref() {
        live.manager
            .lock()
            .map_err(|_| Error::from_reason("manager lock poisoned"))?
            .connect_session(&device.capabilities.device_id, endpoint);
    }
    state.generation.fetch_add(1, AtomicOrdering::AcqRel);
    state.sim = Some(live);
    if let Some((path, lease)) = serial {
        state.serial_path = Some(path);
        state.serial_lease = Some(lease);
    }
    Ok(())
}

pub(crate) fn drain_manager_events(
    manager: &Arc<Mutex<DeviceManager>>,
    generation: u32,
) -> Result<Vec<NativeEvent>> {
    let mut mgr = manager
        .lock()
        .map_err(|_| Error::from_reason("manager lock poisoned"))?;
    let mut out = Vec::new();
    for event in mgr.drain_events() {
        out.push(match event {
            ManagerEvent::Added(id) | ManagerEvent::Connected(id) => NativeEvent {
                generation,
                kind: "connected".into(),
                gesture: None,
                device_id: Some(id),
                input: None,
                message: None,
                mirror: None,
            },
            ManagerEvent::Removed(id) | ManagerEvent::Disconnected(id) => NativeEvent {
                generation,
                kind: "disconnected".into(),
                gesture: None,
                device_id: Some(id),
                input: None,
                message: None,
                mirror: None,
            },
            ManagerEvent::EndpointSwitched { device_id } => NativeEvent {
                generation,
                kind: "endpointSwitched".into(),
                gesture: None,
                device_id: Some(device_id),
                input: None,
                message: None,
                mirror: None,
            },
            ManagerEvent::Input { device_id, event } => NativeEvent {
                generation,
                kind: "input".into(),
                gesture: None,
                device_id: Some(device_id.clone()),
                input: Some(InputEventDto {
                    device_id,
                    surface_id: event.surface_id,
                    pointer_id: event.pointer_id,
                    phase: event.phase,
                    x: event.x,
                    y: event.y,
                    node: event.node,
                }),
                message: None,
                mirror: None,
            },
            ManagerEvent::Gesture { device_id, gesture } => NativeEvent {
                generation,
                kind: "gesture".into(),
                gesture: Some(GestureDto {
                    kind: match gesture.kind {
                        mdc_core::GestureKind::Tap => "tap".into(),
                        mdc_core::GestureKind::LongPress => "long-press".into(),
                        mdc_core::GestureKind::Drag => "drag".into(),
                        mdc_core::GestureKind::Pinch => "pinch".into(),
                        mdc_core::GestureKind::Pan => "pan".into(),
                        mdc_core::GestureKind::Swipe => "swipe".into(),
                        mdc_core::GestureKind::DoubleTap => "double-tap".into(),
                        mdc_core::GestureKind::TripleTap => "triple-tap".into(),
                    },
                    pointer_id: gesture.pointer_id,
                    x: gesture.x,
                    y: gesture.y,
                    node: gesture.node,
                    scale: gesture.scale,
                    rotation: gesture.rotation,
                    dx: gesture.dx,
                    dy: gesture.dy,
                }),
                device_id: Some(device_id),
                input: None,
                message: None,
                mirror: None,
            },
            ManagerEvent::Error { device_id, message } => NativeEvent {
                generation,
                kind: "error".into(),
                gesture: None,
                device_id: Some(device_id),
                input: None,
                message: Some(message),
                mirror: None,
            },
            ManagerEvent::Telemetry {
                device_id,
                name,
                level,
            } => NativeEvent {
                generation,
                // N357: telemetry metadata (the full record is in takeDiagnostics).
                kind: "telemetry".into(),
                gesture: None,
                device_id: Some(device_id),
                input: None,
                message: Some(match level {
                    Some(level) => format!("{level}: {name}"),
                    None => name,
                }),
                mirror: None,
            },
            ManagerEvent::Mirror {
                device_id,
                surface_id,
                frame_id,
                width,
                height,
                ..
            } => NativeEvent {
                generation,
                kind: "mirror".into(),
                gesture: None,
                device_id: Some(device_id),
                input: None,
                message: None,
                mirror: Some(MirrorInfoDto {
                    surface_id,
                    frame_id: frame_id as u32,
                    width,
                    height,
                }),
            },
        });
    }
    Ok(out)
}
