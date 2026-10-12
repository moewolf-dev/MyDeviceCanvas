import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import net from "node:net";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { MyDeviceCanvas } from "./index.js";

const native = (() => {
  try {
    return createRequire(import.meta.url)("./mdc-node-native.node");
  } catch {
    return null;
  }
})();

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const peerBin = path.join(repoRoot, "target/debug/mdc-sim-peer");

function reservePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      server.close((error) => (error ? reject(error) : resolve(port)));
    });
  });
}

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

test("native connectTcp sends an RGB565 frame to mdc-sim-peer", {
  skip: !native || !fs.existsSync(peerBin),
}, async () => {
  const port = await reservePort();
  const address = `127.0.0.1:${port}`;
  const peer = spawn(peerBin, [address], { stdio: "ignore" });
  const mdc = await MyDeviceCanvas.create();
  try {
    let connected = false;
    let last = "not attempted";
    for (let i = 0; i < 40; i += 1) {
      try {
        await mdc.connectTcp(address);
        connected = true;
        break;
      } catch (error) {
        last = error instanceof Error ? error.message : String(error);
        await new Promise((resolve) => setTimeout(resolve, 25));
      }
    }
    assert.equal(connected, true, last);
    const devices = await mdc.devices();
    assert.equal(devices.length, 1);
    const surface = devices[0].surface("main");
    const bytes = new Uint8Array(surface.info.width * surface.info.height * 2);
    bytes[0] = 0x00;
    bytes[1] = 0xf8;
    const request = await surface.sendFrame(bytes);
    assert.equal(typeof request, "number");
    mdc.dispose();
    await assert.rejects(mdc.devices(), /disposed/);
  } finally {
    peer.kill();
  }
});
