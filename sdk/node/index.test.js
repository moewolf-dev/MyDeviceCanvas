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
