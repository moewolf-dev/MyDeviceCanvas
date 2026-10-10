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
  if (target === "sim") return null;
  return `端点 ${target} 未打开。本演示只通过公开 SDK 连接模拟器；串口和网络地址不会被静默打开。`;
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
