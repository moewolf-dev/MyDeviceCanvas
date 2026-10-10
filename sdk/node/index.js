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
  async devices() {
    if (this._disposed) throw new Error("manager is disposed");
    return (await (this._native.devices?.() ?? [])).map(
      (d) => new Device(this._native, d),
    );
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
      } else {
        this.emit(event.kind, event);
      }
    }
    return events;
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
