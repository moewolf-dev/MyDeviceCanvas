# Node SDK interface catalog

Status: living document. It maps every public method to its layer, native
handler, error contract and tests. Update it in the same commit that adds or
changes an interface. The goal is that a caller can find the contract and a
tester can find the smallest test that proves it.

## Layers

```
protocol/vectors ──> crates/mdc-protocol ──> crates/mdc-core ──┐
                                                              ├─> sdk/node/native (N-API DTO + async task)
transport (serial/TCP/WS) ────────────────────────────────────┘         │
                                                                        v
                                                          sdk/node/index.js (validation, copies)
                                                                        │
                                                                        v
                                                  App adapters/core (sdk/connection/firmware/errors)
                                                                        │
                                                                        v
                                                          features/* (business)
```

Rules:

- `sdk/node/native` owns bytes, sessions, clocks and bounded containers.
- `sdk/node/index.js` validates JS input, copies buffers and maps native errors;
  it never implements protocol behavior.
- App adapters depend on the narrow ports in `adapters/core/types.ts`
  (`CoreConnectionFactory`, `CoreConnection`, `CoreFirmwarePort`), not the native
  class.
- Business features never parse protocol bytes, open serial ports or compose
  esptool commands.

## Method catalog

### Lifecycle / discovery

| Method | Native handler | Contract / errors | Primary tests |
|---|---|---|---|
| `MyDeviceCanvas.create()` | constructor of `NativeManager` | resolves even without `.node`; `nativeLoaded()` tells the truth | `index.test.js` optional-native case |
| `nativeLoaded()` | — | false when the artifact is absent | `index.test.js` |
| `openBoard(boardId)` | `open_board_sync` | explicit simulator reopen; rejects if a session is open | `native-smoke.test.js` |
| `connectTcp(address)` | `connect_tcp_sync` | HELLO budget; returns after Ready | `faults.rs`, `native-smoke.test.js` |
| `connectSerial(path, baud, resetOnOpen)` | `connect_serial_sync` | path trimmed; baud 0=115200; lease checked | `native-smoke.test.js` PTY |
| `connectWebSocket(url)` | `connect_web_socket_sync` | WS handshake + HELLO | `faults.rs` |
| `disconnect()` | `disconnect_sync` | idempotent; releases the serial lease; later calls report `no active connection` | `native-smoke.test.js` PTY |
| `devices()` | `devices_sync` | one DTO per device; rejects after explicit disconnect/failure | `device-manager.test.ts`, native smoke |
| `capabilities()` | `capabilities_sync` | negotiated caps; rejects without a session | `native-smoke.test.js` |
| `serialPorts()` | `serial_ports_sync` | endpoint hints only; identity via HELLO | `serial-ports.test.ts` |
| `dispose()` | `dispose` | idempotent; cancels workers; no events after | `index.test.js`, native smoke |

### Frame / tile

| Method | Native handler | Contract / errors | Primary tests |
|---|---|---|---|
| `surface.sendFrame(bytes)` | `send_frame_sync` | RGB565 length checked; buffer copied; 1 in-flight + 1 pending | `frame.test.ts`, simulator/fault tests |
| `surface.sendTile(tile)` | `send_tile_sync` | base frame must match; length checked | `frame.test.ts`, `cxx_peer.rs` |
| `currentFrameId()` | `current_frame_id_sync` | peer-reported id for Tile base | `native-smoke.test.js` |
| `tileReconciliation()` | `tile_reconciliation_sync` | `{sent, received, missing, bytesSent, consistent}` invariants | `native-smoke.test.js` |

### Mirror (Form B)

| Method | Native handler | Contract / errors | Primary tests |
|---|---|---|---|
| `startMirror(surfaceId, opts)` | `start_mirror_sync` | needs `mirror` capability; mode validated | native mirror/PTY tests |
| `mirrorMode()` | `mirror_mode_sync` | `""` before negotiation | native mirror tests |
| `stopMirror(surfaceId)` | `stop_mirror_sync` | idempotent | native mirror tests |
| `emitMirrorFrame()` | `emit_mirror_frame_sync` | simulator stimulus only | `native-mirror.test.ts` |
| `takeMirrorFrame()` / `takeMirrorFrameBounded(max)` | `take_mirror_frame*_sync` | bounded take drops and counts | `native-smoke.test.js` |
| `mirrorSkips()` / `mirrorStats()` | `mirror_skips_sync` / `mirror_stats_sync` | counters only | native smoke |
| `onMirrorFrame(fn)` | — (JS subscription over `pollEvents`) | returns an unsubscribe function | `index.test.js` |
| `normalizeMirrorMode(mode)` | — (static validator) | `full-frame` \| `dirty-region`; rejects unknown | `index.test.js` |

### Package / model

| Method | Native handler | Contract / errors | Primary tests |
|---|---|---|---|
| `compileGltf(bytes, {target})` | `compile_gltf_sync` | embedded glTF; `sprite`/`mesh3d`; typed errors | native `compileGltf` test |
| `installPackage(bytes)` | `install_package_sync` | `.mdcpkg` magic/size/hash; ready after install | `package-install.test.ts` |
| `packageReady()` | `package_ready_sync` | false before/after failure | `package-install.test.ts` |

### Firmware (CORE-R06)

| Method | Native handler | Contract / errors | Primary tests |
|---|---|---|---|
| `boardFacts(boardId)` | `board_facts_sync` | known/verified facts; unknown → `known:false`, no guesses | `core-adapters.test.ts`, native smoke |
| `firmwarePreflight(req)` | `firmware_preflight_sync` | bytes/size/hash/signature/board; no port opened | `core-firmware-flasher.test.ts`, native smoke |
| `firmwareInstall(req)` | `firmware_install_sync` | lease + write + reset + identity; `sim`/`esptool(dry-run)` | native smoke, `native-firmware.test.ts` |
| `firmwareProgress()` | `firmware_progress` | phase/ratio/error snapshot | native smoke |
| `cancelFirmware()` | `cancel_firmware` | flag checked at stage boundaries | native smoke |
| `firmwareReadback({port})` | `firmware_readback_sync` | real HELLO under the lease | native smoke PTY |

### Config / scene / animation

| Method | Native handler | Contract / errors | Primary tests |
|---|---|---|---|
| `getConfig(key)` / `setConfig(key, value)` | `*_config_sync` | capability-gated; secrets redacted in UI | `index.test.js`, App tests |
| `setScene(node)` / `clearScene()` | `set_scene_sync` | CBOR scene; budgets checked device-side | simulator scene tests |
| `setAnimation(clip)` / `stopAnimation()` | `*_animation_sync` | unknown clip rejected | simulator tests |
| `setAnimationBlend(spec)` / `animationBlend()` | `*_blend_sync` | `"clip:weight,..."` or `"off"` | native smoke |

### Input / diagnostics / events

| Method | Native handler | Contract / errors | Primary tests |
|---|---|---|---|
| `simulateInput` / `sendInput` | `*_input_sync` | pointer phases; bounded active pointers | simulator/App input tests |
| `emitMetric` / `takeDiagnostics` / `diagnosticCount` | diagnostics | bounded queue | native smoke |
| `pollEvents()` | `poll_events_sync` | drained events carry a generation | native smoke, `faults.rs` |
| `multiTapRadius` / `setMultiTapRadius` / `multiTapMs` / `setMultiTapMs` | config | clamped ranges | App/native tests |

## Test tiers

| Tier | Command | Proves |
|---|---|---|
| JS validation | `node --test sdk/node/index.test.js` | input copying, ranges, no-native paths |
| Rust logic | `cargo test -p mdc-core -p mdc-protocol -p mdc-simulator` | codec, state machine, containers |
| Native interface | `node --test sdk/node/native-smoke.test.js` | real N-API + PTY/WS/TCP paths |
| Cross-process | `cargo test -p mdc-sim-peer --test faults`, `cxx_peer.rs` | process boundaries, faults, C++ parity |
| App adapter | `npm run test -- tests/core-*.test.ts tests/native-*.test.ts` | ports, error mapping, native E2E |
| Delivery | `./tools/check.sh`, `./tools/gates.sh rc` | full workspace + App pin/gates |

## Adding an interface

1. Add the DTO and `#[napi]` method in `sdk/node/native` (async task unless it is
   a pure counter read).
2. Add the JS wrapper validation/copy in `sdk/node/index.js` and the declaration
   in `index.d.ts`.
3. Add the structural method to `SdkManager` in the App `adapters/core/sdk.ts`
   (optional until the pin guarantees it), and the narrow port method in
   `types.ts`.
4. Implement it in `connection.ts` or `firmware.ts`; keep `native.ts` composition
   only.
5. Tests: JS wrapper, native smoke (if it crosses the process), App unit with a
   manager double, App native E2E (if it needs a peer).
6. Update this catalog, the integration matrix and the pin in one batch.
