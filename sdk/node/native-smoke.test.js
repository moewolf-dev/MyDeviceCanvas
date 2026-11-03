import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import net from "node:net";
import fs from "node:fs";
import { createHash } from "node:crypto";
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

test("native compileGltf produces a package the simulator accepts", { skip: !native }, async () => {
  const gltf = {
    asset: { version: "2.0" },
    nodes: [{ name: "root", mesh: 0 }],
    meshes: [{ primitives: [{ attributes: { POSITION: 0 } }] }],
    accessors: [{ count: 4, type: "VEC3" }],
  };
  const mdc = await MyDeviceCanvas.create();
  await mdc.devices();
  const compiled = await mdc.compileGltf(
    new TextEncoder().encode(JSON.stringify(gltf)),
    { target: "sprite" },
  );
  assert.ok(compiled.bytes instanceof Uint8Array);
  assert.ok(compiled.bytes.length > 0);
  assert.equal(compiled.target, "sprite");
  assert.ok(compiled.nodes >= 1);
  assert.ok(compiled.estimatedBytes >= 0);
  const accepted = await mdc.installPackage(compiled.bytes);
  assert.equal(accepted, true);
  assert.equal(await mdc.packageReady(), true);
  await assert.rejects(
    mdc.compileGltf(new Uint8Array([1, 2, 3])),
    /glTF|package/i,
  );
  await assert.rejects(
    mdc.compileGltf(new TextEncoder().encode("{}"), { target: "nope" }),
    /target/,
  );
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

function firmwareRequest(image, overrides = {}) {
  const sha256 = createHash("sha256").update(image).digest("hex");
  return {
    image,
    artifact: {
      boardId: "esp32-jc3248w535-sim",
      mcu: "esp32-s3",
      runtime: "0.1.0",
      protocolMajor: 1,
      protocolMinor: 0,
      imageSize: image.byteLength,
      sha256,
      source: "catalog",
      ...(overrides.artifact ?? {}),
    },
    board: {
      boardId: "esp32-jc3248w535-sim",
      mcu: "esp32-s3",
      flashBytes: 16 * 1024 * 1024,
      profileVerified: true,
      ...(overrides.board ?? {}),
    },
    hostProtocolMajor: 1,
    hostProtocolMinor: 0,
    allowUnknownFlash: overrides.allowUnknownFlash ?? false,
    flashIdUnusable: overrides.flashIdUnusable ?? false,
  };
}

test("native firmware preflight accepts a matching artifact and names rejections", { skip: !native }, async () => {
  const mdc = await MyDeviceCanvas.create();
  const image = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
  const valid = await mdc.firmwarePreflight(firmwareRequest(image));
  assert.equal(valid.verdict, "ok");
  await assert.rejects(
    mdc.firmwarePreflight(
      firmwareRequest(image, { artifact: { sha256: "0".repeat(64) } }),
    ),
    (error) => error.code === "FIRMWARE_PREFLIGHT_REJECTED" && error.reason === "HashMismatch",
  );
  await assert.rejects(
    mdc.firmwarePreflight(
      firmwareRequest(image, { artifact: { boardId: "another-board" } }),
    ),
    (error) => error.reason === "BoardMismatch",
  );
  const unknown = await mdc.firmwarePreflight(
    firmwareRequest(image, {
      board: { flashBytes: null },
      allowUnknownFlash: true,
    }),
  );
  assert.equal(unknown.verdict, "okUnknownFlash");
  mdc.dispose();
});

test("native firmware sim install reports progress and rejects a readback mismatch", { skip: !native }, async () => {
  const mdc = await MyDeviceCanvas.create();
  const image = new Uint8Array(64);
  const base = {
    preflight: firmwareRequest(image),
    port: "sim://unit-1",
    expectedDeviceId: "sim-esp32-jc3248w535",
    mode: "sim",
  };
  const pending = mdc.firmwareInstall({ ...base, simDelayMs: 200 });
  await new Promise((resolve) => setTimeout(resolve, 60));
  const during = await mdc.firmwareProgress();
  assert.equal(during.phase, "writing");
  const done = await pending;
  assert.equal(done.deviceId, "sim-esp32-jc3248w535");
  assert.equal((await mdc.firmwareProgress()).phase, "done");

  await assert.rejects(
    mdc.firmwareInstall({ ...base, simReadbackDeviceId: "someone-else" }),
    (error) =>
      error.code === "FIRMWARE_INSTALL_FAILED" &&
      error.reason === "IdentityMismatch",
  );
  mdc.dispose();
});

test("native firmware lease blocks a second install and cancel settles", { skip: !native }, async () => {
  const mdc = await MyDeviceCanvas.create();
  const image = new Uint8Array(64);
  const request = {
    preflight: firmwareRequest(image),
    port: "sim://unit-2",
    expectedDeviceId: "sim-esp32-jc3248w535",
    mode: "sim",
    simDelayMs: 300,
  };
  const first = mdc.firmwareInstall(request);
  await new Promise((resolve) => setTimeout(resolve, 60));
  await assert.rejects(
    mdc.firmwareInstall({ ...request, simDelayMs: 0 }),
    (error) => error.reason === "PortBusy",
  );
  const completed = await first;
  assert.equal(completed.phase, "done", "the lease holder finishes normally");

  const cancellable = mdc.firmwareInstall(request);
  await new Promise((resolve) => setTimeout(resolve, 60));
  await mdc.cancelFirmware();
  await assert.rejects(
    cancellable,
    (error) => error.phase === "cancelled" && error.reason === "Cancelled",
  );
  assert.equal((await mdc.firmwareProgress()).phase, "cancelled");
  mdc.dispose();
});

test("native firmware readback performs a real HELLO over a PTY", {
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
    let readback = null;
    let last = "not attempted";
    for (let i = 0; i < 40 && !readback; i += 1) {
      try {
        readback = await mdc.firmwareReadback({ port: ptyPath, timeoutMs: 2000 });
      } catch (error) {
        last = error instanceof Error ? error.message : String(error);
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
    }
    assert.ok(readback, last);
    assert.equal(readback.deviceId, "sim-esp32-jc3248w535");
    // The simulator peer advertises the same id for board and device; the
    // contract only requires a non-empty advertised board id.
    assert.ok(readback.boardId.length > 0);
    assert.equal(typeof readback.protocolVersion, "number");
    assert.ok(readback.firmware.length > 0);

    // An open session holds the process lease, so readback must refuse.
    await mdc.connectSerial(ptyPath, 115200);
    await assert.rejects(
      mdc.firmwareReadback({ port: ptyPath, timeoutMs: 2000 }),
      /leased/,
    );
  } finally {
    mdc.dispose();
    bridge.kill();
    peer.kill();
  }
});

test("native disconnect closes a PTY session and keeps the manager usable", {
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
    assert.equal((await mdc.devices()).length, 1);
    await mdc.disconnect();
    await mdc.disconnect(); // idempotent
    await assert.rejects(mdc.devices(), /no active connection/);
    // An explicit simulator request is still allowed after a disconnect.
    await mdc.openBoard("linux-virt");
    assert.equal((await mdc.devices())[0].info.deviceId, "sim-linux-virt");
  } finally {
    mdc.dispose();
    bridge.kill();
    peer.kill();
  }
});

test("native boardFacts reports known and unknown boards without guessing", { skip: !native }, async () => {
  const mdc = await MyDeviceCanvas.create();
  const sim = await mdc.boardFacts("esp32-jc3248w535-sim");
  assert.equal(sim.known, true);
  assert.equal(sim.mcu, "esp32-s3");
  assert.equal(sim.flashBytes, 16 * 1024 * 1024);
  assert.equal(sim.profileVerified, true);

  const reference = await mdc.boardFacts("jc3248w535");
  assert.equal(reference.known, true);
  assert.equal(reference.profileVerified, false, "physical board stays unverified");

  const unknown = await mdc.boardFacts("not-a-board");
  assert.equal(unknown.known, false);
  assert.equal(unknown.profileVerified, false);
  assert.equal(unknown.mcu, "");
  assert.equal(unknown.flashBytes ?? null, null);
  await assert.rejects(mdc.boardFacts("  "), /non-empty/);
  mdc.dispose();
});
