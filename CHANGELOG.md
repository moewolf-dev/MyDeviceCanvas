# Changelog

All notable host / simulator changes for MyDeviceCanvas. Physical board verification stays out of scope until measured on hardware.

## [0.1.53] — Per-slot UV sets, dithering, palette quantization, per-pixel occlusion, sRGB filtering, verify, Swift sendInput, RSS delta (N403–N412)

### Added

- per-slot `texCoord`: `Material.uv_set` selects the base-colour UV set and
  `uv_set2` (defaulting to `uv_set`) the secondary maps' set, so normal/occlusion/
  emissive/metallicRoughness can sample `TEXCOORD_1` (N403)
- ordered (Bayer) dithering: `bayer_dither_rgb565` and `mdc bake --dither N` (N404)
- median-cut palette quantization: `quantize_palette` and `mdc bake --palette N`
  (writes `atlas.pal.json` + `atlas.idx`) (N405)
- per-pixel occlusion: `occlusionTexture` scales the shaded colour per pixel in the
  per-pixel pass (N406)
- sRGB-correct bilinear filtering: `TextureMap::with_srgb` and `render-mesh --srgb`
  (N407)
- `MdcPackage::verify` and `mdc verify <file.mdcpkg>` (checksum + size check, exit
  code) (N408)
- Swift `MdcSession.sendInput(pointerId:phase:x:y:)` (C ABI
  `mdc_session_send_input`) plus a smoke call (N409)
- `mdc-inspector --rss-delta` bounded host-RSS growth sampler (N410)
- compatibility/memory coverage: full-PBR fixture and an atlas post-processing soak
  (N411)

## [0.1.52] — Emissive/PBR textures, per-slot UV transforms, mip chain, specular extensions, session counters, validate, sendInput (N393–N402)

### Added

- `emissiveTexture` import plus an additive emissive term (scaled by
  `emissiveStrength`) in the textured renderer (N393)
- `KHR_texture_transform` is honoured per texture slot (base colour, normal,
  emissive, metallicRoughness), each sampled with its own transform (N395)
- `metallicRoughnessTexture`'s green (roughness) channel raises the per-teixel
  ambient floor in the per-pixel pass (N394)
- `bake_mipmaps` (box-downsampled, bounded chain) and `mdc bake --mipmaps` (N396)
- `KHR_materials_specular` (`specularFactor`/`specularColorFactor`) and
  `KHR_materials_ior` import; `specularFactor` scales the highlight (N397)
- `Session::stats()` host counters (`packets_sent`, `bytes_sent`, `acks_received`,
  `frames_sent`), surfaced by `mdc inspect --watch` (N398)
- `mdc validate <model>` prints a `ValidationReport` and exits non-zero on fatal
  reference errors (N399)
- host→device pointer input: `Session::send_input`, the C ABI
  `mdc_session_send_input`, and the Node `_native.sendInput` (capability-gated on
  `touch`) (N400)
- compatibility/memory coverage for the PBR texture path and specular factor (N401)

## [0.1.51] — UV sets, 8-influence skinning, scene lights, smooth/specular shading, quantization, CLI introspection (N383–N392)

### Added

- glTF `texCoord` selects the material's UV set (`Material.uv_set`) and
  `TEXCOORD_1` is imported into `MeshGeometry.uv2` (N383)
- `JOINTS_1`/`WEIGHTS_1` import plus `skin_arrays_multi`/`skin_normals_multi`, so a
  vertex can blend up to eight skinning influences (N384)
- `KHR_lights_punctual` import (`Model.lights`/`DevicePackage.lights`); the first
  directional light overrides the renderer's fixed key light (N385)
- `ShadeMode::Smooth` per-pixel (Gouraud) shading for the package renderer, and
  per-pixel shading now also covers untextured triangles (N386)
- `ShadeMode::SmoothSpecular` Blinn-Phong specular highlight with
  strength/shininess derived from `roughness`/`metallic` (N387)
- `mdc model-info` prints a JSON import report (`Model::summary`) (N388)
- `mdc diff <a.mdcpkg> <b.mdcpkg>` prints a JSON manifest + structure diff (N389)
- `KHR_mesh_quantization`: integer POSITION and normalized byte/short TANGENT
  decode (N390)
- compatibility fixtures (smooth/specular shading, imported light on-device) and a
  quantized-import soak (N391)

## [0.1.50] — Texture transforms, scene selection, sparse/interleaved import, AO, normal maps, atlas packing (N373–N382)

### Added

- `KHR_texture_transform` on the base-colour texture (`Material.uv_transform`) is
  imported and applied to UVs before sampling (N373)
- `KHR_materials_emissive_strength` scales `emissiveFactor` before it lifts the
  ambient floor (N374)
- glTF default-scene selection: with `scenes` declared, only the default scene's
  nodes keep a mesh, so unreferenced nodes are not drawn (N375)
- glTF sparse accessors are de-sparsed into a tight byte image before decoding
  (N376)
- interleaved accessors (`bufferView.byteStride` + accessor `byteOffset`) are
  de-strided into the same tight image (N377)
- `occlusionTexture` multiplies each package/device triangle's shade (sampled at
  the triangle centroid); `occlusionStrength` scales it (N378)
- `TANGENT` + `normalTexture` drive per-pixel normal mapping on the package/device
  renderer (per-pixel Lambert); geometry without tangents keeps the per-triangle
  shade (N379)
- `pack_material_atlas` (and `mdc package --atlas MAX_PAGE`) shelf-packs a
  package's per-material textures into one bounded page, carrying 1-bit alpha and
  remapping material indices and mesh UVs (N380)
- compatibility fixtures (occlusion, normal map, packed atlas) and a bounded
  atlas-pack soak (N381)

## [0.1.49] — Emissive/unlit, vertex colours, skinned normals, WS/C parity, crash reports, scene clip, asset store, RSS (N363–N372)

### Added

- glTF `emissiveFactor` + `KHR_materials_unlit` import; an unlit material draws
  without directional shading and emissive lifts the ambient floor (N363)
- glTF `COLOR_0` import into `MeshGeometry.colors`, barycentric-interpolated and
  multiplied into the textured/solid raster on both host and device paths (N364)
- skinned normals: `skin_normals`/`transform_direction` rotate normals by the
  joint matrices so shading follows the pose (N365)
- `mdc_session_connect_ws` in the C ABI and `MdcSession(webSocket:)` in Swift,
  alongside the Node `connectWebSocket` (N366)
- the reserved `Crash` (`0x62`) diagnostic now carries a CBOR `CrashReport`;
  `Session` records it, `mdc diag --crash` emits it, C ABI kind 3 / Node `crash`
  map it (N367)
- scene `clip` node: a scissor rect that offsets (but does not scale) its
  children and only composites pixels inside the rect (N368)
- `mdc-assets` `DiskCache` (SHA-256 keys, LRU by mtime, byte quota) and
  `mdc asset put|ls|get|prune` (N369)
- bounded host RSS sampling: `mdc-inspector --rss`, `mdc inspect --rss`, and an
  `rss_bytes` field in the JSONL series (N370)
- compatibility fixtures (unlit + vertex colours on-device) and a disk-asset-cache
  quota soak (N371)

## [0.1.48] — Multi-material textures, OTA over TCP, rounded rect, stream scale (N353–N362)

### Added

- multi-material textures: `DevicePackage.texture_blobs` (indexed by
  `Material.texture`) with `mesh_package_with_textures`/`attach_model_textures`;
  the package-scene and `render_package_mesh` renderers sample each mesh's own
  texture, falling back to the single `texture` (N353, N358)
- `mdc ota --address host:port` drives the OTA over TCP (N354)
- scene `rounded-rect` (corner radius in `value2`) (N355)
- `/stream[.jpg]?scale=N` downscales each streamed part; frames are encoded on
  demand from the raw BGRA frame (N356)
- `ManagerEvent::Telemetry` (metadata-only) + C ABI kind 7 and Node `telemetry`
  events (N357)
- device multi-texture mesh fixture and a multi-texture/stream-scale memory soak
  (N359, N360)
- `mdc-mirror` `/health` JSON counters (N361)

## [0.1.47] — Roughness, soft alpha, multi-clip bake, delivery tuning, OTA (N343–N352)

### Added

- glTF `metallicFactor`/`roughnessFactor` imported; roughness raises the shading
  ambient floor (N343)
- 8-bit texture alpha (`decode_rgba8_native`/`texture_map_with_alpha8`) folded into
  a `BLEND` material alpha for soft edges (N344)
- `bake-mesh --all-animations --package` bakes one clip per animation into a
  multi-clip atlas package (N345)
- JPEG `--quality` and HTTP `/frame.*?scale=N&quality=N` (nearest downscale from the
  raw frame) (N346)
- device `config filtering <nearest|linear>` selects the package-scene sampler (N347)
- scene `gradient` kind (vertical linear gradient) (N348)
- `sdk/node` diagnostics surface: `emitMetric`/`takeDiagnostics`/`diagnosticCount` (N349)
- `mdc ota <version>` drives begin → chunk → confirm and reads the version back,
  with a rollback report (N350)
- `mdc inspect`/`--watch` expose the surface display profile + diagnostics counters (N351)

## [0.1.46] — Transparency, JPEG delivery, Telemetry, provisioning, WS (N333–N342)

### Added

- glTF `alphaMode: BLEND` with `baseColorFactor` alpha renders as source-over
  transparency in the host and device raster paths (N333)
- glTF sampler wrap (`CLAMP_TO_EDGE` → `clamp`) recorded and applied by
  `TextureMap::with_wrap`; `mdc render-mesh --wrap` and auto-use of the model's
  own embedded texture (N334)
- the reserved `Telemetry` opcode now carries a structured `TelemetryEvent`
  (name/level/fields); `Session` records it in the diagnostics queue; `mdc diag
  --telemetry` (N335)
- `mdc-mirror`: dependency-free baseline JPEG encoder + `/frame.jpg` +
  `mdc mirror --format jpg` (N336)
- `mdc-mirror`: JPEG `multipart/x-mixed-replace` live stream at `/stream.jpg` (N337)
- protocol/simulator: scene node `opacity` (0..=255) applied as source-over alpha
  for `rect`/`text`/`image`/`sprite`/`circle`/`line` (N338)
- `mdc provision <pkg>`: preflight → install → version readback with a rollback
  report on a simulator board (N339)
- `sdk/node`: `connectWebSocket(url)` + `mdc-sim-peer --ws` (N340)
- Electron demo `jpegStreamUrl` / `normalizeOpacity` helpers (N341)

## [0.1.45] — Real model assets, shading, PNG delivery, core pacing (N323–N332)

### Added

- `mdc-model`: glTF embedded image bytes are retained (`Texture.data`, bounded by
  `max_texture_bytes`) with `Model::embedded_texture`/`material_texture` (N323)
- `mdc package` auto-uses a material's embedded `baseColorTexture`
  (`auto_texture_blob`/`resolve_texture_blob`); `--texture` still overrides and a
  failed decode is reported as `textureError` (N324)
- `mdc-bake`: painter depth ordering in `render_scene_at`/`render_package_scene`
  (far-to-near by mean z) (N325)
- glTF `NORMAL` import into `MeshGeometry.normals` (f32 or normalized i8/i16) (N326)
- per-triangle directional shading from normals (fixed key light, ambient 0.45) in
  the scene/package renderers (N327)
- `mdc-mirror`: dependency-free PNG encoder (stored deflate) + `/frame.png` +
  `mdc mirror --format png` (N328)
- `mdc-core`: opt-in `Session::set_frame_pacing` / `clear_frame_pacing` backed by
  `mdc-schedule`, refusing over-budget frames with `CoreError::Throttled` (N329)
- `mdc-simulator`: mesh-package clip cross-fade on `config animation` changes (N330)
- `runtime/esp32` C++: `encode_metric_cbor`/`encode_boot_info_cbor` byte-identical
  to the Rust vectors (N331)

## [0.1.44] — Live OBS stream, diagnostics, device clips, scene text (N313–N322)

### Added

- `mdc-mirror`: live `multipart/x-mixed-replace` HTTP stream at `/stream` for an
  OBS browser source; each streaming client buffers at most one part and drops
  newer frames rather than growing memory (`clipped_frames`) (N313)
- `mdc mirror` paces its output through `mdc-schedule`
  (`--source-fps`/`--max-fps`/`--bandwidth`; `paced_deferred`/`paced_dropped`
  stats) (N314)
- `mdc-bake`/`mdc-simulator`: `SceneClip` selection for a device mesh package
  (`config animation <index>|<name>|default|off`), so the device is not limited to
  `animations.first()` (N315)
- protocol/core: `MetricReport`/`BootInfo` CBOR payloads + a bounded,
  link-independent `Session::take_diagnostics()` queue; `mdc diag` and a bounded
  JSONL inspector series (`mdc-inspector --jsonl`) (N316, N317)
- Scene `text` word-wrap + explicit multi-line (`\n`), with the rect height
  setting a uniform glyph scale (N318)
- C ABI + Swift diagnostics surface (`mdc_session_emit_metric`/
  `mdc_session_take_diagnostic`/`mdc_session_diagnostic_count`, `MdcDiagnostic`)
  (N319)
- Electron demo live-stream helpers (`mirrorStreamUrl`/`parseMultipartBoundary`/
  `streamPanelStatus`) (N320)
- Compatibility + memory fixtures for the stream, diagnostics and text wrap
  (N321)

## [0.1.43] — Reserved diagnostics, reboot semantics, pacing, pixel formats (N303–N312)

### Added

- Reserved diagnostic/telemetry message types `Telemetry`/`Metric`/`Crash`/`BootInfo`
  (`0x60..0x63`), accepted (payload not decoded) by the Rust codec and the C++
  parser; shared `diagnostic_types.json` vector + docs (N303)
- `Hello.boot_id`, `Capabilities.boot_id`/`run_id`, `Ack.run_id`/`event_seq`
  (additive, omitted at 0); `Session` detects a changed device `boot_id`, drops the
  previous run's pending ACKs/held frames and ignores a stale-run ACK (N304)
- New crate `crates/mdc-schedule`: deterministic host frame/tile pacing
  (`Scheduler::plan`, fps clamp, byte budget, dirty-first packing,
  `merge_dirty_ranges`) with a bandwidth-constrained convergence test (N305, N306)
- `PixelFormat` enum threaded through protocol → core → CLI/Node/C/Swift; only
  RGB565 is implemented and the rest are rejected explicitly (N307)
- `Surface.{preferred_tile_size, supports_partial_update,
  supports_full_framebuffer}` + host adaptive tile cut (`plan_tiles`) (N308)
- `render-topology-decision.md` §4.3 mirror-read-source decision + §7.1 scheduling
  arithmetic; new `docs/submodule-contract.md` + README/status callouts (N309, N310)

## [0.1.42] — Device-side texture cutout (N293–N302)

### Added

- `TextureBlob.alpha` (bit-packed mask) + `validate`/`alpha_at` (N293)
- `bake_texture_blob` emits the mask when the source has transparency (N294)
- `render_package_scene`/`render_package_mesh` attach the blob mask so a packaged
  mesh cuts out transparent texels (N295)
- `FakeDevice` masks transparent texels of an accepted mesh package (N296, N297)
- Device masked-render unit test and a packaged-cutout compat fixture (N299, N300)

## [0.1.41] — Texture alpha cutout (N283–N292)

### Added

- `Material.alpha_mode` / `alpha_cutoff` from glTF `alphaMode`/`alphaCutoff`
  (`is_masked()`, `cutoff()`) (N283)
- `decode_rgba_native` + `texture_map_with_alpha`: a 1-bit alpha mask on
  `TextureMap` (N284, N285)
- Masked rasterisation: transparent texels are skipped when the material is
  `MASK` (N286, N287)
- `mdc render-mesh` / `bake-mesh` decode PNG alpha; masked/pixel unit tests,
  a compat fixture and docs (N288–N291)

## [0.1.40] — Multi-primitive mesh merge (N273–N282)

### Added

- `MeshGeometry::merge` rebases indices and appends positions/uv/skin (N273)
- glTF importer merges every primitive into one `Mesh.geometry` (N274)
- `Mesh.primitives` count + `mdc render-mesh`/`package` report it (N279)
- Multi-primitive unit test, compat fixture and render soak (N276–N278)

## [0.1.39] — Device dirty-region mirror (N263–N272)

### Added

- `BoardProfile.mirror_dirty_region` + device advertises `dirty-region` and
  stores the negotiated mode (N263)
- `FakeDevice` emits only changed byte ranges (`Begin + chunks + Commit`) in
  dirty-region mode (N264)
- `Session` patches dirty-region updates onto the last complete frame (N264)
- Dirty-region device/core tests, a compat fixture and a soak (N265–N269)
- `docs/render-topology-decision.md` §4.2

## [0.1.38] — CUBICSPLINE animation sampling (N253–N262)

### Added

- `sample_channel` implements glTF `CUBICSPLINE` as a cubic Hermite using the
  per-key in/out tangents; rotation results are normalized (N253, N254)
- `sample_channel_looped` wraps time into a clip's span for looping playback (N255)
- Cubic and looped sampling unit tests, a compat fixture and a sampling soak (N256–N258)

## [0.1.37] — C ABI + Swift package/config (N243–N252)

### Added

- C ABI `mdc_install_package` / `mdc_package_ready` / `mdc_get_config` /
  `mdc_set_config` + header declarations (N243, N244)
- C ABI tests for config round trip and package readiness (N245)
- Swift `installPackage` / `packageReady` / `getConfig` / `setConfig` (N246)
- Swift smoke exercises config + package readiness (N247)
- `sdk/swift/README.md` documents the new surface (N248)

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
