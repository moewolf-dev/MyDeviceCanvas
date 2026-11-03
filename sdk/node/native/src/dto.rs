//! N-API DTOs. These mirror `sdk/node/index.d.ts`; keep the three copies
//! (Rust, TypeScript, App types) in sync via the interface catalog.
use super::*;

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
