export class UnsupportedError extends Error {
  constructor(message = "feature is not supported by device") {
    super(message);
    this.name = "UnsupportedError";
  }
}

export class Surface {
  constructor(native, info) {
    this._native = native;
    this.info = Object.freeze({ ...info });
  }
  sendFrame(buffer) {
    if (!(buffer instanceof Uint8Array)) {
      throw new TypeError("frame must be a Uint8Array or Buffer");
    }
    const copy = new Uint8Array(buffer);
    return this._native.sendFrame(this.info.id, copy);
  }
  sendTile(tile) {
    if (!tile || !(tile.bytes instanceof Uint8Array)) {
      throw new TypeError("tile.bytes must be a Uint8Array or Buffer");
    }
    const copy = new Uint8Array(tile.bytes);
    return this._native.sendTile({
      ...tile,
      surfaceId: this.info.id,
      bytes: copy,
    });
  }
}

export class Device {
  constructor(native, dto) {
    this._native = native;
    this.info = Object.freeze({ ...dto });
    this.surfaces = new Map(
      dto.surfaces.map((s) => [s.id, new Surface(native, s)]),
    );
  }
  surface(id) {
    return this.surfaces.get(id);
  }
  dispose() {
    this._native.dispose?.();
  }
}

export class MyDeviceCanvas {
  constructor(native = {}) {
    this._native = native;
    this._disposed = false;
    this._listeners = new Map();
  }
  static async create() {
    const native = loadNative();
    return new MyDeviceCanvas(
      native?.NativeManager ? new native.NativeManager() : {},
    );
  }
  /** True when a native Manager with frame send is bound. */
  nativeLoaded() {
    return (
      typeof this._native.devices === "function" &&
      typeof this._native.sendFrame === "function"
    );
  }
  async currentFrameId() {
    if (this._disposed) throw new Error("manager is disposed");
    return (await this._native.currentFrameId?.()) ?? 0;
  }
  async connectTcp(address) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.connectTcp !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.connectTcp(address);
  }
  /**
   * N430/APP-R03: open a named serial port and perform HELLO.
   * `baud` is a positive integer or 0 for the native default (115200).
   */
  async connectSerial(path, baud = 115200) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof path !== "string" || path.trim().length === 0) {
      throw new TypeError("serial path must be a non-empty string");
    }
    if (!Number.isInteger(baud) || baud < 0 || baud > 4_000_000) {
      throw new RangeError("serial baud must be an integer in 0..4000000");
    }
    if (typeof this._native.connectSerial !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.connectSerial(path.trim(), baud);
  }
  /** N340: connect over WebSocket (`ws://host:port`). */
  async connectWebSocket(url) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.connectWebSocket !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.connectWebSocket(url);
  }
  async devices() {
    if (this._disposed) throw new Error("manager is disposed");
    return (await (this._native.devices?.() ?? [])).map(
      (d) => new Device(this._native, d),
    );
  }
  /** APP-R03: negotiated capabilities (mirror/config/scene) for the active session. */
  async capabilities() {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.capabilities !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.capabilities();
  }
  /** N349: emit a metric on the simulator device's diagnostics channel. */
  async emitMetric(name, value, unit) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.emitMetric !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.emitMetric(name, value, unit ?? null);
  }
  /** N349: drain diagnostics (metrics, boot info, telemetry). */
  async takeDiagnostics() {
    if (this._disposed) throw new Error("manager is disposed");
    return (await this._native.takeDiagnostics?.()) ?? [];
  }
  async diagnosticCount() {
    if (this._disposed) throw new Error("manager is disposed");
    return (await this._native.diagnosticCount?.()) ?? 0;
  }
  /** Open a named simulator board, then return its devices. Default stays 480×320. */
  async openBoard(boardId) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.openBoard !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    await this._native.openBoard(boardId);
    return this.devices();
  }
  async simulateDisconnect() {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.simulateDisconnect !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    await this._native.simulateDisconnect();
  }
  /** Drain native ManagerEvent DTOs and re-emit as SDK events. */
  async pollEvents() {
    if (this._disposed) throw new Error("manager is disposed");
    const events = (await this._native.pollEvents?.()) ?? [];
    for (const event of events) {
      if (event.kind === "input" && event.input) {
        this.emit("input", event.input);
      } else if (event.kind === "error") {
        this.emit("error", new Error(event.message ?? "device error"));
      } else if (event.kind === "mirror" && event.mirror) {
        this.emit("mirror", event.mirror);
      } else {
        this.emit(event.kind, event);
      }
    }
    return events;
  }
  /** Subscribe to Form B mirrored frame metadata. Returns an unsubscribe fn. */
  onMirrorFrame(listener) {
    return this.on("mirror", listener);
  }
  /** N559: restrict a mirror mode to the two the protocol negotiates. */
  static normalizeMirrorMode(mode) {
    if (mode === undefined || mode === null || mode === "") return "full-frame";
    if (mode === "full-frame" || mode === "dirty-region") return mode;
    throw new RangeError(`unknown mirror mode: ${mode}`);
  }
  /** Form B: ask the device to start mirroring `surfaceId`. */
  async startMirror(surfaceId, { mode = "full-frame", maxFps = 30 } = {}) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.startMirror !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    const normalized = MyDeviceCanvas.normalizeMirrorMode(mode);
    return this._native.startMirror(surfaceId, normalized, maxFps);
  }
  /** N559: the negotiated mirror mode (parity with Swift/C; "" before confirm). */
  async mirrorMode() {
    if (this._disposed) throw new Error("manager is disposed");
    return (await this._native.mirrorMode?.()) ?? "";
  }
  async stopMirror(surfaceId) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.stopMirror !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.stopMirror(surfaceId);
  }
  /** Sim-only: push one mirrored frame from the local FakeDevice. */
  async emitMirrorFrame() {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.emitMirrorFrame !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    await this._native.emitMirrorFrame();
  }
  /** N528: take the newest frame only when it fits `maxBytes` (else null). */
  async takeMirrorFrameBounded(maxBytes) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.takeMirrorFrameBounded !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return (await this._native.takeMirrorFrameBounded(maxBytes)) ?? null;
  }
  /** N528: mirrored frames dropped because the caller's cap was too small. */
  async mirrorSkips() {
    if (this._disposed) throw new Error("manager is disposed");
    return (await this._native.mirrorSkips?.()) ?? 0;
  }
  /** Drain the latest complete mirrored frame (pixels copied out of native). */
  async takeMirrorFrame() {
    if (this._disposed) throw new Error("manager is disposed");
    return (await this._native.takeMirrorFrame?.()) ?? null;
  }
  async mirrorStats() {
    if (this._disposed) throw new Error("manager is disposed");
    return (await this._native.mirrorStats?.()) ?? null;
  }
  /** N576: partial-update tiles sent/received/missing (parity with C). */
  async tileReconciliation() {
    if (this._disposed) throw new Error("manager is disposed");
    return (await this._native.tileReconciliation?.()) ?? null;
  }
  /** N784: the multi-tap radius in pixels (default 24 before a session exists). */
  async multiTapRadius() {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.multiTapRadius !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.multiTapRadius();
  }
  /** N784: set the multi-tap radius. `0` is exact-pixel; above 4096 throws. */
  async setMultiTapRadius(radius) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.setMultiTapRadius !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    const value = Number(radius);
    if (!Number.isInteger(value) || value < 0 || value > 4096) {
      throw new Error("multi_tap_radius out of range");
    }
    return this._native.setMultiTapRadius(value);
  }
  /** N794: the multi-tap window in milliseconds (default 300 before a session exists). */
  async multiTapMs() {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.multiTapMs !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.multiTapMs();
  }
  /** N794: set the multi-tap window. `0` is simultaneous-only; above 60000 throws. */
  async setMultiTapMs(ms) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.setMultiTapMs !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    const value = Number(ms);
    if (!Number.isInteger(value) || value < 0 || value > 60000) {
      throw new Error("multi_tap_ms out of range");
    }
    return this._native.setMultiTapMs(value);
  }
  /** N42: read a device configuration value. */
  async getConfig(key) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.getConfig !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.getConfig(key);
  }
  /** N42: write a device configuration value; returns the accepted value. */
  async setConfig(key, value) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.setConfig !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.setConfig(key, String(value));
  }
  /** N55: set a vector scene (plain object serialized to the SceneNode JSON). */
  async setScene(node) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.setScene !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.setScene(JSON.stringify(node ?? {}));
  }
  async clearScene() {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.clearScene !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.clearScene();
  }
  /** N74: play a baked-atlas clip; returns the accepted clip name ("off" when stopped). */
  async setAnimation(clip) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.setAnimation !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.setAnimation(clip);
  }
  async stopAnimation() {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.stopAnimation !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.stopAnimation();
  }
  /** N837: blend clips as `"clip:weight,clip:weight"`; `"off"` stops. */
  async setAnimationBlend(blend) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.setAnimationBlend !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    if (typeof blend !== "string") {
      throw new TypeError("setAnimationBlend expects a string");
    }
    return this._native.setAnimationBlend(blend);
  }
  /** N837: the accepted blend, or `"off"`. */
  async animationBlend() {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.animationBlend !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    return this._native.animationBlend();
  }
  /** N234: upload a runtime package (Uint8Array of `.mdcpkg` bytes). */
  async installPackage(bytes) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof this._native.installPackage !== "function") {
      throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）");
    }
    if (!(bytes instanceof Uint8Array)) {
      throw new TypeError("installPackage expects a Uint8Array");
    }
    return this._native.installPackage(Buffer.from(bytes));
  }
  /** N238: true once the last accepted package is render-ready. */
  async packageReady() {
    if (this._disposed) throw new Error("manager is disposed");
    return (await this._native.packageReady?.()) ?? false;
  }
  on(event, listener) {
    if (this._disposed) throw new Error("manager is disposed");
    if (typeof listener !== "function") {
      throw new TypeError("listener must be a function");
    }
    const listeners = this._listeners.get(event) ?? new Set();
    listeners.add(listener);
    this._listeners.set(event, listeners);
    return () => this.off(event, listener);
  }
  off(event, listener) {
    const listeners = this._listeners.get(event);
    if (!listeners) return false;
    const removed = listeners.delete(listener);
    if (listeners.size === 0) this._listeners.delete(event);
    return removed;
  }
  emit(event, value) {
    if (this._disposed) return;
    for (const listener of this._listeners.get(event) ?? []) listener(value);
  }
  dispose() {
    this._disposed = true;
    this._listeners.clear();
    this._native.dispose?.();
  }
}

import { createRequire } from "node:module";

export function loadNative() {
  try {
    return createRequire(import.meta.url)("./mdc-node-native.node");
  } catch {
    return null;
  }
}
