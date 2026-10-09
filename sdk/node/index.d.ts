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
export interface NativeEvent {
  kind: string;
  deviceId?: string;
  input?: InputEventDto;
  message?: string;
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
  devices(): Promise<Device[]>;
  pollEvents(): Promise<NativeEvent[]>;
  on(event: string, listener: (value: unknown) => void): () => void;
  off(event: string, listener: (value: unknown) => void): boolean;
  dispose(): void;
}
