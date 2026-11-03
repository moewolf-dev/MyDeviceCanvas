# Node SDK

Status: **sim-verified** (host logic); the native binding is built per platform
and is **not** installed by default (`loadNative()` returns `null` until a
`mdc-node-native.node` artifact is present).

`@mdc/node` (`sdk/node/`) wraps the Rust Core over N-API. It never reimplements
Core; the JS layer validates inputs, copies caller buffers, and maps native
methods to promises.

## Loading

```js
import { MyDeviceCanvas } from "@mdc/node";
const mdc = await MyDeviceCanvas.create();
mdc.nativeLoaded(); // false when the .node artifact is missing
```

`MyDeviceCanvas.create()` opens the simulator board by default. `mdc.dispose()`
is idempotent and cancels listeners; every method rejects after dispose.

## Async execution

Every public method returns a Promise. The native binding runs the blocking
part (TCP/WebSocket/serial connect, HELLO, frame pump, package upload) on the
libuv worker pool, so JS timers and rendering keep running while a peer is slow
or silent. A silent peer is rejected after a 2 s handshake budget.

`dispose()` never waits for a worker: it sets shared cancellation flags, and an
in-flight handshake stops on its next read poll (transport reads use a 400 ms
timeout). Callers should still `await` the connect Promise to observe the typed
cancellation error.

Once a TCP/WebSocket/serial endpoint has been requested, a failed HELLO leaves
the manager without a session. Later calls report `no active connection` instead
of silently opening an in-process FakeDevice; only a never-connected manager
defaults to the simulator.

The native session owns a real-clock tick: ACK and heartbeat deadlines are
checked on every operation, and reconnect is disabled for SDK sessions. A peer
that exits therefore surfaces a terminal transport error and clears its device;
`devices()` then rejects with `no active connection` instead of returning stale
capabilities.

## API

| Method | Notes |
|---|---|
| `devices()` | `DeviceInfo[]`, each with `surface(id)` |
| `surface.pixelFormat` / `surface.implemented` | N307: canonical wire name plus whether the host/frame path implements it (only RGB565 today) |
| `surface.sendFrame(bytes)` / `sendTile(tile)` | RGB565; the caller buffer is copied before dispatch |
| `currentFrameId()` | |
| `pollEvents()` | drained `NativeEvent[]` (`connected` / `disconnected` / `input` / `mirror` / `error`); each event has a `generation`, and events from a replaced link are discarded |
| `on(event, fn)` / `off` | subscription; cancelled by `dispose()` |
| `startMirror(surfaceId, opts)` / `stopMirror` | Form B; needs the device `mirror` capability; N559 validates `opts.mode` |
| `mirrorMode()` | N559: the negotiated mode (`full-frame` \| `dirty-region`) |
| `MyDeviceCanvas.normalizeMirrorMode(mode)` | N559: static; rejects an unknown mode |
| `takeMirrorFrame()` / `onMirrorFrame(fn)` / `mirrorStats()` / `mirrorSkips()` | pixel + stats access |
| `tileReconciliation()` | N576/N678: partial-update `{sent, received, missing, bytesSent, consistent}`; `missing == max(0, sent-received)` and `consistent == (missing == 0)` |
| `getConfig(key)` / `setConfig(key, value)` | capability-gated |
| `setScene(node)` / `clearScene()` | CBOR scene |
| `setAnimation(clip)` / `stopAnimation()` | baked-atlas clip |
| `setAnimationBlend("idle:700,wave:300")` / `animationBlend()` | weighted atlas or mesh clip blend (N837) |
| `compileGltf(bytes, { target })` | C08: compile an embedded glTF JSON document on the host (`sprite` or `mesh3d`); returns `.mdcpkg` bytes plus node/mesh/animation counts for `installPackage` |
| `installPackage(bytes)` | N234: upload `.mdcpkg` bytes (`Uint8Array`); resolves `true` when ready |
| `packageReady()` | N238: whether the last accepted package is render-ready |
| `boardFacts(boardId)` | CORE-R06: host-side MCU/flash/verified facts; `known: false` must not be guessed |
| `firmwarePreflight(request)` | CORE-R06: board/MCU/protocol/size/hash/signature verdict before any port opens |
| `firmwareInstall(request)` | CORE-R06: preflight + exclusive port lease + write + reset + identity readback; `sim` or `esptool` (dry run by default) |
| `firmwareProgress()` / `cancelFirmware()` | CORE-R06: polled phase/ratio/error and next-boundary cancellation |
| `firmwareReadback({ port, timeoutMs?, baud? })` | CORE-R06: real HELLO identity/version on the leased port |
| `connectTcp(address)` | remote peer instead of the in-process simulator |
| `connectSerial(path, baud = 115200, resetOnOpen = false)` | N430/APP-R03: open a named serial port and perform HELLO; path is trimmed, baud 0 uses the native default, `resetOnOpen` applies DTR/RTS before HELLO |
| `capabilities()` | APP-R03: negotiated frame/tile/touch/OTA limits, mirror modes/FPS, config keys and scene limits |
| `serialPorts()` | APP-R03: enumerate host serial endpoint hints; identity still comes from HELLO |
| `connectWebSocket(url)` | N340: WebSocket peer (`ws://host:port`) |
| `disconnect()` | CORE-R02: close the session and release the serial/process lease without disposing; later calls report `no active connection` until an explicit connect/openBoard |
| `emitMetric(name, value, unit)` / `takeDiagnostics()` / `diagnosticCount()` | N349: simulator diagnostics channel (metrics, boot info, telemetry) |

### compileGltf

```js
const gltf = new TextEncoder().encode(JSON.stringify(document))
const compiled = await mdc.compileGltf(gltf, { target: "sprite" })
await mdc.installPackage(compiled.bytes)
```

Compilation is host-side; it does not need a connected device. Only embedded
glTF documents are accepted in this batch. Invalid JSON, malformed meshes and an
unknown target reject with a typed error.

### installPackage

```js
import fs from "node:fs";
const bytes = fs.readFileSync("rig.mdcpkg"); // Buffer is a Uint8Array
const ready = await mdc.installPackage(bytes);
if (!ready) throw new Error("device rejected the package");
```

The wrapper requires a `Uint8Array` (a `Buffer` qualifies) and forwards a copy.
A malformed package rejects with the device error; `packageReady()` stays false.

### Firmware install (CORE-R06)

```js
import fs from "node:fs";

const image = fs.readFileSync("firmware.bin");
const preflight = {
  image,
  artifact: {
    boardId: "esp32-jc3248w535-sim", mcu: "esp32-s3", runtime: "0.1.0",
    protocolMajor: 1, protocolMinor: 0, imageSize: image.length,
    sha256: "<64 hex>", source: "catalog",
  },
  board: {
    boardId: "esp32-jc3248w535-sim", mcu: "esp32-s3",
    flashBytes: 16 * 1024 * 1024, profileVerified: true,
  },
  hostProtocolMajor: 1, hostProtocolMinor: 0,
};

await mdc.firmwarePreflight(preflight);          // throws FIRMWARE_PREFLIGHT_REJECTED
const pending = mdc.firmwareInstall({
  preflight, port: "sim://device-1",
  expectedDeviceId: "sim-esp32-jc3248w535", mode: "sim", simDelayMs: 200,
});
await mdc.firmwareProgress();                    // { phase: "writing", ratio, error }
const done = await pending;                      // { ok: true, phase: "done", deviceId }
// await mdc.cancelFirmware() instead makes `pending` reject at the next boundary
// with FIRMWARE_INSTALL_FAILED and phase "cancelled".

const readback = await mdc.firmwareReadback({ port: "/dev/cu.usbmodem..." });
// { deviceId, boardId, protocolVersion, firmware } — from a real HELLO
```

- Preflight never opens a port and checks board/MCU/protocol major, image
  size/capacity, SHA-256 and an optional artifact signature. `flashBytes: null`
  stays `Unknown` and is rejected unless `allowUnknownFlash` is set.
- The install job holds a process-wide exclusive lease for the whole flow.
  `mode: "sim"` never opens the port and requires a `sim://` port; `mode:
  "esptool"` writes the bytes to a bounded temp file and defaults to a dry run
  (`dryRun: true`). Live esptool writes, rollback and a real board remain HIL.
- Progress is polled (`firmwareProgress()`), not pushed. `cancelFirmware()` sets
  a flag checked at each stage boundary; a live esptool process is not
  preempted mid-write.
- `firmwareReadback` takes the same lease, performs a real HELLO on the serial
  port, and returns the advertised identity/version. An open `connectSerial`
  session holds that lease, so install/readback refuse a busy port and the
  reverse is also true.
- `simReadbackDeviceId` is a simulator-only fault hook: the fake flasher reads
  back a different id so the `IdentityMismatch` path can be tested without
  hardware.

## Events

`on("mirror", fn)` carries `{ surfaceId, frameId, width, height }` metadata only
(no pixels); pixels come from `takeMirrorFrame()` / `onMirrorFrame`.

## Non-goals

- No native artifact vendored: build per platform (see `sdk/node/package.json`).
- BLE, native USB bulk and PCIe transports are not implemented.
- Live firmware flashing/rollback is not verified on hardware; `esptool` installs
  default to a dry run and `D05/G03/H06` remain open.

## Mirror parity (N528)

`emitMirrorFrame()` (sim-only), `takeMirrorFrameBounded(maxBytes)` and
`mirrorSkips()` mirror the C ABI: a frame that does not fit the caller's cap is
dropped and counted instead of being copied out. `takeMirrorFrame()`/`mirrorStats()`
work over any transport, not just the in-process simulator.

## Tile reconciliation (N648)

`tileReconciliation()` returns `{ sent, received, missing, bytesSent, consistent }`
for partial (tile) updates. The smoke test pins the invariants
`missing === sent - received` and `consistent === (sent === received)`, matching
the C ABI's `mdc_session_tile_reconciliation_ex` and the Swift wrapper.

## Gestures (N473/N474)

`pollEvents()` also yields `kind: "gesture"` events with
`gesture: { kind: "tap" | "long-press" | "drag" | "pinch" | "pan" | "swipe" | "double-tap" | "triple-tap", pointerId, x, y, node?, scale, rotation, dx, dy }`; the
core derives them from the raw input stream (`GestureTracker`). N493 adds the
hit-tested scene `node`. N488 lets the host tune the thresholds via
`Session::set_gesture_config` (`mdc_session_set_gesture_config` in C). N784:
`setMultiTapRadius(radius)` / `multiTapRadius()` (C:
`mdc_session_set_multi_tap_radius`, a `uint32_t` so values above 4096 return
-2) round-trip the multi-tap radius; the 3-argument setter leaves the current
radius in place. N794: `setMultiTapMs(ms)` / `multiTapMs()` (C:
`mdc_session_set_multi_tap_ms`, a `uint32_t`) round-trip the multi-tap window.
`0`..=`60000` is accepted; a larger value throws in JavaScript and returns -2
from C. The 3-argument setter leaves the current window in place. N804: that
stored window is what splits a double-tap (second tap inside `multi_tap_ms`)
from two separate taps (second tap outside it). The C ABI
reports the same events as poll kind `8` with the gesture kind in `phase` and the
node in the `node` out-param.
