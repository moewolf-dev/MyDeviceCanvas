import test from "node:test";
import assert from "node:assert/strict";
import { MyDeviceCanvas, loadNative } from "./index.js";

test("native module is optional until a platform artifact is installed", () => {
  assert.equal(loadNative(), null);
});

test("sendFrame copies caller buffer before native dispatch", async () => {
  let received;
  const mdc = new MyDeviceCanvas({
    async devices() { return [{ deviceId: "test", firmware: "0", surfaces: [{ id: "main", width: 1, height: 1, pixelFormat: "RGB565", stride: 2, rotation: 0 }] }]; },
    sendFrame(_id, bytes) { received = bytes; return Promise.resolve(); }
  });
  const surface = (await mdc.devices())[0].surface("main");
  const source = new Uint8Array([1, 2]);
  await surface.sendFrame(source);
  source[0] = 9;
  assert.deepEqual([...received], [1, 2]);
});

test("installPackage forwards bytes and reports readiness", async () => {
  let received;
  const mdc = new MyDeviceCanvas({
    installPackage(bytes) { received = bytes; return true; },
    packageReady() { return true; },
  });
  const pkg = new Uint8Array([0x4d, 0x44, 0x43, 0x50]);
  assert.equal(await mdc.installPackage(pkg), true);
  assert.deepEqual([...received], [0x4d, 0x44, 0x43, 0x50]);
  assert.equal(await mdc.packageReady(), true);
  await assert.rejects(mdc.installPackage("nope"), /Uint8Array/);
});

test("dispose prevents new device calls", async () => {
  const mdc = new MyDeviceCanvas({ devices: async () => [] });
  mdc.dispose();
  await assert.rejects(mdc.devices(), /disposed/);
});

test("dispose cancels SDK event listeners", () => {
  const mdc = new MyDeviceCanvas();
  let calls = 0;
  mdc.on("error", () => { calls += 1; });
  mdc.dispose();
  assert.doesNotThrow(() => mdc.emit("error", new Error("late")));
  assert.equal(calls, 0);
});

test("sendTile copies caller buffer and forwards surfaceId", async () => {
  let received;
  const mdc = new MyDeviceCanvas({
    async devices() {
      return [{
        deviceId: "test",
        firmware: "0",
        surfaces: [{ id: "main", width: 2, height: 2, pixelFormat: "RGB565", stride: 4, rotation: 0 }],
      }];
    },
    sendTile(tile) {
      received = tile;
      return Promise.resolve(1);
    },
  });
  const surface = (await mdc.devices())[0].surface("main");
  const source = new Uint8Array([1, 2]);
  await surface.sendTile({ x: 0, y: 0, width: 1, height: 1, baseFrameId: 1, bytes: source });
  source[0] = 9;
  assert.equal(received.surfaceId, "main");
  assert.deepEqual([...received.bytes], [1, 2]);
});

test("connectTcp forwards the address to the native binding", async () => {
  let seen;
  const mdc = new MyDeviceCanvas({
    devices() {},
    sendFrame() {},
    connectTcp(address) {
      seen = address;
      return Promise.resolve();
    },
  });
  await mdc.connectTcp("127.0.0.1:9");
  assert.equal(seen, "127.0.0.1:9");
});

test("nativeLoaded reflects a bound sendFrame", () => {
  const bare = new MyDeviceCanvas({});
  assert.equal(bare.nativeLoaded(), false);
  const bound = new MyDeviceCanvas({ devices() {}, sendFrame() {} });
  assert.equal(bound.nativeLoaded(), true);
});

test("pollEvents re-emits native input events", async () => {
  const seen = [];
  const mdc = new MyDeviceCanvas({
    pollEvents: async () => [{
      kind: "input",
      deviceId: "dev",
      input: { deviceId: "dev", surfaceId: "main", pointerId: 1, phase: "down", x: 2, y: 3 },
    }],
  });
  mdc.on("input", (ev) => seen.push(ev));
  const events = await mdc.pollEvents();
  assert.equal(events.length, 1);
  assert.equal(seen[0].phase, "down");
  assert.equal(seen[0].x, 2);
});

test("mirror events and controls go through the public SDK", async () => {
  let started;
  const mdc = new MyDeviceCanvas({
    pollEvents: async () => [{
      kind: "mirror",
      deviceId: "dev",
      mirror: { surfaceId: "main", frameId: 3, width: 2, height: 1 },
    }],
    startMirror(surfaceId, mode, maxFps) { started = [surfaceId, mode, maxFps]; return 7; },
    stopMirror() { return 8; },
    takeMirrorFrame: async () => ({
      surfaceId: "main", frameId: 3, width: 2, height: 1, pixels: new Uint8Array(4),
    }),
    mirrorStats: async () => ({ framesReceived: 1, framesDropped: 0, lastFrameId: 3 }),
  });

  const seen = [];
  mdc.onMirrorFrame((frame) => seen.push(frame.frameId));
  await mdc.pollEvents();
  assert.deepEqual(seen, [3]);

  assert.equal(await mdc.startMirror("main", { mode: "dirty-region", maxFps: 15 }), 7);
  assert.deepEqual(started, ["main", "dirty-region", 15]);
  // N559: an empty mode defaults to full-frame, an unknown one is rejected.
  assert.equal(await mdc.startMirror("main", {}), 7);
  assert.deepEqual(started, ["main", "full-frame", 30]);
  assert.throws(() => MyDeviceCanvas.normalizeMirrorMode("weird"), RangeError);
  assert.equal(MyDeviceCanvas.normalizeMirrorMode(undefined), "full-frame");
  assert.equal(MyDeviceCanvas.normalizeMirrorMode("dirty-region"), "dirty-region");
  assert.equal(await mdc.stopMirror("main"), 8);

  const frame = await mdc.takeMirrorFrame();
  assert.equal(frame.frameId, 3);
  assert.equal(frame.pixels.length, 4);
  const stats = await mdc.mirrorStats();
  assert.equal(stats.framesReceived, 1);
});

test("config get/set go through the public SDK", async () => {
  const writes = [];
  const mdc = new MyDeviceCanvas({
    setConfig(key, value) { writes.push([key, value]); return value; },
    getConfig(key) { return key === "wifi_password" ? "***" : "200"; },
  });
  assert.equal(await mdc.setConfig("brightness", 200), "200");
  assert.deepEqual(writes, [["brightness", "200"]]);
  assert.equal(await mdc.getConfig("brightness"), "200");
  assert.equal(await mdc.getConfig("wifi_password"), "***");
});

test("scene set/clear go through the public SDK", async () => {
  const calls = [];
  const mdc = new MyDeviceCanvas({
    setScene(json) { calls.push(["set", JSON.parse(json).kind]); },
    clearScene() { calls.push(["clear"]); },
  });
  await mdc.setScene({ name: "root", kind: "rect" });
  await mdc.clearScene();
  assert.deepEqual(calls, [["set", "rect"], ["clear"]]);
});

test("animation helpers go through the public SDK", async () => {
  const calls = [];
  const mdc = new MyDeviceCanvas({
    setAnimation(clip) { calls.push(clip); return clip; },
    stopAnimation() { calls.push("off"); },
  });
  assert.equal(await mdc.setAnimation("idle"), "idle");
  await mdc.stopAnimation();
  assert.deepEqual(calls, ["idle", "off"]);
});
