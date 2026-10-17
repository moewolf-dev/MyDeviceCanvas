import test from "node:test";
import assert from "node:assert/strict";
import { colorBarsRgb565, decodeMirrorFrame, formatError, jpegStreamUrl, mirrorPanelStatus, mirrorStreamUrl, normalizeBrightness, normalizeClip, normalizeOpacity, originTile, packageMagicOk, parseMultipartBoundary, sceneRect, planConnection, rejectNonSimEndpoint, streamPanelStatus } from "./demo-logic.js";

test("color bars are RGB565 little-endian and start with red", () => {
  const frame = colorBarsRgb565(8, 1);
  assert.equal(frame.length, 16);
  assert.equal(frame[0], 0x00);
  assert.equal(frame[1], 0xf8);
});

test("origin tile copies the first pixel only", () => {
  const frame = colorBarsRgb565(4, 2);
  const tile = originTile(frame, 4);
  frame[0] = 9;
  assert.deepEqual([...tile], [0x00, 0xf8]);
});

test("non-sim endpoints are refused before any port is opened", () => {
  assert.equal(rejectNonSimEndpoint("sim"), null);
  assert.equal(rejectNonSimEndpoint("tcp:127.0.0.1:9876"), null);
  assert.match(rejectNonSimEndpoint("/dev/cu.usbmodem"), /未打开/);
});

test("tcp plan forwards the address and serial plans open no port", () => {
  const tcp = planConnection("tcp:127.0.0.1:9876");
  assert.equal(tcp.ok, true);
  assert.equal(tcp.mode, "tcp");
  assert.equal(tcp.address, "127.0.0.1:9876");
  assert.equal(tcp.openedPort, false);
  const sim = planConnection("sim");
  assert.equal(sim.mode, "sim");
  assert.equal(sim.openedPort, false);
  const serial = planConnection("/dev/cu.usbmodem");
  assert.equal(serial.ok, false);
  assert.equal(serial.openedPort, false);
  assert.equal(serial.address, undefined);
});

test("formatError prefers Error.message", () => {
  assert.equal(formatError(new Error("未连接")), "未连接");
});

test("decodeMirrorFrame expands RGB565 and validates length", () => {
  const pixels = colorBarsRgb565(2, 1);
  const decoded = decodeMirrorFrame({ surfaceId: "main", frameId: 5, width: 2, height: 1, pixels });
  assert.equal(decoded.rgba.length, 2 * 1 * 4);
  assert.ok(decoded.rgba[0] > 200, "first bar is red");
  assert.equal(decoded.frameId, 5);
  assert.throws(
    () => decodeMirrorFrame({ width: 2, height: 1, pixels: new Uint8Array(2) }),
    /length/,
  );
  assert.throws(() => decodeMirrorFrame({ width: 0, height: 1, pixels: new Uint8Array(0) }), /positive/);
});

test("mirrorPanelStatus summarizes counters or reports n/a", () => {
  assert.match(
    mirrorPanelStatus({ framesReceived: 3, framesDropped: 1, lastFrameId: 9 }),
    /3 frames/,
  );
  assert.equal(mirrorPanelStatus(null), "mirror: n/a");
});

test("normalizeBrightness accepts 0..255 and rejects the rest", () => {
  assert.equal(normalizeBrightness("128"), "128");
  assert.equal(normalizeBrightness(0), "0");
  assert.throws(() => normalizeBrightness(256), /0\.\.255/);
  assert.throws(() => normalizeBrightness("x"), /0\.\.255/);
});

test("normalizeClip maps empty input to off", () => {
  assert.equal(normalizeClip("idle"), "idle");
  assert.equal(normalizeClip("  "), "off");
  assert.equal(normalizeClip(null), "off");
});

test("sceneRect validates geometry and colour", () => {
  assert.deepEqual(sceneRect(1, 2, 3, 4, "#00FF00"), {
    name: "rect", kind: "rect", rect: [1, 2, 3, 4], value: "#00FF00",
  });
  assert.throws(() => sceneRect(0, 0, -1, 4, "#FFFFFF"), /non-negative/);
  assert.throws(() => sceneRect(0, 0, 1, 1, "blue"), /RRGGBB/);
});

test("packageMagicOk checks the MDCP magic", () => {
  assert.equal(packageMagicOk(new Uint8Array([0x4d, 0x44, 0x43, 0x50, 1])), true);
  assert.equal(packageMagicOk(new Uint8Array([1, 2, 3, 4])), false);
  assert.equal(packageMagicOk(new Uint8Array([0x4d, 0x44])), false);
});

test("mirrorStreamUrl builds a live stream endpoint", () => {
  assert.equal(mirrorStreamUrl("http://127.0.0.1:8080"), "http://127.0.0.1:8080/stream");
  assert.equal(mirrorStreamUrl("http://127.0.0.1:8080/"), "http://127.0.0.1:8080/stream");
  assert.throws(() => mirrorStreamUrl("127.0.0.1:8080"), /http/);
  assert.throws(() => mirrorStreamUrl(""), /required/);
});

test("parseMultipartBoundary extracts the boundary", () => {
  assert.equal(parseMultipartBoundary("multipart/x-mixed-replace; boundary=mdcframe"), "mdcframe");
  assert.equal(parseMultipartBoundary('multipart/x-mixed-replace; boundary="x"'), "x");
  assert.equal(parseMultipartBoundary("image/bmp"), null);
});

test("streamPanelStatus reports frames and clipping", () => {
  assert.equal(streamPanelStatus(null), "stream: disconnected");
  assert.match(
    streamPanelStatus({ connected: true, frames: 5, dropped: 2 }),
    /5 frames, 2 clipped/,
  );
});

test("jpegStreamUrl builds the JPEG live stream endpoint", () => {
  assert.equal(jpegStreamUrl("http://127.0.0.1:8080"), "http://127.0.0.1:8080/stream.jpg");
});

test("normalizeOpacity accepts 0..255 and rejects the rest", () => {
  assert.equal(normalizeOpacity(128), 128);
  assert.equal(normalizeOpacity(0), 0);
  assert.equal(normalizeOpacity(255), 255);
  assert.throws(() => normalizeOpacity(256), /0\.\.255/);
  assert.throws(() => normalizeOpacity(-1), /0\.\.255/);
});
