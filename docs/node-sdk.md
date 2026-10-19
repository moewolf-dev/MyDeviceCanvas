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

## API

| Method | Notes |
|---|---|
| `devices()` | `DeviceInfo[]`, each with `surface(id)` |
| `surface.pixelFormat` / `surface.implemented` | N307: canonical wire name plus whether the host/frame path implements it (only RGB565 today) |
| `surface.sendFrame(bytes)` / `sendTile(tile)` | RGB565; the caller buffer is copied before dispatch |
| `currentFrameId()` | |
| `pollEvents()` | drained `NativeEvent[]` (`connected` / `disconnected` / `input` / `mirror` / `error`) |
| `on(event, fn)` / `off` | subscription; cancelled by `dispose()` |
| `startMirror(surfaceId, opts)` / `stopMirror` | Form B; needs the device `mirror` capability; N559 validates `opts.mode` |
| `mirrorMode()` | N559: the negotiated mode (`full-frame` \| `dirty-region`) |
| `MyDeviceCanvas.normalizeMirrorMode(mode)` | N559: static; rejects an unknown mode |
| `takeMirrorFrame()` / `onMirrorFrame(fn)` / `mirrorStats()` / `mirrorSkips()` | pixel + stats access |
| `tileReconciliation()` | N576: partial-update `{sent, received, missing, bytesSent, consistent}` |
| `getConfig(key)` / `setConfig(key, value)` | capability-gated |
| `setScene(node)` / `clearScene()` | CBOR scene |
| `setAnimation(clip)` / `stopAnimation()` | baked-atlas clip |
| `installPackage(bytes)` | N234: upload `.mdcpkg` bytes (`Uint8Array`); resolves `true` when ready |
| `packageReady()` | N238: whether the last accepted package is render-ready |
| `connectTcp(address)` | remote peer instead of the in-process simulator |
| `connectWebSocket(url)` | N340: WebSocket peer (`ws://host:port`) |
| `emitMetric(name, value, unit)` / `takeDiagnostics()` / `diagnosticCount()` | N349: simulator diagnostics channel (metrics, boot info, telemetry) |

### installPackage

```js
import fs from "node:fs";
const bytes = fs.readFileSync("rig.mdcpkg"); // Buffer is a Uint8Array
const ready = await mdc.installPackage(bytes);
if (!ready) throw new Error("device rejected the package");
```

The wrapper requires a `Uint8Array` (a `Buffer` qualifies) and forwards a copy.
A malformed package rejects with the device error; `packageReady()` stays false.

## Events

`on("mirror", fn)` carries `{ surfaceId, frameId, width, height }` metadata only
(no pixels); pixels come from `takeMirrorFrame()` / `onMirrorFrame`.

## Non-goals

- No serial transport in the Node binding yet (TCP + in-process simulator; N340 adds WebSocket).
- No native artifact vendored: build per platform (see `sdk/node/package.json`).
- No physical-board claim; D05/G03 remain open.

## Mirror parity (N528)

`emitMirrorFrame()` (sim-only), `takeMirrorFrameBounded(maxBytes)` and
`mirrorSkips()` mirror the C ABI: a frame that does not fit the caller's cap is
dropped and counted instead of being copied out. `takeMirrorFrame()`/`mirrorStats()`
work over any transport, not just the in-process simulator.

## Gestures (N473/N474)

`pollEvents()` also yields `kind: "gesture"` events with
`gesture: { kind: "tap" | "long-press" | "drag", pointerId, x, y, node? }`; the
core derives them from the raw input stream (`GestureTracker`). N493 adds the
hit-tested scene `node`. N488 lets the host tune the thresholds via
`Session::set_gesture_config` (`mdc_session_set_gesture_config` in C). The C ABI
reports the same events as poll kind `8` with the gesture kind in `phase` and the
node in the `node` out-param.
