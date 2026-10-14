/** RGB565 little-endian color bars shared by the Electron main process and tests. */

export const BAR_COLORS = [
  0xf800, 0x07e0, 0x001f, 0xffe0, 0x07ff, 0xf81f, 0xffff, 0x0000,
];

export function colorBarsRgb565(width, height) {
  if (!Number.isInteger(width) || !Number.isInteger(height) || width <= 0 || height <= 0) {
    throw new TypeError("width and height must be positive integers");
  }
  const bytes = width * height * 2;
  if (!Number.isSafeInteger(bytes)) {
    throw new RangeError("frame size overflow");
  }
  const out = new Uint8Array(bytes);
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const bar = Math.min(BAR_COLORS.length - 1, Math.floor((x * BAR_COLORS.length) / width));
      const color = BAR_COLORS[bar];
      const i = (y * width + x) * 2;
      out[i] = color & 0xff;
      out[i + 1] = (color >> 8) & 0xff;
    }
  }
  return out;
}

/** Top-left 1×1 tile taken from an RGB565 frame. */
export function originTile(frame, width) {
  if (!(frame instanceof Uint8Array) || frame.length < 2 || width <= 0) {
    throw new TypeError("frame must contain at least one RGB565 pixel");
  }
  return frame.slice(0, 2);
}

/** Non-sim endpoints are refused here so the demo never opens a serial port implicitly. */
export function rejectNonSimEndpoint(endpoint) {
  const target = String(endpoint ?? "sim").trim() || "sim";
  if (target === "sim" || target.startsWith("tcp:")) return null;
  return `端点 ${target} 未打开。本演示只通过公开 SDK 连接模拟器；串口和网络地址不会被静默打开。`;
}

/**
 * Decide how the main process connects. Serial paths are rejected here and
 * never produce a port to open.
 */
export function planConnection(endpoint) {
  const rejected = rejectNonSimEndpoint(endpoint);
  if (rejected) {
    return { ok: false, error: rejected, openedPort: false };
  }
  const target = String(endpoint ?? "sim").trim() || "sim";
  if (target.startsWith("tcp:")) {
    const address = target.slice("tcp:".length);
    if (!address) {
      return { ok: false, error: "tcp 地址为空", openedPort: false };
    }
    return { ok: true, mode: "tcp", address, endpoint: target, openedPort: false };
  }
  return { ok: true, mode: "sim", endpoint: target, openedPort: false };
}

export function formatError(error) {
  if (error && typeof error.message === "string" && error.message.length > 0) {
    return error.message;
  }
  return String(error);
}

/** Expand RGB565 LE into RGBA for canvas preview. Does not talk to a device. */
export function rgb565ToRgba(bytes, width, height) {
  const data = new Uint8ClampedArray(width * height * 4);
  for (let i = 0; i < width * height; i += 1) {
    const pixel = bytes[i * 2] | (bytes[i * 2 + 1] << 8);
    const r = ((pixel >> 11) & 31) * 255 / 31;
    const g = ((pixel >> 5) & 63) * 255 / 63;
    const b = (pixel & 31) * 255 / 31;
    data[i * 4] = r;
    data[i * 4 + 1] = g;
    data[i * 4 + 2] = b;
    data[i * 4 + 3] = 255;
  }
  return data;
}

/**
 * Decode a Form B mirror frame DTO (from `onMirrorFrame`/`takeMirrorFrame`) into
 * an RGBA preview. Pure logic: no device or Electron access.
 */
export function decodeMirrorFrame(frame) {
  if (!frame || !(frame.pixels instanceof Uint8Array)) {
    throw new TypeError("mirror frame pixels must be a Uint8Array");
  }
  const { width, height } = frame;
  if (!Number.isInteger(width) || !Number.isInteger(height) || width <= 0 || height <= 0) {
    throw new TypeError("mirror geometry must be positive integers");
  }
  if (frame.pixels.length !== width * height * 2) {
    throw new RangeError(
      `mirror pixel length ${frame.pixels.length} != ${width}x${height} RGB565`,
    );
  }
  return {
    surfaceId: String(frame.surfaceId ?? "main"),
    frameId: Number(frame.frameId ?? 0),
    width,
    height,
    rgba: rgb565ToRgba(frame.pixels, width, height),
  };
}

/** One-line status for the mirror panel; `null` stats read as unavailable. */
export function mirrorPanelStatus(stats) {
  if (!stats) return "mirror: n/a";
  return `mirror: ${stats.framesReceived} frames, ${stats.framesDropped} dropped, last #${stats.lastFrameId}`;
}

/** N56: validate a brightness input before it reaches the device. */
export function normalizeBrightness(value) {
  const n = Number(value);
  if (!Number.isInteger(n) || n < 0 || n > 255) {
    throw new RangeError("brightness must be an integer 0..255");
  }
  return String(n);
}

/** N75: normalize a clip-name input; empty becomes the stop sentinel. */
export function normalizeClip(value) {
  const text = String(value ?? "").trim();
  return text.length === 0 ? "off" : text;
}

/** N86: build a scene rect node with validated geometry and colour. */
export function sceneRect(x, y, w, h, color) {
  for (const [name, value] of Object.entries({ x, y, w, h })) {
    if (!Number.isInteger(value) || value < 0) {
      throw new RangeError(`${name} must be a non-negative integer`);
    }
  }
  const text = String(color ?? "");
  if (!/^#?[0-9a-fA-F]{6}$/.test(text)) {
    throw new RangeError("color must be RRGGBB");
  }
  return { name: "rect", kind: "rect", rect: [x, y, w, h], value: text };
}
