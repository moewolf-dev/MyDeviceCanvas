//! Node native bindings: MemoryLink FakeDevice session (sim path).
use mdc_core::{ConnectionState, DeviceManager, Frame, ManagerEvent, Session, SessionConfig, Tile};
use mdc_flasher::{flags_for_board, EspToolFlasher, SimFlasher};
use mdc_protocol::InputEvent;
use mdc_provision::{
    known_board_facts, preflight_verdict_bytes, Artifact, BoardFacts, FlashSize, Flasher,
    ImageGeometry, InstallError, InstallPlan, InstallState, Installer, PortLease, PortLeases,
    PreflightInputBytes, PreflightVerdict,
};
use mdc_simulator::{BoardProfile, FakeDevice, SimError};
use mdc_transport::{TcpTransport, Transport};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// SIM-R06/CORE-R06: one process-wide lease registry so a firmware install and
/// an open serial session cannot hold the same port at the same time.
static FIRMWARE_LEASES: LazyLock<PortLeases> = LazyLock::new(PortLeases::default);
static FIRMWARE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[napi(object)]
pub struct SurfaceInfo {
    pub id: String,
    pub width: u16,
    pub height: u16,
    #[napi(js_name = "pixelFormat")]
    pub pixel_format: String,
    /// N307: only RGB565 is implemented on the host/frame path.
    pub implemented: bool,
    pub stride: u32,
    pub rotation: u16,
}

#[napi(object)]
pub struct DeviceInfo {
    #[napi(js_name = "deviceId")]
    pub device_id: String,
    pub firmware: String,
    pub surfaces: Vec<SurfaceInfo>,
}

#[napi(object)]
pub struct CapabilitiesInfo {
    #[napi(js_name = "deviceId")]
    pub device_id: String,
    #[napi(js_name = "boardId")]
    pub board_id: String,
    #[napi(js_name = "protocolVersion")]
    pub protocol_version: u16,
    pub firmware: String,
    pub surfaces: Vec<SurfaceInfo>,
    pub frame: bool,
    pub tile: bool,
    pub touch: bool,
    pub ota: bool,
    #[napi(js_name = "maxMessage")]
    pub max_message: u32,
    #[napi(js_name = "maxChunk")]
    pub max_chunk: u32,
    #[napi(js_name = "maxInFlight")]
    pub max_in_flight: u16,
    #[napi(js_name = "maxFps")]
    pub max_fps: u16,
    pub mirror: bool,
    #[napi(js_name = "mirrorModes")]
    pub mirror_modes: Vec<String>,
    #[napi(js_name = "mirrorMaxFps")]
    pub mirror_max_fps: u16,
    #[napi(js_name = "localDisplayFps")]
    pub local_display_fps: u16,
    #[napi(js_name = "mirrorFps")]
    pub mirror_fps: u16,
    #[napi(js_name = "mirrorBandwidth")]
    pub mirror_bandwidth: f64,
    #[napi(js_name = "configKeys")]
    pub config_keys: Vec<String>,
    pub scene: bool,
    #[napi(js_name = "sceneMaxNodes")]
    pub scene_max_nodes: u32,
    #[napi(js_name = "sceneMaxDepth")]
    pub scene_max_depth: u32,
    #[napi(js_name = "bootId")]
    pub boot_id: f64,
    #[napi(js_name = "runId")]
    pub run_id: f64,
}

#[napi(object)]
pub struct SerialPortDto {
    pub path: String,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    #[napi(js_name = "serialNumber")]
    pub serial_number: Option<String>,
}

/// CORE-R06: firmware metadata mirrored from the signed catalog artifact.
/// `image` itself travels as a Buffer so the App never builds a flash command.
#[napi(object)]
#[derive(Clone)]
pub struct FirmwareArtifactDto {
    #[napi(js_name = "boardId")]
    pub board_id: String,
    pub mcu: String,
    pub runtime: String,
    #[napi(js_name = "protocolMajor")]
    pub protocol_major: u32,
    #[napi(js_name = "protocolMinor")]
    pub protocol_minor: u32,
    /// Image length in bytes (JS number; checked below).
    #[napi(js_name = "imageSize")]
    pub image_size: f64,
    pub sha256: String,
    pub source: String,
    #[napi(js_name = "publicKey")]
    pub public_key: Option<Buffer>,
    pub signature: Option<Buffer>,
}

#[napi(object)]
#[derive(Clone)]
pub struct FirmwareBoardFactsDto {
    #[napi(js_name = "boardId")]
    pub board_id: String,
    pub mcu: String,
    /// Physical flash bytes; `null`/omitted means Unknown (never guessed).
    #[napi(js_name = "flashBytes")]
    pub flash_bytes: Option<f64>,
    #[napi(js_name = "profileVerified")]
    pub profile_verified: bool,
}

#[napi(object)]
#[derive(Clone)]
pub struct FirmwareImageGeometryDto {
    #[napi(js_name = "chipFamily")]
    pub chip_family: String,
    #[napi(js_name = "flashBytes")]
    pub flash_bytes: f64,
}

#[napi(object)]
#[derive(Clone)]
pub struct FirmwarePreflightRequest {
    pub image: Buffer,
    pub artifact: FirmwareArtifactDto,
    pub board: FirmwareBoardFactsDto,
    #[napi(js_name = "hostProtocolMajor")]
    pub host_protocol_major: u32,
    #[napi(js_name = "hostProtocolMinor")]
    pub host_protocol_minor: u32,
    #[napi(js_name = "detectedChipFamily")]
    pub detected_chip_family: Option<String>,
    #[napi(js_name = "imageGeometry")]
    pub image_geometry: Option<FirmwareImageGeometryDto>,
    #[napi(js_name = "allowUnknownFlash")]
    pub allow_unknown_flash: Option<bool>,
    #[napi(js_name = "flashIdUnusable")]
    pub flash_id_unusable: Option<bool>,
}

#[napi(object)]
pub struct FirmwarePreflightResult {
    pub ok: bool,
    /// `ok` | `okUnknownFlash` | `reject`
    pub verdict: String,
    /// Rejection reason (`BoardMismatch`, `HashMismatch`, ...) when not ok.
    pub reason: Option<String>,
}

#[napi(object)]
#[derive(Clone)]
pub struct FirmwareInstallRequest {
    pub preflight: FirmwarePreflightRequest,
    pub port: String,
    #[napi(js_name = "expectedDeviceId")]
    pub expected_device_id: String,
    /// `sim` (never touches the port) or `esptool` (host-side writer).
    pub mode: String,
    #[napi(js_name = "esptoolPath")]
    pub esptool_path: Option<String>,
    /// esptool only: default true. A dry run records argv but writes nothing.
    #[napi(js_name = "dryRun")]
    pub dry_run: Option<bool>,
    /// Simulator only: delay the fake write so progress/cancel can be observed.
    #[napi(js_name = "simDelayMs")]
    pub sim_delay_ms: Option<u32>,
    /// Simulator only: identity the fake flasher reads back. Defaults to
    /// `expectedDeviceId`; a different value exercises the mismatch path.
    #[napi(js_name = "simReadbackDeviceId")]
    pub sim_readback_device_id: Option<String>,
}

#[napi(object)]
#[derive(Clone, Default)]
pub struct FirmwareProgressDto {
    pub phase: String,
    /// 0..1 when the phase has a meaningful position, else `null`.
    pub ratio: Option<f64>,
    pub error: Option<String>,
}

#[napi(object)]
pub struct FirmwareInstallResult {
    pub ok: bool,
    pub phase: String,
    #[napi(js_name = "deviceId")]
    pub device_id: Option<String>,
    pub error: Option<String>,
}

#[napi(object)]
#[derive(Clone)]
pub struct FirmwareReadbackRequest {
    pub port: String,
    #[napi(js_name = "timeoutMs")]
    pub timeout_ms: Option<u32>,
    pub baud: Option<u32>,
}

#[napi(object)]
pub struct FirmwareReadbackResult {
    #[napi(js_name = "deviceId")]
    pub device_id: String,
    #[napi(js_name = "boardId")]
    pub board_id: String,
    #[napi(js_name = "protocolVersion")]
    pub protocol_version: u16,
    pub firmware: String,
}

/// CORE-R06: host-side board facts used to build a firmware preflight request.
/// `known: false` means the id is not in this SDK's table; the caller must not
/// guess MCU/flash values.
#[napi(object)]
pub struct BoardFactsDto {
    #[napi(js_name = "boardId")]
    pub board_id: String,
    pub mcu: String,
    #[napi(js_name = "flashBytes")]
    pub flash_bytes: Option<f64>,
    #[napi(js_name = "profileVerified")]
    pub profile_verified: bool,
    pub known: bool,
}

#[napi(object)]
pub struct TileRequest {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    /// Frame id as JS number (protocol uses u64; sim ids stay in u32 range).
    #[napi(js_name = "baseFrameId")]
    pub base_frame_id: u32,
    #[napi(js_name = "surfaceId")]
    pub surface_id: String,
    pub bytes: Buffer,
}

/// N455: host-side session counters.
#[napi(object)]
pub struct SessionStatsDto {
    #[napi(js_name = "packetsSent")]
    pub packets_sent: i64,
    #[napi(js_name = "bytesSent")]
    pub bytes_sent: i64,
    #[napi(js_name = "bytesReceived")]
    pub bytes_received: i64,
    #[napi(js_name = "acksReceived")]
    pub acks_received: i64,
    #[napi(js_name = "framesSent")]
    pub frames_sent: i64,
    #[napi(js_name = "droppedFrames")]
    pub dropped_frames: i64,
    #[napi(js_name = "packagesSent")]
    pub packages_sent: i64,
    #[napi(js_name = "mirrorFrames")]
    pub mirror_frames: i64,
    #[napi(js_name = "pacingDropped")]
    pub pacing_dropped: i64,
}

/// N576: the host's partial-update reconciliation against the device.
#[napi(object)]
pub struct TileReconciliationDto {
    pub sent: i64,
    pub received: i64,
    pub missing: i64,
    #[napi(js_name = "bytesSent")]
    pub bytes_sent: i64,
    pub consistent: bool,
}

#[napi(object)]
pub struct InputEventDto {
    #[napi(js_name = "deviceId")]
    pub device_id: String,
    #[napi(js_name = "surfaceId")]
    pub surface_id: String,
    #[napi(js_name = "pointerId")]
    pub pointer_id: u16,
    pub phase: String,
    pub x: u16,
    pub y: u16,
    /// N418: the hit-tested scene node, when the device reports one.
    pub node: Option<String>,
}

#[napi(object)]
pub struct DiagnosticDto {
    #[napi(js_name = "kind")]
    pub kind: String,
    pub name: String,
    pub value: f64,
    pub unit: Option<String>,
    /// N349: JSON of the telemetry fields (empty for metric/boot info).
    pub fields: Option<String>,
}

#[napi(object)]
pub struct MirrorInfoDto {
    #[napi(js_name = "surfaceId")]
    pub surface_id: String,
    #[napi(js_name = "frameId")]
    pub frame_id: u32,
    pub width: u16,
    pub height: u16,
}

#[napi(object)]
pub struct MirrorFrameDto {
    #[napi(js_name = "surfaceId")]
    pub surface_id: String,
    #[napi(js_name = "frameId")]
    pub frame_id: u32,
    pub width: u16,
    pub height: u16,
    pub pixels: Buffer,
}

#[napi(object)]
pub struct MirrorStatsDto {
    #[napi(js_name = "framesReceived")]
    pub frames_received: f64,
    #[napi(js_name = "framesDropped")]
    pub frames_dropped: f64,
    #[napi(js_name = "lastFrameId")]
    pub last_frame_id: f64,
}

#[napi(object)]
pub struct NativeEvent {
    /// SIM-R04: session generation. Events from an older link can be ignored.
    pub generation: u32,
    /// "input" | "connected" | "disconnected" | "endpointSwitched" | "error" | "mirror"
    pub kind: String,
    #[napi(js_name = "deviceId")]
    pub device_id: Option<String>,
    pub input: Option<InputEventDto>,
    /// N474: a recognised gesture (tap / long-press / drag).
    pub gesture: Option<GestureDto>,
    pub message: Option<String>,
    pub mirror: Option<MirrorInfoDto>,
}

/// C08: host-side glTF → device package compilation result.
#[napi(object)]
pub struct CompiledPackageDto {
    pub bytes: Buffer,
    pub target: String,
    pub nodes: u32,
    pub meshes: u32,
    pub animations: Vec<String>,
    #[napi(js_name = "estimatedBytes")]
    pub estimated_bytes: f64,
}

/// N474: a recognised pointer gesture.
#[napi(object)]
pub struct GestureDto {
    /// tap | long-press | drag | pinch
    pub kind: String,
    #[napi(js_name = "pointerId")]
    pub pointer_id: u16,
    pub x: u16,
    pub y: u16,
    /// N493: the hit-tested scene node, when reported.
    pub node: Option<String>,
    /// N725: pinch distance ratio in permille (1000 for other gestures).
    pub scale: u16,
    /// N725: pinch angle change in degrees (0 for other gestures).
    pub rotation: i16,
    /// N735: pan midpoint translation in pixels (0 for other gestures).
    pub dx: i16,
    pub dy: i16,
}

struct SimSession {
    session: Session<Box<dyn Transport>>,
    device: Option<FakeDevice>,
    manager: Arc<Mutex<DeviceManager>>,
    /// SIM-R05: set by `dispose`; long handshakes must stop promptly.
    cancelled: Arc<AtomicBool>,
    /// SIM-R04: real-clock origin for the Session tick owner.
    created: Instant,
}

/// SIM-R04/R08: Node drives deadlines from a monotonic clock instead of a
/// caller-supplied tick. Reconnect is disabled so a dead peer reaches a
/// terminal error rather than waiting for a transport the SDK cannot supply.
fn node_session_config() -> SessionConfig {
    SessionConfig {
        reconnect: false,
        ..SessionConfig::default()
    }
}

fn tick_session(sim: &mut SimSession) -> Result<()> {
    let now_ms = sim.created.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    sim.session
        .tick(now_ms)
        .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(())
}

/// A peer that never answers HELLO is rejected after this budget instead of
/// looping for tens of seconds.
const HANDSHAKE_BUDGET: Duration = Duration::from_millis(2_000);

fn pump_link(sim: &mut SimSession) -> Result<()> {
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
struct State {
    disposed: bool,
    /// SIM-R05: once a real endpoint was requested, a failed HELLO must not
    /// silently fall back to an in-process FakeDevice.
    connection_attempted: bool,
    /// SIM-R04: incremented for every new Session; shared with NativeManager.
    generation: Arc<AtomicU32>,
    board_id: Option<String>,
    sim: Option<SimSession>,
    last_frame: Option<Vec<u8>>,
    /// N528: mirrored frames dropped because the caller's cap was too small.
    mirror_skips: u64,
    /// CORE-R06: the serial path this manager holds, and the matching process
    /// lease. Both are released when the session is replaced or disposed.
    serial_path: Option<String>,
    serial_lease: Option<PortLease>,
}

/// CORE-R06: reject JS numbers that cannot name a real image length.
fn image_len(value: f64) -> Result<u64> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return Err(Error::from_reason(
            "artifact.imageSize must be a whole byte count",
        ));
    }
    Ok(value as u64)
}

fn artifact_from_dto(dto: &FirmwareArtifactDto) -> Result<Artifact> {
    Ok(Artifact {
        board_id: dto.board_id.clone(),
        mcu: dto.mcu.clone(),
        runtime: dto.runtime.clone(),
        protocol_major: dto.protocol_major.min(u32::from(u8::MAX)) as u8,
        protocol_minor: dto.protocol_minor.min(u32::from(u8::MAX)) as u8,
        image_size: image_len(dto.image_size)?,
        sha256: dto.sha256.clone(),
        source: dto.source.clone(),
        public_key: dto
            .public_key
            .as_ref()
            .map(|bytes| bytes.to_vec())
            .unwrap_or_default(),
        signature: dto
            .signature
            .as_ref()
            .map(|bytes| bytes.to_vec())
            .unwrap_or_default(),
    })
}

fn board_facts_from_dto(dto: &FirmwareBoardFactsDto) -> Result<BoardFacts> {
    let flash = match dto.flash_bytes {
        Some(bytes) => FlashSize::Known(image_len(bytes)?),
        None => FlashSize::Unknown,
    };
    Ok(BoardFacts {
        board_id: dto.board_id.clone(),
        mcu: dto.mcu.clone(),
        flash,
        profile_verified: dto.profile_verified,
    })
}

fn preflight_verdict_for(request: &FirmwarePreflightRequest) -> Result<PreflightVerdict> {
    let artifact = artifact_from_dto(&request.artifact)?;
    let board = board_facts_from_dto(&request.board)?;
    let geometry = match request.image_geometry.as_ref() {
        Some(geometry) => Some(ImageGeometry {
            chip_family: geometry.chip_family.clone(),
            flash_bytes: image_len(geometry.flash_bytes)?,
        }),
        None => None,
    };
    Ok(preflight_verdict_bytes(PreflightInputBytes {
        bytes: &request.image,
        artifact: &artifact,
        board: &board,
        host: mdc_protocol::Version {
            major: request.host_protocol_major.min(u32::from(u8::MAX)) as u8,
            minor: request.host_protocol_minor.min(u32::from(u8::MAX)) as u8,
        },
        detected_chip_family: request.detected_chip_family.as_deref(),
        image_geometry: geometry.as_ref(),
        allow_unknown_flash: request.allow_unknown_flash.unwrap_or(false),
        flash_id_unusable: request.flash_id_unusable.unwrap_or(false),
    }))
}

fn preflight_result(verdict: PreflightVerdict) -> FirmwarePreflightResult {
    match verdict {
        PreflightVerdict::Ok => FirmwarePreflightResult {
            ok: true,
            verdict: "ok".into(),
            reason: None,
        },
        PreflightVerdict::OkUnknownFlash => FirmwarePreflightResult {
            ok: true,
            verdict: "okUnknownFlash".into(),
            reason: None,
        },
        PreflightVerdict::Reject(error) => FirmwarePreflightResult {
            ok: false,
            verdict: "reject".into(),
            reason: Some(format!("{error:?}")),
        },
    }
}

fn install_phase(state: InstallState) -> &'static str {
    match state {
        InstallState::Idle => "idle",
        InstallState::AcquiringPort => "lease",
        InstallState::Flashing => "writing",
        InstallState::Resetting => "rebooting",
        InstallState::Verifying => "readback",
        InstallState::Succeeded => "done",
        InstallState::Cancelled => "cancelled",
        InstallState::Failed => "failed",
    }
}

fn install_ratio(state: InstallState) -> Option<f64> {
    match state {
        InstallState::AcquiringPort => Some(0.1),
        InstallState::Flashing => Some(0.3),
        InstallState::Resetting => Some(0.7),
        InstallState::Verifying => Some(0.9),
        InstallState::Succeeded => Some(1.0),
        _ => None,
    }
}

fn install_error_reason(error: &InstallError) -> String {
    match error {
        InstallError::PortBusy => "PortBusy".into(),
        InstallError::Cancelled => "Cancelled".into(),
        InstallError::Flasher(message) => format!("Flasher: {message}"),
        InstallError::IdentityMismatch => "IdentityMismatch".into(),
    }
}

fn set_progress(
    progress: &Mutex<FirmwareProgressDto>,
    phase: &str,
    ratio: Option<f64>,
    error: Option<String>,
) {
    if let Ok(mut slot) = progress.lock() {
        *slot = FirmwareProgressDto {
            phase: phase.into(),
            ratio,
            error,
        };
    }
}

/// Simulator flasher with a cancellable delay so the public SDK can show
/// progress and settle `cancelFirmware()` before the write completes.
struct DelayedSimFlasher {
    inner: SimFlasher,
    delay: Duration,
    cancel: Arc<AtomicBool>,
}

impl DelayedSimFlasher {
    fn new(device_id: String, delay: Duration, cancel: Arc<AtomicBool>) -> Self {
        Self {
            inner: SimFlasher::new(device_id),
            delay,
            cancel,
        }
    }

    fn wait(&self) -> std::result::Result<(), String> {
        let mut remaining = self.delay;
        while !remaining.is_zero() {
            if self.cancel.load(AtomicOrdering::Acquire) {
                return Err("cancelled".into());
            }
            let slice = remaining.min(Duration::from_millis(20));
            thread::sleep(slice);
            remaining = remaining.saturating_sub(slice);
        }
        Ok(())
    }
}

impl Flasher for DelayedSimFlasher {
    fn flash(&mut self, plan: &InstallPlan) -> std::result::Result<(), String> {
        self.wait()?;
        self.inner.flash(plan)
    }
    fn reset(&mut self) -> std::result::Result<(), String> {
        self.inner.reset()
    }
    fn read_device_id(&mut self) -> std::result::Result<String, String> {
        self.inner.read_device_id()
    }
}

/// esptool needs a file; the SDK receives bytes. Write a bounded temp image and
/// remove it even when the write fails.
fn write_temp_image(bytes: &[u8]) -> std::result::Result<PathBuf, String> {
    let serial = FIRMWARE_TEMP_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("mdc-firmware-{}-{serial}.bin", std::process::id()));
    std::fs::write(&path, bytes).map_err(|error| format!("temp image: {error}"))?;
    Ok(path)
}

fn ensure_sim(state: &mut State) -> Result<()> {
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
fn connect_transport(
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

fn drain_manager_events(
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

#[napi]
pub struct NativeManager {
    state: Arc<Mutex<State>>,
    /// SIM-R05: shared with the worker task so `dispose` can cancel I/O without
    /// first taking the state lock.
    cancelled: Arc<AtomicBool>,
    disposed: Arc<AtomicBool>,
    generation: Arc<AtomicU32>,
    /// CORE-R06: firmware install cancellation and the last progress snapshot.
    firmware_cancel: Arc<AtomicBool>,
    firmware_progress: Arc<Mutex<FirmwareProgressDto>>,
}

impl Default for NativeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeManager {
    /// Connect (or reuse) FakeDevice simulator and return device DTOs.
    fn devices_sync(&self) -> Result<Vec<DeviceInfo>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_ref().unwrap();
        if sim.session.device.is_none() {
            return Err(Error::from_reason("no active connection"));
        }
        let mgr = sim
            .manager
            .lock()
            .map_err(|_| Error::from_reason("manager lock poisoned"))?;
        let mut out = Vec::new();
        for d in mgr.devices() {
            out.push(DeviceInfo {
                device_id: d.capabilities.device_id.clone(),
                firmware: d.capabilities.firmware.clone(),
                surfaces: d
                    .capabilities
                    .surfaces
                    .iter()
                    .map(|s| SurfaceInfo {
                        id: s.id.clone(),
                        width: s.width,
                        height: s.height,
                        pixel_format: s.pixel_format.to_string(),
                        implemented: s.pixel_format.is_implemented(),
                        stride: s.stride,
                        rotation: s.rotation,
                    })
                    .collect(),
            });
        }
        Ok(out)
    }

    /// Send RGB565 frame bytes to `surfaceId` via the sim Session.
    fn send_frame_sync(&self, surface_id: String, bytes: Buffer) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let copy = bytes.to_vec();
        state.last_frame = Some(copy.clone());
        let sim = state.sim.as_mut().unwrap();
        let (w, h) = sim
            .session
            .device
            .as_ref()
            .and_then(|d| d.capabilities.surfaces.iter().find(|s| s.id == surface_id))
            .map(|s| (s.width, s.height))
            .unwrap_or_else(|| {
                sim.device
                    .as_ref()
                    .map(|device| (device.profile().width, device.profile().height))
                    .unwrap_or((0, 0))
            });
        let expected = usize::from(w) * usize::from(h) * 2;
        if copy.len() != expected {
            return Err(Error::from_reason(format!(
                "frame length {} != expected {}",
                copy.len(),
                expected
            )));
        }
        let req = sim
            .session
            .send_frame(Frame {
                surface_id,
                width: w,
                height: h,
                bytes: copy,
            })
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(req)
    }

    /// Send an RGB565 tile (blocked after reconnect until a full frame ACK).
    fn send_tile_sync(&self, tile: TileRequest) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let copy = tile.bytes.to_vec();
        let expected = usize::from(tile.width) * usize::from(tile.height) * 2;
        if copy.len() != expected {
            return Err(Error::from_reason(format!(
                "tile length {} != expected {}",
                copy.len(),
                expected
            )));
        }
        let sim = state.sim.as_mut().unwrap();
        let req = sim
            .session
            .send_tile(Tile {
                surface_id: tile.surface_id,
                base_frame_id: u64::from(tile.base_frame_id),
                x: tile.x,
                y: tile.y,
                width: tile.width,
                height: tile.height,
                bytes: copy,
            })
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(req)
    }

    /// Peer-reported current frame id (for tile baseFrameId).
    fn current_frame_id_sync(&self) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        Ok(state
            .sim
            .as_ref()
            .unwrap()
            .device
            .as_ref()
            .map(|device| device.current_frame_id() as u32)
            .unwrap_or(0))
    }

    /// Drain ManagerEvent queue into JS-friendly DTOs.
    fn poll_events_sync(&self) -> Result<Vec<NativeEvent>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let generation = state.generation.load(AtomicOrdering::Acquire);
        let sim = state.sim.as_mut().unwrap();
        let pumped = pump_link(sim);
        let events = drain_manager_events(&sim.manager, generation)?;
        if events.is_empty() {
            pumped?;
        }
        Ok(events)
    }

    /// Sim-only: emit a touch event from FakeDevice toward the host Session.
    fn simulate_input_sync(
        &self,
        surface_id: String,
        pointer_id: u16,
        phase: String,
        x: u16,
        y: u16,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let device = sim
            .device
            .as_mut()
            .ok_or_else(|| Error::from_reason("tcp session cannot inject local touch"))?;
        device
            .emit_input(InputEvent {
                surface_id,
                pointer_id,
                phase,
                x,
                y,

                node: None,
            })
            .map_err(|e| Error::from_reason(e.to_string()))?;
        sim.session
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(())
    }

    /// N400: send a pointer event from the host to the device (capability-gated).
    fn send_input_sync(
        &self,
        surface_id: String,
        pointer_id: u16,
        phase: String,
        x: u16,
        y: u16,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .send_input(InputEvent {
                surface_id,
                pointer_id,
                phase,
                x,
                y,

                node: None,
            })
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(())
    }

    /// Form B: ask the device to start/stop mirroring (capability-gated).
    fn start_mirror_sync(&self, surface_id: String, mode: String, max_fps: u16) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let request = sim
            .session
            .start_mirror(&surface_id, &mode, max_fps)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(request)
    }

    fn stop_mirror_sync(&self, surface_id: String) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let request = sim
            .session
            .stop_mirror(&surface_id)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(request)
    }

    /// Sim-only: push one mirrored frame from FakeDevice toward the host.
    fn emit_mirror_frame_sync(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let device = sim
            .device
            .as_mut()
            .ok_or_else(|| Error::from_reason("tcp session cannot emit a local mirror frame"))?;
        device
            .emit_mirror_frame()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        sim.session
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(())
    }

    /// Drain the latest complete mirrored frame (pixels are copied out).
    fn take_mirror_frame_sync(&self) -> Result<Option<MirrorFrameDto>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        Ok(sim.session.take_mirror_frame().map(|frame| MirrorFrameDto {
            surface_id: frame.surface_id,
            frame_id: frame.frame_id as u32,
            width: frame.width,
            height: frame.height,
            pixels: Buffer::from(frame.bytes),
        }))
    }

    /// N528: take the newest mirrored frame only when it fits `max_bytes`;
    /// otherwise the frame is dropped and counted (see `mirrorSkips`).
    fn take_mirror_frame_bounded_sync(&self, max_bytes: u32) -> Result<Option<MirrorFrameDto>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let Some(frame) = sim.session.take_mirror_frame() else {
            return Ok(None);
        };
        if frame.bytes.len() > max_bytes as usize {
            state.mirror_skips = state.mirror_skips.saturating_add(1);
            return Ok(None);
        }
        Ok(Some(MirrorFrameDto {
            surface_id: frame.surface_id,
            frame_id: frame.frame_id as u32,
            width: frame.width,
            height: frame.height,
            pixels: Buffer::from(frame.bytes),
        }))
    }

    /// N528: mirrored frames dropped because the caller's buffer was too small.
    fn mirror_skips_sync(&self) -> Result<f64> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        Ok(state.mirror_skips as f64)
    }

    /// N559: the negotiated mirror mode (`full-frame` | `dirty-region`, empty
    /// before the device confirms). Parity with the Swift/C SDKs.
    fn mirror_mode_sync(&self) -> Result<String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        Ok(sim.session.mirror_mode().to_string())
    }

    fn mirror_stats_sync(&self) -> Result<MirrorStatsDto> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let stats = sim.session.mirror_stats();
        Ok(MirrorStatsDto {
            frames_received: stats.frames_received as f64,
            frames_dropped: stats.frames_dropped as f64,
            last_frame_id: stats.last_frame_id as f64,
        })
    }

    /// N42: read a device configuration value (sim path).
    fn get_config_sync(&self, key: String) -> Result<String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .get_config(&key)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(sim.session.config(&key).unwrap_or("").to_string())
    }

    /// N784: the current multi-tap radius in pixels.
    fn multi_tap_radius_sync(&self) -> Result<u16> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        let Some(sim) = state.sim.as_ref() else {
            return Ok(mdc_core::GestureConfig::default().multi_tap_radius);
        };
        Ok(sim.session.gesture_config().multi_tap_radius)
    }

    /// N784: set the multi-tap radius. Values above 4096 are refused.
    fn set_multi_tap_radius_sync(&self, radius: u32) -> Result<u16> {
        if radius > 4096 {
            return Err(Error::from_reason("multi_tap_radius out of range"));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let mut config = sim.session.gesture_config();
        config.multi_tap_radius = radius as u16;
        sim.session.set_gesture_config(config);
        Ok(config.multi_tap_radius)
    }

    /// N794: the current multi-tap window in milliseconds.
    fn multi_tap_ms_sync(&self) -> Result<u32> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        let ms = match state.sim.as_ref() {
            Some(sim) => sim.session.gesture_config().multi_tap_ms,
            None => mdc_core::GestureConfig::default().multi_tap_ms,
        };
        Ok(u32::try_from(ms).unwrap_or(u32::MAX))
    }

    /// N794: set the multi-tap window. Values above 60000 are refused.
    fn set_multi_tap_ms_sync(&self, ms: u32) -> Result<u32> {
        if u64::from(ms) > mdc_core::MULTI_TAP_MS_MAX {
            return Err(Error::from_reason("multi_tap_ms out of range"));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let mut config = sim.session.gesture_config();
        config.multi_tap_ms = u64::from(ms);
        sim.session.set_gesture_config(config);
        Ok(ms)
    }

    /// N42: write a device configuration value and return the accepted value.
    fn set_config_sync(&self, key: String, value: String) -> Result<String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .set_config(&key, &value)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(sim.session.config(&key).unwrap_or("").to_string())
    }

    /// N55: set a vector scene from a JSON SceneNode (sim path).
    fn set_scene_sync(&self, json: String) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let node: mdc_protocol::SceneNode = serde_json::from_str(&json)
            .map_err(|e| Error::from_reason(format!("scene json: {e}")))?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .set_scene(node)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(())
    }

    fn clear_scene_sync(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .clear_scene()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(())
    }

    /// N74: play a baked-atlas clip on the device (via the animation setting).
    fn set_animation_sync(&self, clip: String) -> Result<String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .set_animation(&clip)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(sim.session.active_animation().unwrap_or("off").to_string())
    }

    fn stop_animation_sync(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .stop_animation()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(())
    }

    /// N837: blend up to four clips (`"idle:700,wave:300"`).
    fn set_animation_blend_sync(&self, blend: String) -> Result<String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .set_animation_blend(&blend)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(sim
            .session
            .active_animation_blend()
            .unwrap_or("off")
            .to_string())
    }

    /// N837: the blend the device last accepted (`"off"` when inactive).
    fn animation_blend_sync(&self) -> Result<String> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        let sim = state
            .sim
            .as_ref()
            .ok_or_else(|| Error::from_reason("no simulator session"))?;
        Ok(sim
            .session
            .active_animation_blend()
            .unwrap_or("off")
            .to_string())
    }

    /// N233: upload a runtime package (`.mdcpkg` bytes) and report readiness.
    fn install_package_sync(&self, bytes: Buffer) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        sim.session
            .install_package(bytes.as_ref())
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(sim
            .device
            .as_ref()
            .map(|device| device.package_ready())
            .unwrap_or(false))
    }

    /// N238: true once the last accepted package is render-ready.
    fn package_ready_sync(&self) -> Result<bool> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        Ok(state
            .sim
            .as_ref()
            .and_then(|sim| sim.device.as_ref())
            .map(|device| device.package_ready())
            .unwrap_or(false))
    }

    /// Open a named simulator board before the first `devices()` call.
    /// CORE-R02: close the active session (serial/TCP/WS or simulator) without
    /// disposing the manager. Releasing the serial lease lets a firmware install
    /// take the port; the next call reports `no active connection` instead of
    /// creating an implicit FakeDevice. Idempotent.
    fn disconnect_sync(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        if let Some(mut sim) = state.sim.take() {
            sim.session.disconnect();
        }
        state.serial_lease = None;
        state.serial_path = None;
        state.last_frame = None;
        state.generation.fetch_add(1, AtomicOrdering::AcqRel);
        Ok(())
    }

    fn open_board_sync(&self, board_id: String) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        if state.sim.is_some() {
            return Err(Error::from_reason("simulator is already open"));
        }
        // An explicit simulator request is the one path allowed to create a
        // FakeDevice after a real endpoint or a disconnect.
        state.connection_attempted = false;
        state.board_id = Some(board_id);
        ensure_sim(&mut state)
    }

    /// Sim-only: drop the link and emit cancel for pointers that are still down.
    fn simulate_disconnect_sync(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let device = sim
            .device
            .as_mut()
            .ok_or_else(|| Error::from_reason("tcp session cannot inject a local disconnect"))?;
        device.faults.request_disconnect();
        let device_result = device.poll();
        sim.session
            .poll()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        match device_result {
            Ok(()) | Err(SimError::Disconnected) => Ok(()),
            Err(error) => Err(Error::from_reason(error.to_string())),
        }
    }

    /// Replace the session with a TCP connection to a simulator peer (`host:port`).
    fn connect_tcp_sync(&self, address: String) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        state.connection_attempted = true;
        let tcp = TcpTransport::connect(address.trim(), Duration::from_millis(400))
            .map_err(|e| Error::from_reason(e.to_string()))?;
        connect_transport(
            &mut state,
            Box::new(tcp),
            address.trim(),
            "tcp",
            &self.cancelled,
            None,
        )
    }

    /// N455: the host-side session counters.
    fn stats_sync(&self) -> Result<SessionStatsDto> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_ref().unwrap();
        let stats = sim.session.stats();
        Ok(SessionStatsDto {
            packets_sent: stats.packets_sent as i64,
            bytes_sent: stats.bytes_sent as i64,
            bytes_received: stats.bytes_received as i64,
            acks_received: stats.acks_received as i64,
            frames_sent: stats.frames_sent as i64,
            dropped_frames: stats.dropped_frames as i64,
            packages_sent: stats.packages_sent as i64,
            mirror_frames: stats.mirror_frames as i64,
            pacing_dropped: stats.pacing_dropped as i64,
        })
    }

    /// N576: the host's partial-update reconciliation (parity with C `mdc_session_tile_reconciliation`).
    fn tile_reconciliation_sync(&self) -> Result<TileReconciliationDto> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_ref().unwrap();
        let report = sim.session.tile_reconciliation();
        Ok(TileReconciliationDto {
            sent: report.sent as i64,
            received: report.received as i64,
            missing: report.missing() as i64,
            bytes_sent: report.bytes_sent as i64,
            consistent: report.consistent,
        })
    }

    /// APP-R03: enumerate host serial ports. A port is only an endpoint hint;
    /// identity is confirmed by the subsequent HELLO.
    fn serial_ports_sync(&self) -> Result<Vec<SerialPortDto>> {
        let ports = mdc_transport::available_serial_ports()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(ports
            .into_iter()
            .map(|port| SerialPortDto {
                path: port.path,
                manufacturer: port.manufacturer,
                product: port.product,
                serial_number: port.serial_number,
            })
            .collect())
    }

    /// APP-R03: the negotiated device capabilities, including mirror/config/scene.
    fn capabilities_sync(&self) -> Result<CapabilitiesInfo> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        let sim = state
            .sim
            .as_ref()
            .ok_or_else(|| Error::from_reason("no active session"))?;
        let device = sim
            .session
            .device
            .as_ref()
            .ok_or_else(|| Error::from_reason("device is not ready"))?;
        let caps = &device.capabilities;
        let surfaces = caps
            .surfaces
            .iter()
            .map(|surface| SurfaceInfo {
                id: surface.id.clone(),
                width: surface.width,
                height: surface.height,
                pixel_format: surface.pixel_format.to_string(),
                implemented: surface.pixel_format.is_implemented(),
                stride: surface.stride,
                rotation: surface.rotation,
            })
            .collect();
        let mirror_modes = caps.mirror.mode.clone();
        Ok(CapabilitiesInfo {
            device_id: caps.device_id.clone(),
            board_id: caps.board_id.clone(),
            protocol_version: caps.protocol_version,
            firmware: caps.firmware.clone(),
            surfaces,
            frame: caps.frame,
            tile: caps.tile,
            touch: caps.touch,
            ota: caps.ota,
            max_message: caps.max_message,
            max_chunk: caps.max_chunk,
            max_in_flight: u16::from(caps.max_in_flight),
            max_fps: caps.max_fps,
            mirror: !mirror_modes.is_empty() && caps.mirror_fps > 0,
            mirror_modes,
            mirror_max_fps: caps.mirror.max_fps,
            local_display_fps: caps.local_display_fps,
            mirror_fps: caps.mirror_fps,
            mirror_bandwidth: caps.mirror_bandwidth as f64,
            config_keys: caps.config.keys.clone(),
            scene: !caps.scene.is_disabled(),
            scene_max_nodes: caps.scene.max_nodes,
            scene_max_depth: caps.scene.max_depth,
            boot_id: caps.boot_id as f64,
            run_id: caps.run_id as f64,
        })
    }

    /// N430/CORE-R06: replace the session with a serial connection (`/dev/...`).
    /// The port is leased for the session lifetime so a firmware install cannot
    /// take it, and a firmware install's lease blocks this open.
    fn connect_serial_sync(
        &self,
        path: String,
        baud: u32,
        reset_on_open: Option<bool>,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        state.connection_attempted = true;
        let path = path.trim().to_string();
        let same_path =
            state.serial_path.as_deref() == Some(path.as_str()) && state.serial_lease.is_some();
        // A different path must be leased before opening so an in-flight install
        // cannot be raced. A reconnect on the same path already holds its lease;
        // keep it in place until the new handle is open so a failed open leaves
        // the old session and lease untouched.
        let new_lease =
            if same_path {
                None
            } else {
                Some(FIRMWARE_LEASES.acquire(&path).map_err(|_| {
                    Error::from_reason("serial port is leased by a firmware install")
                })?)
            };
        let serial = mdc_transport::SerialTransport::open_with_config(
            &path,
            mdc_transport::SerialConfig {
                baud: if baud == 0 { 115_200 } else { baud },
                timeout: Duration::from_millis(400),
                reset_on_open: reset_on_open.unwrap_or(false),
            },
        )
        .map_err(|e| Error::from_reason(e.to_string()))?;
        let lease = match new_lease {
            Some(lease) => lease,
            None => state
                .serial_lease
                .take()
                .ok_or_else(|| Error::from_reason("serial lease missing"))?,
        };
        connect_transport(
            &mut state,
            Box::new(serial),
            &path,
            "serial",
            &self.cancelled,
            Some((path.clone(), lease)),
        )
    }

    /// N340: replace the session with a WebSocket connection (`ws://host:port`).
    fn connect_web_socket_sync(&self, url: String) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        state.connection_attempted = true;
        let ws = mdc_transport::WebSocketTransport::connect(url.trim(), Duration::from_millis(400))
            .map_err(|e| Error::from_reason(e.to_string()))?;
        connect_transport(
            &mut state,
            Box::new(ws),
            url.trim(),
            "websocket",
            &self.cancelled,
            None,
        )
    }

    fn last_frame_sync(&self) -> Result<Option<Buffer>> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        Ok(state.last_frame.clone().map(Buffer::from))
    }

    /// N349: emit a metric on the simulator device's diagnostics channel.
    fn emit_metric_sync(&self, name: String, value: f64, unit: Option<String>) -> Result<u32> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        ensure_sim(&mut state)?;
        let sim = state.sim.as_mut().unwrap();
        let device = sim
            .device
            .as_mut()
            .ok_or_else(|| Error::from_reason("tcp/ws session cannot emit a local metric"))?;
        device
            .emit_metric(&name, value, unit.as_deref())
            .map_err(|e| Error::from_reason(e.to_string()))?;
        pump_link(sim)?;
        Ok(sim.session.diagnostic_count() as u32)
    }

    /// C08: compile an embedded glTF JSON document into a device package. This
    /// is a pure host operation; the caller can upload the bytes with
    /// `installPackage` and then drive Form B.
    fn compile_gltf_sync(&self, bytes: Buffer, target: String) -> Result<CompiledPackageDto> {
        let capability = match target.as_str() {
            "sprite" => mdc_model::RendererCapability::sprite_only(),
            "mesh3d" => mdc_model::RendererCapability::full_3d(),
            _ => return Err(Error::from_reason("target must be 'sprite' or 'mesh3d'")),
        };
        let model = mdc_model::GltfImporter
            .import(bytes.as_ref(), &mdc_model::ImportLimits::default())
            .map_err(|error| Error::from_reason(format!("glTF import: {error}")))?;
        let package = mdc_model::compile_to_package(&model, &capability)
            .map_err(|error| Error::from_reason(format!("package compile: {error}")))?;
        let animations = package
            .package
            .animations
            .iter()
            .map(|animation| animation.name.clone())
            .collect();
        let nodes = package.package.nodes.len() as u32;
        let meshes = package.package.meshes.len() as u32;
        let estimated_bytes = package.package.estimated_bytes as f64;
        let bytes = package
            .to_bytes()
            .map_err(|error| Error::from_reason(format!("package encode: {error}")))?;
        Ok(CompiledPackageDto {
            bytes: Buffer::from(bytes),
            target,
            nodes,
            meshes,
            animations,
            estimated_bytes,
        })
    }

    /// N349: drain the bounded diagnostics channel (metrics, boot info, telemetry).
    fn take_diagnostics_sync(&self) -> Result<Vec<DiagnosticDto>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        let Some(sim) = state.sim.as_mut() else {
            return Ok(Vec::new());
        };
        let records = sim.session.take_diagnostics();
        Ok(records
            .into_iter()
            .map(|record| match record {
                mdc_core::DiagnosticRecord::Metric(metric) => DiagnosticDto {
                    kind: "metric".into(),
                    name: metric.name,
                    value: metric.value,
                    unit: metric.unit,
                    fields: None,
                },
                mdc_core::DiagnosticRecord::BootInfo(boot) => DiagnosticDto {
                    kind: "bootInfo".into(),
                    name: boot.firmware,
                    value: boot.boot_id as f64,
                    unit: boot.reset_reason,
                    fields: None,
                },
                mdc_core::DiagnosticRecord::Telemetry(event) => DiagnosticDto {
                    kind: "telemetry".into(),
                    name: event.name,
                    value: event.event_seq as f64,
                    unit: event.level,
                    fields: serde_json::to_string(&event.fields).ok(),
                },
                mdc_core::DiagnosticRecord::Crash(report) => DiagnosticDto {
                    kind: "crash".into(),
                    name: report.reason,
                    value: f64::from(report.pc.unwrap_or(0)),
                    unit: report.task,
                    fields: None,
                },
            })
            .collect())
    }

    /// N349: number of diagnostics waiting.
    fn diagnostic_count_sync(&self) -> Result<u32> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::from_reason("native state lock poisoned"))?;
        if state.disposed {
            return Err(Error::from_reason("manager is disposed"));
        }
        Ok(state
            .sim
            .as_ref()
            .map(|sim| sim.session.diagnostic_count() as u32)
            .unwrap_or(0))
    }

    /// CORE-R06: board facts for firmware preflight, without parsing YAML.
    fn board_facts_sync(&self, board_id: String) -> Result<BoardFactsDto> {
        if self.disposed.load(AtomicOrdering::Acquire) {
            return Err(Error::from_reason("manager is disposed"));
        }
        let board_id = board_id.trim();
        if board_id.is_empty() {
            return Err(Error::from_reason("board id must be non-empty"));
        }
        Ok(match known_board_facts(board_id) {
            Some(facts) => BoardFactsDto {
                board_id: facts.board_id,
                mcu: facts.mcu,
                flash_bytes: match facts.flash {
                    FlashSize::Known(bytes) => Some(bytes as f64),
                    FlashSize::Unknown => None,
                },
                profile_verified: facts.profile_verified,
                known: true,
            },
            None => BoardFactsDto {
                board_id: board_id.to_string(),
                mcu: String::new(),
                flash_bytes: None,
                profile_verified: false,
                known: false,
            },
        })
    }

    /// CORE-R06: verify a firmware image against the board facts without
    /// touching a serial port. Returns a structured verdict, not a throw.
    fn firmware_preflight_sync(
        &self,
        request: FirmwarePreflightRequest,
    ) -> Result<FirmwarePreflightResult> {
        if self.disposed.load(AtomicOrdering::Acquire) {
            return Err(Error::from_reason("manager is disposed"));
        }
        Ok(preflight_result(preflight_verdict_for(&request)?))
    }

    /// CORE-R06: preflight + exclusive lease + write + reset + identity readback.
    /// The `sim` mode never opens the port; `esptool` writes a bounded temp image
    /// and defaults to a dry run, so real flashing stays opt-in and unverified.
    fn firmware_install_sync(
        &self,
        request: FirmwareInstallRequest,
        cancel: Arc<AtomicBool>,
        progress: Arc<Mutex<FirmwareProgressDto>>,
    ) -> Result<FirmwareInstallResult> {
        if self.disposed.load(AtomicOrdering::Acquire) {
            return Err(Error::from_reason("manager is disposed"));
        }
        cancel.store(false, AtomicOrdering::Release);
        set_progress(&progress, "preflight", None, None);
        let verdict = preflight_verdict_for(&request.preflight)?;
        let preflight = preflight_result(verdict);
        if !preflight.ok {
            let reason = preflight.reason.unwrap_or_else(|| "Preflight".into());
            set_progress(&progress, "failed", None, Some(reason.clone()));
            return Ok(FirmwareInstallResult {
                ok: false,
                phase: "preflight".into(),
                device_id: None,
                error: Some(reason),
            });
        }
        let artifact = artifact_from_dto(&request.preflight.artifact)?;
        let port = request.port.trim().to_string();
        let expected_device_id = request.expected_device_id.trim().to_string();
        if port.is_empty() || expected_device_id.is_empty() {
            let reason = "InvalidRequest".to_string();
            set_progress(&progress, "failed", None, Some(reason.clone()));
            return Ok(FirmwareInstallResult {
                ok: false,
                phase: "preflight".into(),
                device_id: None,
                error: Some(reason),
            });
        }
        let plan = InstallPlan {
            port: port.clone(),
            artifact,
            expected_device_id: expected_device_id.clone(),
        };
        let mut installer = Installer::default();
        let progress_for_cb = progress.clone();
        let mut on_state = move |state: InstallState| {
            set_progress(
                &progress_for_cb,
                install_phase(state),
                install_ratio(state),
                None,
            );
        };
        let mut temp_image: Option<PathBuf> = None;
        let outcome = match request.mode.as_str() {
            "sim" => {
                if !port.starts_with("sim://") {
                    // Never pretend a real port was flashed by the fake writer.
                    let reason = "SimModeRequiresSimPort".to_string();
                    set_progress(&progress, "failed", None, Some(reason.clone()));
                    return Ok(FirmwareInstallResult {
                        ok: false,
                        phase: "preflight".into(),
                        device_id: None,
                        error: Some(reason),
                    });
                }
                let delay =
                    Duration::from_millis(u64::from(request.sim_delay_ms.unwrap_or(0).min(60_000)));
                let mut flasher = DelayedSimFlasher::new(
                    request
                        .sim_readback_device_id
                        .clone()
                        .unwrap_or_else(|| expected_device_id.clone()),
                    delay,
                    cancel.clone(),
                );
                installer.run_with(
                    &FIRMWARE_LEASES,
                    &plan,
                    &mut flasher,
                    &cancel,
                    &mut on_state,
                )
            }
            "esptool" => {
                let image_path = match write_temp_image(&request.preflight.image) {
                    Ok(path) => path,
                    Err(error) => {
                        let reason = format!("Flasher: {error}");
                        set_progress(&progress, "failed", None, Some(reason.clone()));
                        return Ok(FirmwareInstallResult {
                            ok: false,
                            phase: "failed".into(),
                            device_id: None,
                            error: Some(reason),
                        });
                    }
                };
                temp_image = Some(image_path.clone());
                let mut flasher = EspToolFlasher::new(&image_path);
                if let Some(flags) = flags_for_board(&plan.artifact.board_id) {
                    flasher.flags = flags;
                }
                if request.dry_run == Some(false) {
                    flasher = flasher.with_live_esptool(
                        request
                            .esptool_path
                            .clone()
                            .unwrap_or_else(|| "esptool.py".into()),
                    );
                }
                installer.run_with(
                    &FIRMWARE_LEASES,
                    &plan,
                    &mut flasher,
                    &cancel,
                    &mut on_state,
                )
            }
            other => {
                let reason = format!("UnknownFirmwareMode: {other}");
                set_progress(&progress, "failed", None, Some(reason.clone()));
                return Ok(FirmwareInstallResult {
                    ok: false,
                    phase: "preflight".into(),
                    device_id: None,
                    error: Some(reason),
                });
            }
        };
        if let Some(path) = temp_image {
            let _ = std::fs::remove_file(path);
        }
        match outcome {
            Ok(()) => {
                set_progress(&progress, "done", Some(1.0), None);
                Ok(FirmwareInstallResult {
                    ok: true,
                    phase: "done".into(),
                    device_id: Some(expected_device_id),
                    error: None,
                })
            }
            Err(InstallError::Cancelled) => {
                set_progress(&progress, "cancelled", None, None);
                Ok(FirmwareInstallResult {
                    ok: false,
                    phase: "cancelled".into(),
                    device_id: None,
                    error: Some("Cancelled".into()),
                })
            }
            Err(error) => {
                let reason = install_error_reason(&error);
                set_progress(&progress, "failed", None, Some(reason.clone()));
                Ok(FirmwareInstallResult {
                    ok: false,
                    phase: "failed".into(),
                    device_id: None,
                    error: Some(reason),
                })
            }
        }
    }

    /// CORE-R06: open the port under the same lease, perform HELLO, and report
    /// the device identity/version the peer actually advertises.
    fn firmware_readback_sync(
        &self,
        request: FirmwareReadbackRequest,
    ) -> Result<FirmwareReadbackResult> {
        if self.disposed.load(AtomicOrdering::Acquire) {
            return Err(Error::from_reason("manager is disposed"));
        }
        let port = request.port.trim().to_string();
        if port.is_empty() {
            return Err(Error::from_reason("readback port must be non-empty"));
        }
        let timeout_ms = u64::from(request.timeout_ms.unwrap_or(3_000).clamp(200, 15_000));
        let lease = FIRMWARE_LEASES.acquire(&port).map_err(|_| {
            Error::from_reason("serial port is leased by a firmware install or an open session")
        })?;
        let baud = request.baud.unwrap_or(115_200).max(1);
        let serial = mdc_transport::SerialTransport::open_with_config(
            &port,
            mdc_transport::SerialConfig {
                baud,
                timeout: Duration::from_millis(400),
                reset_on_open: false,
            },
        )
        .map_err(|e| Error::from_reason(e.to_string()))?;
        let mut session = Session::with_config(
            Box::new(serial) as Box<dyn Transport>,
            SessionConfig {
                connect_timeout_ms: timeout_ms,
                ack_timeout_ms: 5_000,
                heartbeat_ms: 5_000,
                reconnect: false,
            },
        );
        session
            .connect()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if let Err(error) = session.poll() {
                session.disconnect();
                return Err(Error::from_reason(error.to_string()));
            }
            if session.state == ConnectionState::Ready {
                break;
            }
            if Instant::now() >= deadline {
                session.disconnect();
                return Err(Error::from_reason(
                    "firmware readback timed out waiting for HELLO",
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
        let capabilities = session
            .device
            .as_ref()
            .map(|device| device.capabilities.clone())
            .ok_or_else(|| Error::from_reason("firmware readback has no capabilities"))?;
        session.disconnect();
        drop(lease);
        Ok(FirmwareReadbackResult {
            device_id: capabilities.device_id,
            board_id: capabilities.board_id,
            protocol_version: capabilities.protocol_version,
            firmware: capabilities.firmware,
        })
    }
}

/// A unit-returning native call resolves to JS `undefined`.
fn unit(result: Result<()>) -> Result<Undefined> {
    result?;
    Ok(())
}

type Work<R> = Box<dyn FnOnce(&NativeManager) -> Result<R> + Send>;

/// SIM-R05: run one blocking native operation on the libuv worker pool. The JS
/// thread only creates the task and receives a Promise, so timers keep firing
/// while a silent peer, serial open or handshake is waiting.
pub struct BlockingTask<R>
where
    R: Send + 'static + ToNapiValue + TypeName,
{
    manager: NativeManager,
    work: Option<Work<R>>,
}

impl<R> Task for BlockingTask<R>
where
    R: Send + 'static + ToNapiValue + TypeName,
{
    type Output = R;
    type JsValue = R;

    fn compute(&mut self) -> Result<Self::Output> {
        if self.manager.disposed.load(AtomicOrdering::Acquire) {
            return Err(Error::from_reason("manager is disposed"));
        }
        let work = self
            .work
            .take()
            .ok_or_else(|| Error::from_reason("native task ran twice"))?;
        work(&self.manager)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

impl NativeManager {
    fn spawn<R>(
        &self,
        work: impl FnOnce(&NativeManager) -> Result<R> + Send + 'static,
    ) -> AsyncTask<BlockingTask<R>>
    where
        R: Send + 'static + ToNapiValue + TypeName,
    {
        AsyncTask::new(BlockingTask {
            manager: NativeManager {
                state: self.state.clone(),
                cancelled: self.cancelled.clone(),
                disposed: self.disposed.clone(),
                generation: self.generation.clone(),
                firmware_cancel: self.firmware_cancel.clone(),
                firmware_progress: self.firmware_progress.clone(),
            },
            work: Some(Box::new(work)),
        })
    }
}

#[napi]
impl NativeManager {
    #[napi(constructor)]
    pub fn new() -> Self {
        let generation = Arc::new(AtomicU32::new(0));
        let state = State {
            generation: generation.clone(),
            ..State::default()
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            cancelled: Arc::new(AtomicBool::new(false)),
            disposed: Arc::new(AtomicBool::new(false)),
            generation,
            firmware_cancel: Arc::new(AtomicBool::new(false)),
            firmware_progress: Arc::new(Mutex::new(FirmwareProgressDto {
                phase: "idle".into(),
                ratio: None,
                error: None,
            })),
        }
    }

    #[napi]
    pub fn devices(&self) -> AsyncTask<BlockingTask<Vec<DeviceInfo>>> {
        self.spawn(|manager| manager.devices_sync())
    }

    #[napi(js_name = "sendFrame")]
    pub fn send_frame(&self, surface_id: String, bytes: Buffer) -> AsyncTask<BlockingTask<u32>> {
        self.spawn(move |manager| manager.send_frame_sync(surface_id, bytes))
    }

    #[napi(js_name = "sendTile")]
    pub fn send_tile(&self, tile: TileRequest) -> AsyncTask<BlockingTask<u32>> {
        self.spawn(move |manager| manager.send_tile_sync(tile))
    }

    #[napi(js_name = "currentFrameId")]
    pub fn current_frame_id(&self) -> AsyncTask<BlockingTask<u32>> {
        self.spawn(|manager| manager.current_frame_id_sync())
    }

    #[napi(js_name = "pollEvents")]
    pub fn poll_events(&self) -> AsyncTask<BlockingTask<Vec<NativeEvent>>> {
        self.spawn(|manager| manager.poll_events_sync())
    }

    #[napi(js_name = "simulateInput")]
    pub fn simulate_input(
        &self,
        surface_id: String,
        pointer_id: u16,
        phase: String,
        x: u16,
        y: u16,
    ) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(move |manager| {
            unit(manager.simulate_input_sync(surface_id, pointer_id, phase, x, y))
        })
    }

    #[napi(js_name = "sendInput")]
    pub fn send_input(
        &self,
        surface_id: String,
        pointer_id: u16,
        phase: String,
        x: u16,
        y: u16,
    ) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(move |manager| {
            unit(manager.send_input_sync(surface_id, pointer_id, phase, x, y))
        })
    }

    #[napi(js_name = "startMirror")]
    pub fn start_mirror(
        &self,
        surface_id: String,
        mode: String,
        max_fps: u16,
    ) -> AsyncTask<BlockingTask<u32>> {
        self.spawn(move |manager| manager.start_mirror_sync(surface_id, mode, max_fps))
    }

    #[napi(js_name = "stopMirror")]
    pub fn stop_mirror(&self, surface_id: String) -> AsyncTask<BlockingTask<u32>> {
        self.spawn(move |manager| manager.stop_mirror_sync(surface_id))
    }

    #[napi(js_name = "emitMirrorFrame")]
    pub fn emit_mirror_frame(&self) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(|manager| unit(manager.emit_mirror_frame_sync()))
    }

    #[napi(js_name = "takeMirrorFrame")]
    pub fn take_mirror_frame(&self) -> AsyncTask<BlockingTask<Option<MirrorFrameDto>>> {
        self.spawn(|manager| manager.take_mirror_frame_sync())
    }

    #[napi(js_name = "takeMirrorFrameBounded")]
    pub fn take_mirror_frame_bounded(
        &self,
        max_bytes: u32,
    ) -> AsyncTask<BlockingTask<Option<MirrorFrameDto>>> {
        self.spawn(move |manager| manager.take_mirror_frame_bounded_sync(max_bytes))
    }

    #[napi(js_name = "mirrorSkips")]
    pub fn mirror_skips(&self) -> AsyncTask<BlockingTask<f64>> {
        self.spawn(|manager| manager.mirror_skips_sync())
    }

    #[napi(js_name = "mirrorMode")]
    pub fn mirror_mode(&self) -> AsyncTask<BlockingTask<String>> {
        self.spawn(|manager| manager.mirror_mode_sync())
    }

    #[napi(js_name = "mirrorStats")]
    pub fn mirror_stats(&self) -> AsyncTask<BlockingTask<MirrorStatsDto>> {
        self.spawn(|manager| manager.mirror_stats_sync())
    }

    #[napi(js_name = "getConfig")]
    pub fn get_config(&self, key: String) -> AsyncTask<BlockingTask<String>> {
        self.spawn(move |manager| manager.get_config_sync(key))
    }

    #[napi(js_name = "multiTapRadius")]
    pub fn multi_tap_radius(&self) -> AsyncTask<BlockingTask<u16>> {
        self.spawn(|manager| manager.multi_tap_radius_sync())
    }

    #[napi(js_name = "setMultiTapRadius")]
    pub fn set_multi_tap_radius(&self, radius: u32) -> AsyncTask<BlockingTask<u16>> {
        self.spawn(move |manager| manager.set_multi_tap_radius_sync(radius))
    }

    #[napi(js_name = "multiTapMs")]
    pub fn multi_tap_ms(&self) -> AsyncTask<BlockingTask<u32>> {
        self.spawn(|manager| manager.multi_tap_ms_sync())
    }

    #[napi(js_name = "setMultiTapMs")]
    pub fn set_multi_tap_ms(&self, ms: u32) -> AsyncTask<BlockingTask<u32>> {
        self.spawn(move |manager| manager.set_multi_tap_ms_sync(ms))
    }

    #[napi(js_name = "setConfig")]
    pub fn set_config(&self, key: String, value: String) -> AsyncTask<BlockingTask<String>> {
        self.spawn(move |manager| manager.set_config_sync(key, value))
    }

    #[napi(js_name = "setScene")]
    pub fn set_scene(&self, json: String) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(move |manager| unit(manager.set_scene_sync(json)))
    }

    #[napi(js_name = "clearScene")]
    pub fn clear_scene(&self) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(|manager| unit(manager.clear_scene_sync()))
    }

    #[napi(js_name = "setAnimation")]
    pub fn set_animation(&self, clip: String) -> AsyncTask<BlockingTask<String>> {
        self.spawn(move |manager| manager.set_animation_sync(clip))
    }

    #[napi(js_name = "stopAnimation")]
    pub fn stop_animation(&self) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(|manager| unit(manager.stop_animation_sync()))
    }

    #[napi(js_name = "setAnimationBlend")]
    pub fn set_animation_blend(&self, blend: String) -> AsyncTask<BlockingTask<String>> {
        self.spawn(move |manager| manager.set_animation_blend_sync(blend))
    }

    #[napi(js_name = "animationBlend")]
    pub fn animation_blend(&self) -> AsyncTask<BlockingTask<String>> {
        self.spawn(|manager| manager.animation_blend_sync())
    }

    #[napi(js_name = "installPackage")]
    pub fn install_package(&self, bytes: Buffer) -> AsyncTask<BlockingTask<bool>> {
        self.spawn(move |manager| manager.install_package_sync(bytes))
    }

    #[napi(js_name = "packageReady")]
    pub fn package_ready(&self) -> AsyncTask<BlockingTask<bool>> {
        self.spawn(|manager| manager.package_ready_sync())
    }

    #[napi(js_name = "openBoard")]
    pub fn open_board(&self, board_id: String) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(move |manager| unit(manager.open_board_sync(board_id)))
    }

    #[napi(js_name = "simulateDisconnect")]
    pub fn simulate_disconnect(&self) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(|manager| unit(manager.simulate_disconnect_sync()))
    }

    #[napi(js_name = "connectTcp")]
    pub fn connect_tcp(&self, address: String) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(move |manager| unit(manager.connect_tcp_sync(address)))
    }

    #[napi(js_name = "stats")]
    pub fn stats(&self) -> AsyncTask<BlockingTask<SessionStatsDto>> {
        self.spawn(|manager| manager.stats_sync())
    }

    #[napi(js_name = "tileReconciliation")]
    pub fn tile_reconciliation(&self) -> AsyncTask<BlockingTask<TileReconciliationDto>> {
        self.spawn(|manager| manager.tile_reconciliation_sync())
    }

    /// C08: compile an embedded glTF document for the device renderer.
    #[napi(js_name = "compileGltf")]
    pub fn compile_gltf(
        &self,
        bytes: Buffer,
        target: String,
    ) -> AsyncTask<BlockingTask<CompiledPackageDto>> {
        self.spawn(move |manager| manager.compile_gltf_sync(bytes, target))
    }

    #[napi(js_name = "serialPorts")]
    pub fn serial_ports(&self) -> AsyncTask<BlockingTask<Vec<SerialPortDto>>> {
        self.spawn(|manager| manager.serial_ports_sync())
    }

    #[napi(js_name = "capabilities")]
    pub fn capabilities(&self) -> AsyncTask<BlockingTask<CapabilitiesInfo>> {
        self.spawn(|manager| manager.capabilities_sync())
    }

    /// CORE-R06: host-side board facts for firmware preflight.
    #[napi(js_name = "boardFacts")]
    pub fn board_facts(&self, board_id: String) -> AsyncTask<BlockingTask<BoardFactsDto>> {
        self.spawn(move |manager| manager.board_facts_sync(board_id))
    }

    /// CORE-R06: verify a firmware image without opening a port.
    #[napi(js_name = "firmwarePreflight")]
    pub fn firmware_preflight(
        &self,
        request: FirmwarePreflightRequest,
    ) -> AsyncTask<BlockingTask<FirmwarePreflightResult>> {
        self.spawn(move |manager| manager.firmware_preflight_sync(request))
    }

    /// CORE-R06: install a firmware image under an exclusive port lease.
    #[napi(js_name = "firmwareInstall")]
    pub fn firmware_install(
        &self,
        request: FirmwareInstallRequest,
    ) -> AsyncTask<BlockingTask<FirmwareInstallResult>> {
        let cancel = self.firmware_cancel.clone();
        let progress = self.firmware_progress.clone();
        self.spawn(move |manager| manager.firmware_install_sync(request, cancel, progress))
    }

    /// CORE-R06: ask the running install to stop at its next stage boundary.
    #[napi(js_name = "cancelFirmware")]
    pub fn cancel_firmware(&self) -> Result<()> {
        self.firmware_cancel.store(true, AtomicOrdering::Release);
        Ok(())
    }

    /// CORE-R06: last progress snapshot (poll while `firmwareInstall` is pending).
    #[napi(js_name = "firmwareProgress")]
    pub fn firmware_progress(&self) -> Result<FirmwareProgressDto> {
        Ok(self
            .firmware_progress
            .lock()
            .map_err(|_| Error::from_reason("firmware progress lock poisoned"))?
            .clone())
    }

    /// CORE-R06: HELLO readback for the flashed device identity/version.
    #[napi(js_name = "firmwareReadback")]
    pub fn firmware_readback(
        &self,
        request: FirmwareReadbackRequest,
    ) -> AsyncTask<BlockingTask<FirmwareReadbackResult>> {
        self.spawn(move |manager| manager.firmware_readback_sync(request))
    }

    #[napi(js_name = "connectSerial")]
    pub fn connect_serial(
        &self,
        path: String,
        baud: u32,
        reset_on_open: Option<bool>,
    ) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(move |manager| unit(manager.connect_serial_sync(path, baud, reset_on_open)))
    }

    /// CORE-R02: close the session without disposing the manager.
    #[napi(js_name = "disconnect")]
    pub fn disconnect(&self) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(|manager| unit(manager.disconnect_sync()))
    }

    #[napi(js_name = "connectWebSocket")]
    pub fn connect_web_socket(&self, url: String) -> AsyncTask<BlockingTask<Undefined>> {
        self.spawn(move |manager| unit(manager.connect_web_socket_sync(url)))
    }

    #[napi(js_name = "lastFrame")]
    pub fn last_frame(&self) -> AsyncTask<BlockingTask<Option<Buffer>>> {
        self.spawn(|manager| manager.last_frame_sync())
    }

    #[napi(js_name = "emitMetric")]
    pub fn emit_metric(
        &self,
        name: String,
        value: f64,
        unit_name: Option<String>,
    ) -> AsyncTask<BlockingTask<u32>> {
        self.spawn(move |manager| manager.emit_metric_sync(name, value, unit_name))
    }

    #[napi(js_name = "takeDiagnostics")]
    pub fn take_diagnostics(&self) -> AsyncTask<BlockingTask<Vec<DiagnosticDto>>> {
        self.spawn(|manager| manager.take_diagnostics_sync())
    }

    #[napi(js_name = "diagnosticCount")]
    pub fn diagnostic_count(&self) -> AsyncTask<BlockingTask<u32>> {
        self.spawn(|manager| manager.diagnostic_count_sync())
    }

    /// SIM-R05: `dispose` never waits for an in-flight worker. It flips the
    /// shared flags, so the worker's next poll (<= one 400 ms read timeout)
    /// aborts, and cleanup runs on a detached thread.
    #[napi]
    pub fn dispose(&self) -> Result<()> {
        if self.disposed.swap(true, AtomicOrdering::AcqRel) {
            return Ok(());
        }
        self.cancelled.store(true, AtomicOrdering::Release);
        let state = self.state.clone();
        thread::spawn(move || {
            if let Ok(mut state) = state.lock() {
                if let Some(mut sim) = state.sim.take() {
                    sim.session.disconnect();
                }
                state.serial_lease = None;
                state.serial_path = None;
                state.disposed = true;
                state.last_frame = None;
            }
        });
        Ok(())
    }
}
