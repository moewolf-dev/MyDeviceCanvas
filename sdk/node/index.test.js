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
