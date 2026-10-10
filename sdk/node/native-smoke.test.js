import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { MyDeviceCanvas } from "./index.js";

const native = (() => {
  try {
    return createRequire(import.meta.url)("./mdc-node-native.node");
  } catch {
    return null;
  }
})();

test("native simulator session lists a device and accepts an RGB565 frame", { skip: !native }, async () => {
  const mdc = await MyDeviceCanvas.create();
  assert.equal(mdc.nativeLoaded(), true);
  const devices = await mdc.devices();
  assert.equal(devices.length, 1);
  const surface = devices[0].surface("main");
  assert.ok(surface);
  const bytes = new Uint8Array(surface.info.width * surface.info.height * 2);
  bytes[0] = 0x00;
  bytes[1] = 0xf8;
  const request = await surface.sendFrame(bytes);
  assert.equal(typeof request, "number");
  mdc.dispose();
  await assert.rejects(mdc.devices(), /disposed/);
});
