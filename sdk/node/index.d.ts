export interface SurfaceInfo {
  id: string;
  width: number;
  height: number;
  /** Canonical wire name, e.g. "RGB565", "RGB888"; N307. */
  pixelFormat: string;
  /** N307: only RGB565 has an implemented host/frame path. */
  implemented: boolean;
  stride: number;
  rotation: number;
  /** N735: pan midpoint translation in pixels (0 for other gestures). */
  dx: number;
  dy: number;
}
export interface DeviceInfo {
  deviceId: string;
  firmware: string;
  surfaces: SurfaceInfo[];
}
/** APP-R03: one serial endpoint hint. */
export interface SerialPortInfo {
  path: string;
  manufacturer?: string;
  product?: string;
  serialNumber?: string;
}
/** CORE-R06: firmware metadata, mirroring the signed catalog artifact. */
export interface FirmwareArtifact {
  boardId: string;
  mcu: string;
  runtime: string;
  protocolMajor: number;
  protocolMinor: number;
  imageSize: number;
  sha256: string;
  source: string;
  publicKey?: Uint8Array;
  signature?: Uint8Array;
}
/** CORE-R06: what the host knows about the target board. */
export interface FirmwareBoardFacts {
  boardId: string;
  mcu: string;
  /** Physical flash bytes; `null`/omitted means Unknown (never guessed). */
  flashBytes?: number | null;
  profileVerified: boolean;
}
export interface FirmwareImageGeometry {
  chipFamily: string;
  flashBytes: number;
}
export interface FirmwarePreflightRequest {
  image: Uint8Array;
  artifact: FirmwareArtifact;
  board: FirmwareBoardFacts;
  hostProtocolMajor: number;
  hostProtocolMinor: number;
  detectedChipFamily?: string;
  imageGeometry?: FirmwareImageGeometry;
  allowUnknownFlash?: boolean;
  flashIdUnusable?: boolean;
}
export type FirmwarePreflightVerdict = "ok" | "okUnknownFlash" | "reject";
export interface FirmwarePreflightResult {
  ok: boolean;
  verdict: FirmwarePreflightVerdict;
  reason?: string | null;
}
export interface FirmwareInstallRequest {
  preflight: FirmwarePreflightRequest;
  port: string;
  expectedDeviceId: string;
  /** `sim` never opens the port; `esptool` writes a bounded temp image. */
  mode?: "sim" | "esptool";
  esptoolPath?: string;
  /** esptool only: default true (records argv, writes nothing). */
  dryRun?: boolean;
  /** Simulator only: delay the fake write so progress/cancel are observable. */
  simDelayMs?: number;
  /** Simulator only: identity the fake flasher reads back (defaults to expected). */
  simReadbackDeviceId?: string;
}
export interface FirmwareProgress {
  phase:
    | "idle"
    | "preflight"
    | "lease"
    | "writing"
    | "rebooting"
    | "readback"
    | "done"
    | "cancelled"
    | "failed";
  ratio: number | null;
  error: string | null;
}
export interface FirmwareInstallResult {
  ok: boolean;
  phase: string;
  deviceId: string | null;
  error: string | null;
}
export interface FirmwareReadbackRequest {
  port: string;
  timeoutMs?: number;
  baud?: number;
}
/** CORE-R06: the identity the device advertises over a real HELLO. */
export interface FirmwareReadback {
  deviceId: string;
  boardId: string;
  protocolVersion: number;
  firmware: string;
}
/** APP-R03: negotiated capabilities for the active session. */
export interface CapabilitiesInfo extends DeviceInfo {
  boardId: string;
  protocolVersion: number;
  frame: boolean;
  tile: boolean;
  touch: boolean;
  ota: boolean;
  maxMessage: number;
  maxChunk: number;
  maxInFlight: number;
  maxFps: number;
  mirror: boolean;
  mirrorModes: string[];
  mirrorMaxFps: number;
  localDisplayFps: number;
  mirrorFps: number;
  mirrorBandwidth: number;
  configKeys: string[];
  scene: boolean;
  sceneMaxNodes: number;
  sceneMaxDepth: number;
  bootId: number;
  runId: number;
}
export interface TileRequest {
  x: number;
  y: number;
  width: number;
  height: number;
  baseFrameId: number;
  bytes: Uint8Array;
}
/** N474: a recognised pointer gesture. */
export interface GestureDto {
  kind: "tap" | "long-press" | "drag" | "pinch" | "pan" | "swipe" | "double-tap" | "triple-tap";
  pointerId: number;
  x: number;
  y: number;
  /** N493: the hit-tested scene node, when reported. */
  node?: string;
  /** N725: pinch distance ratio in permille (1000 for other gestures). */
  scale: number;
  /** N725: pinch angle change in degrees (0 for other gestures). */
  rotation: number;
}

export interface InputEventDto {
  /** N418: the hit-tested scene node, when the device reports one. */
  node?: string;
  deviceId: string;
  surfaceId: string;
  pointerId: number;
  phase: string;
  x: number;
  y: number;
}
export interface MirrorInfo {
  surfaceId: string;
  frameId: number;
  width: number;
  height: number;
}
export interface MirrorFrame extends MirrorInfo {
  pixels: Uint8Array;
}
export interface MirrorStats {
  framesReceived: number;
  framesDropped: number;
  lastFrameId: number;
}
/** N576: partial-update tile reconciliation. */
export interface TileReconciliation {
  sent: number;
  received: number;
  missing: number;
  bytesSent: number;
  consistent: boolean;
}
/** N349: one diagnostics record. */
export interface DiagnosticDto {
  kind: string;
  name: string;
  value: number;
  unit?: string;
  /** JSON of the telemetry fields (telemetry only). */
  fields?: string;
}
export interface CompiledPackage {
  bytes: Uint8Array;
  target: "sprite" | "mesh3d";
  nodes: number;
  meshes: number;
  animations: string[];
  estimatedBytes: number;
}
export interface NativeEvent {
  /** SIM-R04: session generation; drop events older than the current link. */
  generation: number;
  kind: string;
  deviceId?: string;
  input?: InputEventDto;
  /** N474: a recognised gesture. */
  gesture?: GestureDto;
  message?: string;
  mirror?: MirrorInfo;
}
export declare function loadNative(): unknown | null;
export declare class Surface {
  readonly info: SurfaceInfo;
  sendFrame(bytes: Uint8Array): Promise<number> | number;
  sendTile(tile: TileRequest): Promise<number> | number;
}
export declare class Device {
  readonly info: DeviceInfo;
  readonly surfaces: Map<string, Surface>;
  surface(id: string): Surface | undefined;
  dispose(): void;
}
export declare class MyDeviceCanvas {
  constructor(native?: unknown);
  static create(): Promise<MyDeviceCanvas>;
  nativeLoaded(): boolean;
  currentFrameId(): Promise<number>;
  connectTcp(address: string): Promise<void>;
  /** N430/APP-R03: open a named serial port and perform HELLO. */
  connectSerial(path: string, baud?: number, resetOnOpen?: boolean): Promise<void>;
  /** N340: connect over WebSocket (`ws://host:port`). */
  connectWebSocket(url: string): Promise<void>;
  /** CORE-R02: close the session, release the serial lease, keep the manager. */
  disconnect(): Promise<void>;
  devices(): Promise<Device[]>;
  capabilities(): Promise<CapabilitiesInfo>;
  serialPorts(): Promise<SerialPortInfo[]>;
  /** N349: emit a simulator metric; returns the queue length. */
  emitMetric(name: string, value: number, unit?: string | null): Promise<number>;
  /** N349: drain diagnostics (metrics, boot info, telemetry). */
  takeDiagnostics(): Promise<DiagnosticDto[]>;
  diagnosticCount(): Promise<number>;
  pollEvents(): Promise<NativeEvent[]>;
  onMirrorFrame(listener: (frame: MirrorInfo) => void): () => void;
  /** N559: restrict a mirror mode to `full-frame` | `dirty-region`. */
  static normalizeMirrorMode(mode?: string | null): "full-frame" | "dirty-region";
  startMirror(
    surfaceId: string,
    options?: { mode?: "full-frame" | "dirty-region"; maxFps?: number },
  ): Promise<number>;
  /** N559: the negotiated mirror mode (parity with Swift/C; "" before confirm). */
  mirrorMode(): Promise<string>;
  stopMirror(surfaceId: string): Promise<number>;
  /** Sim-only: push one mirrored frame from the local FakeDevice. */
  emitMirrorFrame(): Promise<void>;
  takeMirrorFrame(): Promise<MirrorFrame | null>;
  /** N528: take the newest frame only when it fits `maxBytes` (else null). */
  takeMirrorFrameBounded(maxBytes: number): Promise<MirrorFrame | null>;
  /** N528: frames dropped because the caller's cap was too small. */
  mirrorSkips(): Promise<number>;
  mirrorStats(): Promise<MirrorStats | null>;
  /** N576: partial-update tiles sent/received/missing (parity with C). */
  tileReconciliation(): Promise<TileReconciliation | null>;
  /** N784: multi-tap radius in pixels. */
  multiTapRadius(): Promise<number>;
  /** N784: set the multi-tap radius (`0`..=`4096`). */
  setMultiTapRadius(radius: number): Promise<number>;
  /** N794: multi-tap window in milliseconds. */
  multiTapMs(): Promise<number>;
  /** N794: set the multi-tap window (`0`..=`60000`). */
  setMultiTapMs(ms: number): Promise<number>;
  getConfig(key: string): Promise<string>;
  setConfig(key: string, value: string | number): Promise<string>;
  setScene(node: { name: string; kind: string; children?: unknown[]; value?: string }): Promise<void>;
  clearScene(): Promise<void>;
  setAnimation(clip: string): Promise<string>;
  stopAnimation(): Promise<void>;
  /** N837: blend clips as `"clip:weight,clip:weight"`; `"off"` stops. */
  setAnimationBlend(blend: string): Promise<string>;
  /** N837: the accepted blend, or `"off"`. */
  animationBlend(): Promise<string>;
  /** N234: upload a runtime package (`.mdcpkg` bytes). */
  compileGltf(
    bytes: Uint8Array,
    options?: { target?: "sprite" | "mesh3d" },
  ): Promise<CompiledPackage>;
  installPackage(bytes: Uint8Array): Promise<boolean>;
  /** N238: true once the last accepted package is render-ready. */
  packageReady(): Promise<boolean>;
  /** CORE-R06: verify a firmware image without opening a port. */
  firmwarePreflight(request: FirmwarePreflightRequest): Promise<FirmwarePreflightResult>;
  /** CORE-R06: preflight + lease + write + reset + identity readback. */
  firmwareInstall(request: FirmwareInstallRequest): Promise<FirmwareInstallResult>;
  /** CORE-R06: last progress snapshot; poll while an install is pending. */
  firmwareProgress(): Promise<FirmwareProgress>;
  /** CORE-R06: cancel at the next stage boundary. */
  cancelFirmware(): Promise<void>;
  /** CORE-R06: real HELLO identity/version readback on the leased port. */
  firmwareReadback(request: FirmwareReadbackRequest): Promise<FirmwareReadback>;
  on(event: string, listener: (value: unknown) => void): () => void;
  off(event: string, listener: (value: unknown) => void): boolean;
  dispose(): void;
}
