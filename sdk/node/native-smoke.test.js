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

test("native tile reconciliation keeps its invariants", { skip: !native }, async () => {
  // N648: `missing` and `consistent` agree with `sent`/`received`, matching the
  // C `mdc_session_tile_reconciliation_ex` fields the other SDKs expose.
  const mdc = await MyDeviceCanvas.create();
  const report = await mdc.tileReconciliation();
  assert.ok(report, "a reconciliation report");
  assert.equal(report.missing, report.sent - report.received);
  assert.equal(report.consistent, report.sent === report.received);
  assert.equal(report.sent, 0, "a fresh simulator sent no tiles");
  assert.equal(report.bytesSent, 0);
  assert.equal(report.missing, 0);
  mdc.dispose();
});

test("native bounded mirror take drops an oversized frame", { skip: !native }, async () => {
  // N528: `takeMirrorFrameBounded` returns the frame when it fits the cap and
  // drops (counting) it otherwise.
  const mdc = await MyDeviceCanvas.create();
  await mdc.openBoard("linux-virt");
  assert.equal(await mdc.mirrorSkips(), 0);
  // N559: the mode is validated and the negotiated mode is readable.
  await mdc.startMirror("main", { mode: "dirty-region" });
  assert.equal(await mdc.mirrorMode(), "dirty-region");
  await assert.rejects(mdc.startMirror("main", { mode: "nope" }), /mirror mode/);
  await mdc.startMirror("main");
  await mdc.emitMirrorFrame();
  // A tiny cap cannot hold the 800x480 RGB565 frame.
  assert.equal(await mdc.takeMirrorFrameBounded(16), null);
  assert.equal(await mdc.mirrorSkips(), 1);
  // A generous cap takes the next frame.
  await mdc.emitMirrorFrame();
  const frame = await mdc.takeMirrorFrameBounded(800 * 480 * 2);
  assert.ok(frame, "a frame within the cap is returned");
  assert.equal(frame.width, 800);
  assert.equal(frame.height, 480);
  assert.equal(frame.surfaceId, "main");
  mdc.dispose();
});

test("native mirror mode and tile reconciliation are exposed", { skip: !native }, async () => {
  // N588: the real .node binding reports the negotiated mode and the tile
  // reconciliation snapshot end to end.
  const mdc = await MyDeviceCanvas.create();
  await mdc.openBoard("linux-virt");
  await mdc.startMirror("main", { mode: "dirty-region", maxFps: 15 });
  assert.equal(await mdc.mirrorMode(), "dirty-region");
  const report = await mdc.tileReconciliation();
  assert.ok(report, "a reconciliation snapshot");
  // N608: the same field set the C smoke asserts.
  assert.equal(report.sent, 0, "no partial updates were sent");
  assert.equal(report.received, 0);
  assert.equal(report.missing, 0);
  assert.equal(report.bytesSent, 0);
  assert.equal(report.consistent, true);
  // N678: the same invariants the C/Swift SDKs promise.
  assert.equal(report.missing, Math.max(0, report.sent - report.received));
  assert.equal(report.consistent, report.missing === 0);
  assert.ok(report.bytesSent >= 0);
  await mdc.stopMirror("main");
  assert.equal(await mdc.mirrorMode(), "");
  mdc.dispose();
});

test("native installPackage rejects a malformed package", { skip: !native }, async () => {
  const mdc = await MyDeviceCanvas.create();
  await assert.rejects(mdc.installPackage(new Uint8Array([1, 2, 3])));
  assert.equal(await mdc.packageReady(), false);
  mdc.dispose();
});

test("native connectTcp sends an RGB565 frame to mdc-sim-peer", {
  skip: !native || !fs.existsSync(peerBin),
}, async () => {
  const port = await reservePort();
  const address = `127.0.0.1:${port}`;
  const peer = spawn(peerBin, [address], { stdio: "ignore" });
  const mdc = await MyDeviceCanvas.create();
  try {
    // Queue one event on the memory link; replacing the endpoint must drop it.
    await mdc.devices();
    await mdc._native.simulateInput("main", 77, "down", 1, 1);
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
    // SIM-R04: events from the replaced memory link are gone, and the new
    // generation is stamped on every delivered event.
    const events = await mdc.pollEvents();
    assert.ok(events.every((event) => event.generation >= 1));
    assert.ok(
      !events.some((event) => event.input?.pointerId === 77),
      "old-generation input survived the endpoint switch",
    );
    mdc.dispose();
    await assert.rejects(mdc.devices(), /disposed/);
  } finally {
    peer.kill();
  }
});

test("native TCP peer exit surfaces a transport error", {
  skip: !native || !fs.existsSync(peerBin),
}, async () => {
  const port = await reservePort();
  const address = `127.0.0.1:${port}`;
  const peer = spawn(peerBin, [address, "--exit-after-ms", "900"], { stdio: "ignore" });
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
    await new Promise((resolve) => setTimeout(resolve, 1200));
    let failure = null;
    for (let i = 0; i < 30; i += 1) {
      try {
        await mdc.pollEvents();
      } catch (error) {
        failure = error;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.ok(failure, "peer exit must surface as a typed error");
    assert.match(String(failure.message), /closed|transport|timeout/i);
    // No stale FakeDevice may replace the dead TCP session.
    await assert.rejects(mdc.devices(), /no active connection/);
  } finally {
    peer.kill();
    mdc.dispose();
  }
});

test("native connectSerial reaches a peer through a real PTY", {
  skip: !native || !fs.existsSync(peerBin) || process.platform === "win32",
}, async () => {
  const port = await reservePort();
  const address = `127.0.0.1:${port}`;
  const peer = spawn(peerBin, [address], { stdio: "ignore" });
  const bridge = spawn("python3", [path.join(repoRoot, "sdk/node/pty_bridge.py"), address], {
    stdio: ["ignore", "pipe", "ignore"],
  });
  const mdc = await MyDeviceCanvas.create();
  try {
    const ptyPath = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("pty bridge timeout")), 5000);
      bridge.stdout.once("data", (chunk) => {
        clearTimeout(timer);
        resolve(String(chunk).trim().split("\n")[0]);
      });
      bridge.once("error", reject);
    });
    let connected = false;
    let last = "not attempted";
    for (let i = 0; i < 40; i += 1) {
      try {
        await mdc.connectSerial(ptyPath, 115200);
        connected = true;
        break;
      } catch (error) {
        last = error instanceof Error ? error.message : String(error);
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
    }
    assert.equal(connected, true, last);
    const devices = await mdc.devices();
    assert.equal(devices.length, 1);
    assert.equal(devices[0].info.deviceId, "sim-esp32-jc3248w535");
  } finally {
    bridge.kill();
    peer.kill();
    mdc.dispose();
  }
});

test("native serial PTY peer exit surfaces a transport error", {
  skip: !native || !fs.existsSync(peerBin) || process.platform === "win32",
}, async () => {
  const port = await reservePort();
  const address = `127.0.0.1:${port}`;
  const peer = spawn(peerBin, [address, "--exit-after-ms", "1500"], { stdio: "ignore" });
  const bridge = spawn("python3", [path.join(repoRoot, "sdk/node/pty_bridge.py"), address], {
    stdio: ["ignore", "pipe", "ignore"],
  });
  const mdc = await MyDeviceCanvas.create();
  try {
    const ptyPath = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("pty bridge timeout")), 5000);
      bridge.stdout.once("data", (chunk) => {
        clearTimeout(timer);
        resolve(String(chunk).trim().split("\n")[0]);
      });
      bridge.once("error", reject);
    });
    let connected = false;
    let last = "not attempted";
    for (let i = 0; i < 40; i += 1) {
      try {
        await mdc.connectSerial(ptyPath, 115200);
        connected = true;
        break;
      } catch (error) {
        last = error instanceof Error ? error.message : String(error);
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
    }
    assert.equal(connected, true, last);
    await new Promise((resolve) => setTimeout(resolve, 1800));
    let failure = null;
    for (let i = 0; i < 30; i += 1) {
      try {
        await mdc.pollEvents();
      } catch (error) {
        failure = error;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.ok(failure, "PTY peer close must surface as a typed error");
    await assert.rejects(mdc.devices(), /no active connection/);
  } finally {
    bridge.kill();
    peer.kill();
    mdc.dispose();
  }
});

test("native serial PTY corrupt ACK is rejected", {
  skip: !native || !fs.existsSync(peerBin) || process.platform === "win32",
}, async () => {
  const port = await reservePort();
  const address = `127.0.0.1:${port}`;
  const peer = spawn(peerBin, [address, "--corrupt-after", "1"], { stdio: "ignore" });
  const bridge = spawn("python3", [path.join(repoRoot, "sdk/node/pty_bridge.py"), address], {
    stdio: ["ignore", "pipe", "ignore"],
  });
  const mdc = await MyDeviceCanvas.create();
  try {
    const ptyPath = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("pty bridge timeout")), 5000);
      bridge.stdout.once("data", (chunk) => {
        clearTimeout(timer);
        resolve(String(chunk).trim().split("\n")[0]);
      });
      bridge.once("error", reject);
    });
    let connected = false;
    let last = "not attempted";
    for (let i = 0; i < 40; i += 1) {
      try {
        await mdc.connectSerial(ptyPath, 115200);
        connected = true;
        break;
      } catch (error) {
        last = error instanceof Error ? error.message : String(error);
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
    }
    assert.equal(connected, true, last);
    const devices = await mdc.devices();
    const surface = devices[0].surface("main");
    const bytes = new Uint8Array(surface.info.width * surface.info.height * 2);
    let failure = null;
    try {
      await surface.sendFrame(bytes);
    } catch (error) {
      failure = error;
    }
    for (let i = 0; i < 30 && !failure; i += 1) {
      try {
        await mdc.pollEvents();
      } catch (error) {
        failure = error;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.ok(failure, "corrupt PTY ACK must be rejected");
    assert.match(String(failure.message), /protocol|checksum|invalid/i);
  } finally {
    bridge.kill();
    peer.kill();
    mdc.dispose();
  }
});

test("native connectWebSocket sends an RGB565 frame to mdc-sim-peer", {
  skip: !native || !fs.existsSync(peerBin),
}, async () => {
  const port = await reservePort();
  const address = `127.0.0.1:${port}`;
  const peer = spawn(peerBin, [address, "esp32-jc3248w535-sim", "--ws"], { stdio: "ignore" });
  const mdc = await MyDeviceCanvas.create();
  try {
    let connected = false;
    let last = "not attempted";
    for (let i = 0; i < 40; i += 1) {
      try {
        await mdc.connectWebSocket(`ws://${address}`);
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

test("native diagnostics round trip", { skip: !native }, async () => {
  const mdc = await MyDeviceCanvas.create();
  await mdc.emitMetric("fps", 29.5, "fps");
  assert.equal(await mdc.diagnosticCount(), 1);
  const diagnostics = await mdc.takeDiagnostics();
  assert.equal(diagnostics.length, 1);
  assert.equal(diagnostics[0].kind, "metric");
  assert.equal(diagnostics[0].name, "fps");
  assert.equal(diagnostics[0].unit, "fps");
  assert.equal(await mdc.diagnosticCount(), 0);
  mdc.dispose();
  await assert.rejects(mdc.takeDiagnostics(), /disposed/);
});

test("native silent peer does not block the JS event loop and cancels promptly", { skip: !native }, async () => {
  // SIM-R05: the handshake runs on the libuv pool; a silent peer must not
  // starve timers, and dispose must settle the in-flight connect quickly.
  const server = net.createServer(() => {
    // Accept and stay silent: no HELLO/Capabilities reply.
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  const mdc = await MyDeviceCanvas.create();
  let maxGap = 0;
  let last = performance.now();
  const timer = setInterval(() => {
    const now = performance.now();
    maxGap = Math.max(maxGap, now - last);
    last = now;
  }, 10);
  const started = Date.now();
  try {
    const pending = mdc
      .connectTcp(`127.0.0.1:${port}`)
      .then(() => null, (error) => error);
    await new Promise((resolve) => setTimeout(resolve, 100));
    mdc.dispose();
    const error = await pending;
    const settleMs = Date.now() - started;
    assert.ok(error instanceof Error, "connect rejects when cancelled");
    assert.match(error.message, /cancel|HELLO|closed|timed out/i);
    assert.ok(maxGap < 100, `event loop gap ${maxGap.toFixed(1)}ms`);
    assert.ok(settleMs < 500, `cancel settled in ${settleMs}ms`);
  } finally {
    clearInterval(timer);
    server.close();
  }
});

test("native dual-sim lists overlay and disconnect cancel keeps the pointer", { skip: !native }, async () => {
  const mdc = await MyDeviceCanvas.create();
  const devices = await mdc.openBoard("dual-sim");
  assert.equal(devices[0].info.deviceId, "sim-dual");
  assert.equal(devices[0].surface("main").info.width, 8);
  assert.equal(devices[0].surface("main").info.height, 4);
  assert.equal(devices[0].surface("overlay").info.width, 4);
  assert.equal(devices[0].surface("overlay").info.height, 2);
  mdc.dispose();

  const touch = await MyDeviceCanvas.create();
  const ready = await touch.devices();
  assert.equal(ready[0].surface("main").info.width, 480);
  await touch._native.simulateInput("main", 3, "down", 6, 7);
  await touch.simulateDisconnect();
  const events = await touch.pollEvents();
  const cancel = events.find((event) => event.input?.phase === "cancel");
  assert.ok(cancel);
  assert.equal(cancel.input.pointerId, 3);
  assert.equal(cancel.input.x, 6);
  assert.equal(cancel.input.y, 7);
  touch.dispose();

  // N430: the host can also send a pointer event to the device.
  const hostInput = await MyDeviceCanvas.create();
  await hostInput.devices();
  await hostInput._native.sendInput("main", 9, "down", 12, 13);
  hostInput.dispose();

  // N455: host session counters are readable from Node.
  const statsSession = await MyDeviceCanvas.create();
  await statsSession.devices();
  const stats = await statsSession._native.stats();
  assert.ok(stats.packetsSent >= 0);
  assert.ok(stats.bytesSent >= 0);
  statsSession.dispose();

  // N430/APP-R03: the public SDK rejects an unusable path cleanly.
  const serial = await MyDeviceCanvas.create();
  await assert.rejects(serial.connectSerial("   "), /non-empty/);
  await assert.rejects(serial.connectSerial("/no/such/port", 115200));
  // SIM-R05: a failed real endpoint must not be replaced by a FakeDevice.
  await assert.rejects(serial.devices(), /no active connection/);
  serial.dispose();
});
