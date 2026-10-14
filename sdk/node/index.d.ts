export interface SurfaceInfo {
  id: string;
  width: number;
  height: number;
  pixelFormat: string;
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
export interface InputEventDto {
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
export interface NativeEvent {
  kind: string;
  deviceId?: string;
  input?: InputEventDto;
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
  devices(): Promise<Device[]>;
  pollEvents(): Promise<NativeEvent[]>;
  onMirrorFrame(listener: (frame: MirrorInfo) => void): () => void;
  startMirror(
    surfaceId: string,
    options?: { mode?: string; maxFps?: number },
  ): Promise<number>;
  stopMirror(surfaceId: string): Promise<number>;
  takeMirrorFrame(): Promise<MirrorFrame | null>;
  mirrorStats(): Promise<MirrorStats | null>;
  on(event: string, listener: (value: unknown) => void): () => void;
  off(event: string, listener: (value: unknown) => void): boolean;
  dispose(): void;
}
