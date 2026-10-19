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
}
export interface DeviceInfo {
  deviceId: string;
  firmware: string;
  surfaces: SurfaceInfo[];
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
  kind: "tap" | "long-press" | "drag";
  pointerId: number;
  x: number;
  y: number;
  /** N493: the hit-tested scene node, when reported. */
  node?: string;
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
/** N349: one diagnostics record. */
export interface DiagnosticDto {
  kind: string;
  name: string;
  value: number;
  unit?: string;
  /** JSON of the telemetry fields (telemetry only). */
  fields?: string;
}
export interface NativeEvent {
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
  /** N340: connect over WebSocket (`ws://host:port`). */
  connectWebSocket(url: string): Promise<void>;
  devices(): Promise<Device[]>;
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
  getConfig(key: string): Promise<string>;
  setConfig(key: string, value: string | number): Promise<string>;
  setScene(node: { name: string; kind: string; children?: unknown[]; value?: string }): Promise<void>;
  clearScene(): Promise<void>;
  setAnimation(clip: string): Promise<string>;
  stopAnimation(): Promise<void>;
  /** N234: upload a runtime package (`.mdcpkg` bytes). */
  installPackage(bytes: Uint8Array): Promise<boolean>;
  /** N238: true once the last accepted package is render-ready. */
  packageReady(): Promise<boolean>;
  on(event: string, listener: (value: unknown) => void): () => void;
  off(event: string, listener: (value: unknown) => void): boolean;
  dispose(): void;
}
