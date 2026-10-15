# Changelog

All notable host / simulator changes for MyDeviceCanvas. Physical board verification stays out of scope until measured on hardware.

## [0.1.36] — Node SDK package install (N233–N242)

### Added

- `sdk/node` native + JS `installPackage(bytes)` (uploads a `.mdcpkg`) and
  `packageReady()` (N233, N234, N238)
- JS mock test and a native smoke test for the install path (N235, N239)
- Electron demo `packageMagicOk` helper + test (N236)
- `docs/node-sdk.md` documenting the API (N237)

## [0.1.35] — Geometry morph targets (N223–N232)

### Added

- `MeshGeometry.morph_targets` + glTF POSITION delta import (N223, N224)
- `morphed_positions` (base + Σ w·delta) applied by `render_geometry_at`,
  `render_scene_at` and `render_package_scene` before skinning (N225, N226)
- `mdc render-mesh` / `mdc package` report `morphTargets` (N227)
- Geometry morph unit test, a device morph-mirror fixture and a render soak (N228–N230)

## [0.1.34] — Live2D motion3 curves (N213–N222)

### Added

- `motion3.json` `Curves[].Segments` parse into `MotionInfo.curves`
  (`MotionCurve`, bounded to `MAX_MOTION_SEGMENTS`) (N213)
- `MotionCurve::sample` evaluates linear/step/inverse-step/bezier segments;
  `MotionInfo::value_at(id, time)` (N214, N215)
- `mdc motion <motion3.json> [--param ID] [--samples N]` (N217)
- `docs/live2d-motion.md`; curve unit tests, a compat fixture and a sampling soak (N218–N221)

## [0.1.33] — Texture filtering and material tint (N203–N212)

### Added

- `TextureMap` nearest/bilinear `Filter` + `with_filter` (N203)
- `rgb565_mul` and a per-draw `tint` (material colour × texture) in the textured
  rasteriser (N204)
- Geometry/scene/package renders tint the texture by the mesh material colour (N205)
- The device tints its textured geometry by the material colour too (N206)
- `mdc render-mesh --filter nearest|linear` (N207)
- Bilinear/tint unit tests, a tinted-package fixture and a linear-filter soak (N208–N210)

## [0.1.32] — Device mesh scene rendering (N193–N202)

### Added

- `mdc-bake::render_package_scene` renders a `DevicePackage`'s whole mesh scene
  at a time (node animation, skinning, material colour/texture) (N193, N194)
- `FakeDevice` keeps the accepted package and renders it as a multi-mesh scene at
  its clock, so `MIRRORFRAME` carries a device-rendered animated mesh (N195, N196)
- `mdc package` reports node/mesh/animation counts (N197)
- Animated mesh-package mirror fixture and device mesh render soak (N198, N199)

## [0.1.31] — Multi-mesh scene render (N183–N192)

### Added

- `texture::draw_indexed` — draw a bounded geometry into an existing framebuffer (N183)
- `render_scene_at` renders every mesh node (skinned or node-placed) against one
  bind-pose box with per-mesh material colour/texture (N184)
- `bake_scene_frames` / `bake_scene_to_package` (N185)
- `mdc render-mesh` renders the whole scene by default (`--single` for one mesh) (N186)
- `render_package_mesh` + `mdc render-package` for mesh-mode packages (N187)
- Multi-mesh compat fixture and scene render soak (N188, N189)

## [0.1.30] — Material colours + package textures (N173–N182)

### Added

- `Material.color` from glTF `baseColorFactor` + `color_rgb565()` (N173, N174)
- Geometry and preview renders prefer the mesh's material colour (N175)
- `DevicePackage.texture: Option<TextureBlob>` (RGB565) with `TextureBlob::sample` (N176)
- `mdc-bake` `bake_texture_blob` / `mesh_package_with_texture` (N177)
- `FakeDevice` samples the package texture through the geometry UVs (N178)
- `mdc package --texture tex.png` writes a textured mesh package (N179)
- Material/texture compat fixture and package soak (N180, N181)

## [0.1.29] — Bounded full-mesh geometry (N163–N172)

### Added

- IR `Mesh.geometry` (`MeshGeometry`): POSITION/indices/UV/skin, bounded to 4096
  vertices / 16384 indices (N163)
- glTF importer decodes full geometry per primitive (N164)
- `mdc-bake` `raster_indexed`, `render_geometry_at` (aspect-fit, skinned, textured)
  and `bake_geometry_frames` / `bake_geometry_to_package` (N165–N167)
- `mdc render-mesh` prefers full geometry and reports `geometry` (N168)
- `FakeDevice` projects and renders accepted full geometry (N169)
- Geometry compat fixture and render soak (N170, N171)

## [0.1.28] — Animation cross-fade + scene primitives (N153–N162)

### Added

- Device animation cross-fade: switching clips blends the frozen previous frame
  over the new clip's `fade_in_ms` (capped 2 s) (N153, N154)
- `FakeDevice::stop_animation` clears clip and cross-fade state (N155)
- Scene `circle` (inscribed ellipse) and `line` (Bresenham) primitives (N156, N157)
- Scene-primitive compat fixtures and soak (N157, N158)

## [0.1.27] — HTTP mirror for OBS (N143–N152)

### Added

- `mdc-mirror::encode_bmp` — 24-bit BMP from a BGRA frame (no JPEG dependency) (N143)
- `mdc-mirror::HttpMirrorServer` — bounded HTTP/1.1 fan-out: `/`, `/frame.bmp`,
  `/frame.ppm` for an OBS Browser Source (N144)
- CLI `mdc mirror --http ADDR` and `--format bmp|ppm` (N146, N147)
- `docs/obs-bridge.md` documents the Browser Source delivery (N148)
- HTTP-mirror compat fixture and publish soak (N149, N150)

## [0.1.26] — Offline render + scene expression (N133–N142)

### Added

- `mdc-bake` `render_frame_at`: single posed frame (morph → skin → textured raster) (N133)
- CLI `mdc render-mesh <model.gltf>` and `mdc render-package <pkg>` to PPM (N134, N135)
- Scene `rect` colour accepts `#RGB`, bare hex and named colours (N136)
- Scene `group` nodes offset and scale children via their `rect` (N138)

### Changed

- `render_scene` clears the framebuffer first, so repeated renders are
  deterministic (N137)

## [0.1.25] — Morph targets + device polish (N123–N132)

### Added

- glTF morph targets (`targets[].POSITION`) and `mesh.weights` import into
  `Mesh.morph_targets` / `morph_weights` (N123)
- Animation `weights` channels import as flat key×target tracks (N124)
- `mdc-bake` `sample_channel`, `morph_weights_at`, `apply_morphs`; morphs deform
  before skinning in `bake_skinned_mesh` / `bake_textured_mesh` (N125, N126)
- CLI `mdc bake-mesh` reports `morphTargets` (N127)
- Device scene text scales with the node rect (1..=8×, N128)
- Simulator `animation_loops()` observability (N129)

## [0.1.24] — Texture-mapped skinned bake (N113–N122)

### Added

- `TEXCOORD_0` imports into `Mesh.preview_uv` (N113)
- `mdc-bake` `TextureMap` + wrapping nearest sampling + barycentric textured
  triangle rasterisation (N114, N115)
- `bake_textured_mesh` / `bake_textured_to_package` combine skinning with UV
  sampling; a mesh without UVs falls back to a solid colour (N115, N116)
- CLI `mdc bake-mesh --texture <png>` and `skinned`/`textured` flags (N117, N121)
- Device font gains punctuation glyphs and fixes lowercase mapping (N118)

### Fixed

- The baked-package capability gate validates the whole `columns × rows` grid,
  so a padded atlas (frame count not a perfect square) is accepted (N119)

## [0.1.23] — Skeletal skinning bake (N109–N112)

### Added

- glTF node TRS/matrix, `JOINTS_0`/`WEIGHTS_0`, `skins[].inverseBindMatrices` and
  timed animation channels import into the IR (N109, N110)
- `mdc-bake` linear blend skinning: `pose_world_matrices`, `skin_preview`,
  `bake_skinned_mesh`, `bake_skinned_to_package` (N111)
- CLI `mdc bake-mesh <model.gltf>` writes skinned pose frames or a runtime package
  (N112)

## [0.1.22] — Distinct mesh bake frames

### Added

- `bake_displaced_mesh` shifts a preview triangle per frame so the atlas is not a cycled source image

## [0.1.21] — Indexed preview and fade-out

### Added

- glTF index accessors become `preview_indices`; the simulator draws those triangles
- Non-looping clips honor `fade_out_ms`
- Scene text uses a 5×7 face for digits and A–Z

## [0.1.20] — Preview, sprite blit, fade-in (N99–N108)

### Added

- Installed mesh `preview_xy` rasterizes on the simulator (N99)
- Scene `sprite` nearest-neighbour atlas blit (N100)
- Atlas clip `fade_in_ms` darkens the first frames (N101)
- Bitmap glyphs `0` and `1` (N102)
- CLI `mdc preview-mesh` (N103)

## [0.1.19] — Pixel raster batch (N89–N98)

### Added

- glTF POSITION samples become `Mesh.preview_xy` (N89)
- `raster_triangle_fan` and `mix_rgb565` in `mdc-bake` (N90, N91)
- Scene `text` bitmap and `image` hex blit on the simulator (N92, N93)
- CLI `mdc raster-demo` (N94)

## [0.1.18] — Scene rendering batch (N79–N88)

### Added

- `SceneNode.rect` geometry + `color_rgb565` (`scene_rect.json` vector) (N79)
- Device scene rasterizer: rect fill, z order, clipping (N80)
- Device-rendered scene emitted as a `MIRRORFRAME` (N81)
- `mdc scene-render --out f.ppm` exports the device framebuffer (N82)
- Setting a scene stops the running animation; core `show_scene` (N83)
- Scene geometry compatibility fixtures (N84) and soak (N85)
- Electron/Node scene preview with geometry (N86)
- `docs/scene-spec.md` rendering rules (N87)

## [0.1.17] — Device animation batch (N69–N78)

### Added

- `AtlasClip` + `AtlasBlob.clips`, populated by the baker (N69, N70)
- Device atlas animation runtime driven by the `animation` config key (N71)
- Device renders the running clip and emits it as a `MIRRORFRAME` (N72)
- Core `set_animation`/`stop_animation`/`active_animation` (N73)
- Node `setAnimation`/`stopAnimation` (N74)
- Electron animation controls (N75)
- Animation compatibility fixtures (N76) and soak (N77)
- `docs/baked-sprites.md` playback section (N78)

## [0.1.16] — Baked sprite delivery batch (N59–N68)

### Added

- `PackageMode::BakedSprites` + `AtlasBlob` in the device package (N59)
- `bake_to_package` sealing a baked atlas into a runtime package (N60)
- Device accepts a baked package and records the atlas grid (N61)
- `mdc bake --package` writes a `.mdcpkg` (N62)
- Scene set/clear in the C ABI and Swift (N63)
- `mdc inspect` config keys + scene limits; `--watch` scene/atlas counts (N64)
- Baked-package and scene compatibility fixtures (N65)
- Baked-package soak (N66)
- `docs/baked-sprites.md` (N67)

## [0.1.15] — Pixel baking + Scene batch (N49–N58)

### Added

- `mdc-bake` crate: PNG/JPEG decode + resize + RGB565 with a decode-bomb guard (N49)
- `AtlasPacker` and `bake_sprites` producing a baked atlas + manifest (N50, N51)
- `mdc bake` CLI writing `atlas.ppm` + `atlas.json` (N52)
- Scene capability + `SCENE` (0x34) contract and spec (N53)
- Structured scene set/clear in Core and the simulator (N54)
- `mdc scene` CLI and Node `setScene`/`clearScene` (N55)
- Electron configuration panel (N56)
- `mdc inspect --watch` bounded live metrics (N57)
- Bake soak test and docs (N58)

## [0.1.14] — Configuration host + provisioning batch (N39–N48)

### Added

- `Capabilities.config` + `CONFIG` (0x33) CBOR message with a canonical vector (N39)
- Core `set_config`/`get_config` gated on the advertised keys (N40)
- Simulator config store with per-key validation and secret redaction (N41)
- `mdc config get|set` CLI and Node `getConfig`/`setConfig` (N42)
- Best-effort live mDNS query in `mdc discover` (N43)
- esptool version lock enforced before a live flash (N44)
- Capability-driven model optimizer plan (N45)
- VRM meta/expressions and Live2D physics/pose metadata import (N46)
- Config compatibility fixtures (N47)
- `docs/config-host.md` (N48)

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
