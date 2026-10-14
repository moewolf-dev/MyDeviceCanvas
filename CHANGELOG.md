# Changelog

All notable host / simulator changes for MyDeviceCanvas. Physical board verification stays out of scope until measured on hardware.

## [0.1.13] — OBS delivery + SDK/runtime batch (N29–N38)

### Added

- `mdc-mirror` atomic PPM directory sink and bounded TCP mirror server (N29, N30)
- `mdc mirror` CLI command writing PPM files or serving TCP (N31)
- `mdc package` / `mdc install-package` CLI commands (N32)
- C++ runtime `PACKAGE` receive + `MIRRORFRAME` emit (N33)
- C ABI and Swift mirror surfaces (N34, N35)
- Bounded mirror + package memory soak crate and package/mirror fixtures (N36, N37)
- `docs/obs-bridge.md` OBS delivery guide (N38)

## [0.1.12] — Form B end-to-end batch (N19–N28)

### Added

- `MdcPackage` versioned runtime package container with integrity + capability gate (N19, N20)
- `PackageAssembler` bounded chunked upload (N21)
- `PACKAGE` protocol message (0x32) begin/chunk/commit with a shared vector (N22)
- `mdc-core install_package` gated on the device renderer; FakeDevice accepts it and reports ready (N23)
- `ManagerEvent::Mirror` metadata event and `mirror_stats()` (N24)
- Node SDK `onMirrorFrame`/`startMirror`/`stopMirror`/`takeMirrorFrame`/`mirrorStats` (N25)
- Electron mirror preview panel (N26)
- Board profile `renderer` section + generated header macros (N27)
- `mdc inspect` reports mirror + renderer capability (N28)

## [0.1.11] — Model compiler, mirror and capability batch (N09–N18)

### Added

- glTF `skins` and VRM humanoid import into IR skeletons (N09)
- Live2D `motion3.json` bundle import with real curve counts and fade metadata (N10)
- Deterministic sprite-atlas baker with bounded frame/grid layout (N11)
- `mdc-mirror` reference PPM sink and bounded latest-wins publish queue with pacing (N12)
- Capability-gated dirty-region mirror path that patches the base frame (N13)
- ESP32-S3-BOX-3 profile grounded in the esp-bsp reference; Arduino AVR profile grounded in arduino-core-avr; Rockchip/Jetson profiles cite their SDK references (N14, N15)
- Real mDNS / DNS-SD subset discovery with a loopback responder test (N16)
- `Capabilities.renderer{}` on the wire with Rust/C++ shared bytes and `compile_auto` wiring (N17)
- `Runner` abstraction, fake runner, and a pinned esptool version lock (N18)

## [0.1.10] — Model compiler deepening

### Added

- `GltfImporter` accepts the binary `.glb` container (JSON + BIN chunks) as well as `.gltf` JSON
- Accessor/bufferView/buffer bounds are validated; an overrun is rejected before use
- Buffers resolve from the GLB BIN chunk or a base64 `data:` URI; the decoded total is capped by `max_input_bytes`
- Embedded image dimensions are read from the PNG IHDR / JPEG SOF header instead of trusting the declared size
- `Live2DImporter` reads a Cubism `model3.json` manifest into MDC IR (textures, motion groups, expressions, moc reference); the `.moc3` is not decoded
- `TargetBackend` trait with the MCU `esp32-plane` backend and a second `linux-mesh` backend; `compile_auto` selects by renderer capability only
- MDC IR `Model` gains an optional `sprites` field for author-supplied atlases (e.g. Live2D)

## [0.1.2] — Simulator batch without physical gates

### Added

- C ABI `poll_event`, `simulate_input`, and `connect_tcp`
- Swift `AsyncStream` over the C event queue
- Node `connectTcp` and an Electron `tcp:` endpoint
- Simulator OTA rollback that keeps the installed firmware on failure
- Shared protocol vector check in the C++ host test
- `AllocationLedger` short soak and a Linux systemd unit contract
- `mdc sim-accept`: handshake, full frame, tile, bounded inspect, and a failed OTA against `mdc-sim-peer` without changing firmware

## [0.1.3] — Simulator acceptance batch

### Added

- `tools/check.sh` runs `mdc sim-accept` against `mdc-sim-peer` and the ESP-IDF host stub
- `MDC_PAIRING_STORE` so pairing files do not share one temp path
- `mdc ota-confirm` installs firmware `2.0.0` on the same connection
- TCP reconnect blocks Tile until a displayed full frame
- `mdc-sim-peer --emit-input` delivers one touch after the full frame
- Node TCP smoke, Swift `MDC_TCP_ADDR` hello, and Electron `planConnection`
- WebSocket accept path: unpaired display is refused, then a full frame is acknowledged

## [0.1.4] — Simulator behavior batch

### Added

- FakeDevice rejects Frame and Tile with `E_FROZEN` while an OTA begin is active, and accepts them again after abort or rollback
- `dual-sim` board advertises `main` and `overlay`; an unknown surface is rejected
- Touch coordinates go through `transform_touch` using the board rotation; out-of-range points are dropped
- Three missed heartbeats reconnect with 1/2/4 s backoff, and the next Tile waits for a displayed full frame
- A third full frame replaces only the unsent pending slot
- WebSocket binary payloads larger than the configured limit are rejected before decode
- `mdc-sim-peer --drop-next` drops one packet without changing pixels
- `mdc linux-accept` checks the 800×480 linux-virt gate: Tile fails before the full frame and succeeds after it

## [0.1.9] — Budgets on the send path

### Added

- `send_frame` counts the pixel buffer, transport copy, and SDK copy before enqueue. One legal frame fits; a second full-size frame that would pass 3 MiB is refused. The hold is released on a displayed acknowledgement
- A second surface that would pass the copy budget is refused
- `FrameStore` refuses another framebuffer when the simulated reserve would be exhausted
- Artifact preflight checks a present signature and rejects an empty or mismatched one
- Non-zero header flags are rejected before the declared payload is allocated
- The C++ peer freezes rendering on OTA `begin` and resumes after `abort` or `timeout`
- A replayed displayed acknowledgement does not launch the pending frame again
- Input that arrives before the new handshake is dropped
- The C ABI releases the frame-copy budget on disconnect, and a later send does not allocate

## [0.1.8] — Link generation, short writes, and budgets

### Added

- A displayed ACK from the previous link cannot release the frame in flight on the new link
- Non-zero reserved bytes and unknown message types are rejected before the declared payload is allocated
- The C++ peer reports `E_CHUNK`, `E_INCOMPLETE`, and `E_FROZEN` without changing pixels
- A fake artifact signature must match or provisioning is refused
- A short write resumes the same frame and does not admit the next frame
- A successful write without PONG still expires the heartbeat on the virtual clock
- Host copy budget counts the pixel buffer, the transport copy, and the SDK copy
- A simulated heap reserve refuses another framebuffer
- E-ink busy keeps the previous frame and does not advertise color FPS
- A declared decompression size above the quota is refused before allocation

## [0.1.7] — Queue bounds, resync, and timing

### Added

- The byte stream skips serial-log noise and still rejects a bad checksum
- Move events coalesce; a full input queue keeps down/up/cancel and cancels that pointer
- Unacked control messages stop at 32. Frame and Tile stay on the surface queue
- `mdc benchmark` reports displayed-ACK transport, render, and end-to-end time plus dropped frames
- Chunked asset uploads abort back to zero bytes, and `jpeg: false` falls back to RGB565
- A flash lease blocks a second open of the same port until release or timeout
- Linux peers select a memory framebuffer or a no-session backend
- Install and pairing logs replace passwords and tokens

## [0.1.6] — Displayed frames, surfaces, and pairing

### Added

- A `received` acknowledgement keeps the held frame until `displayed`
- C ABI, Swift, and Node can list `dual-sim` main (8×4) and overlay (4×2)
- An overlay tile updates only the overlay; an out-of-bounds tile returns `E_DIM`
- The C++ peer returns `E_DIM` before committing a mismatched frame
- Revoking a pairing rejects the next frame on the same TCP session
- Disconnect `cancel` is visible from the C ABI and Node, with the same pointer and coordinates

## [0.1.5] — Held frames, cancel, and peer errors

### Added

- An ACKed in-flight frame releases the next held frame onto the link
- A full frame queue rejects another Tile with `QueueFull`
- Disconnect sends `cancel` for pointers that are still down
- OTA begin rolls back when the virtual clock passes the chunk timeout
- Dropping a chunked frame's commit keeps the previous pixels and accepts the next full frame
- `mdc_linux_peer` answers a mismatched tile with `E_BASE`
- A silent WebSocket peer misses three heartbeats without treating the open frame as displayed

## [0.1.1] — Wave 12 host gaps

### Added

- Electron demo connects explicitly, draws RGB565 color bars, and sends Frame/Tile through the public Node SDK
- Swift `MdcSession` wrapper over the C ABI, including Scene refusal
- `mdc-tauri-demo` command layer: endpoint switch requires a full frame; inspector stores counts only
- Linux runtime `update_geometry` and `mdc_linux_probe` (no display session / zero size)
- Board profiles `pi4-hdmi` and `waveshare-eink` (`color_fps: false` on e-ink)
- `Session::send_scene` returns Unsupported and does not emit a Scene packet

## [0.1.0] — Waves 0–7 architecture (sim-verified)

### Added

- Protocol v1 host crates: `mdc-protocol`, `mdc-transport`, `mdc-core`, `mdc-simulator`
- CLI `mdc` with `simulate`, `devices`, `inspect`, `send-image --sim`, `benchmark --sim`, `--address` TCP
- Node native `NativeManager` FakeDevice session (`devices`, `sendFrame`, `dispose`)
- Discovery `MockMdnsProvider`, provision `tools/flasher` SimFlasher
- InputEvent → `ManagerEvent::Input`; OTA capability-gated in Session + FakeDevice
- C ABI (`sdk/c`), Swift wrap notes, optional `mdc-assets` LRU cache
- Inspector metrics JSON from last simulate; Linux runtime peer + board templates
- Compatibility + memory lifecycle tests (major mismatch, low limits, connect/disconnect ×1000)

### Notes

- G03 exit criterion: **sim-verified** end-to-end on FakeDevice / MemoryLink (see `docs/compatibility-matrix.md`).
- Physical: pending for all board profiles.
