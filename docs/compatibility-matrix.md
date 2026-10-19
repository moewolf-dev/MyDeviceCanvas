# Compatibility Matrix

States: `architecture` | `sim-verified` | `physical: pending`

| Surface | Host / Sim | Physical |
|---|---|---|
| Protocol v1 HELLO / Capabilities | sim-verified | physical: pending |
| Frame (legacy + chunked) | sim-verified | physical: pending |
| Tile + base_frame_id | sim-verified | physical: pending |
| MemoryLink Session | sim-verified | n/a |
| TcpTransport | sim-verified (loopback unit) | physical: pending |
| FakeDevice 480×320 baseline | sim-verified | n/a |
| CLI `mdc simulate` | sim-verified | n/a |
| Major version mismatch reject | sim-verified | n/a |
| Low max_chunk negotiation | sim-verified | n/a |
| InputEvent → ManagerEvent::Input | sim-verified | physical: pending |
| OTA command capability gate | sim-verified | physical: pending |
| Discovery (Static + Mock mDNS) | sim-verified | physical: pending |
| Real mDNS / DNS-SD parse | sim-verified (`mdns`, PTR/SRV/A + compression; loopback responder) | physical: pending |
| Provision / SimFlasher | sim-verified | physical: pending |
| Node NativeManager sim | sim-verified (JS facade + native tile/events; `.node` optional) | physical: pending |
| C ABI `mdc_c` | sim-verified (frame/tile/surface host tests) | physical: pending |
| Linux runtime framebuffer peer | sim-verified (`mdc_linux_peer` TCP) | physical: pending |
| `mdc linux-accept` 800×480 | sim-verified (Rust peer and C++ `mdc_linux_peer` `E_BASE`) | physical: pending |
| Displayed ACK releases the next frame | sim-verified (`received` keeps the held frame) | n/a |
| Geometry mismatch `E_DIM` | sim-verified (C++ peer rejects before commit) | physical: pending |
| Mid-session pairing revoke | sim-verified (next TCP frame rejected, pixels stay) | n/a |
| PTY serial HELLO | sim-verified (no USB device; non-candidate port waits 300s) | physical: pending |
| Serial log resync | sim-verified (noise then `MD`, bad checksum rejected) | n/a |
| Input queue overflow | sim-verified (moves coalesce; down/up/cancel kept and cancelled) | n/a |
| Control queue cap 32 | sim-verified (frames stay on the surface queue) | n/a |
| Benchmark displayed timing | sim-verified (monotonic transport/render/e2e; 30 FPS is not a promise) | physical: pending |
| Touch corners and two pointers | sim-verified | physical: pending |
| Chunked asset upload | sim-verified (abort clears bytes; `jpeg: false` falls back to RGB565) | n/a |
| Flash lease vs second open | sim-verified (fake port, no device node opened) | physical: pending |
| esptool runner + version lock | sim-verified (`FakeRunner`; pinned 4.7.0; no binary spawned) | physical: pending |
| Linux display backend | sim-verified (memory framebuffer or no session = 2; no DRM link) | physical: pending |
| Provision log redaction | sim-verified | n/a |
| Stale ACK after transport switch | sim-verified (previous request id cannot release the new frame) | n/a |
| Reserved byte and unknown type | sim-verified (rejected before payload allocation) | n/a |
| C++ incomplete frame / freeze | sim-verified (`E_CHUNK`, `E_INCOMPLETE`, `E_FROZEN` leave pixels unchanged) | physical: pending |
| Artifact signature | sim-verified (fake key, no port opened) | physical: pending |
| Short write resume | sim-verified (one frame, next frame stays queued off the wire) | n/a |
| Write without PONG | sim-verified (virtual heartbeat, no wall sleep) | n/a |
| Host copy budget | sim-verified (three copies, 1 MiB limit) | n/a |
| Simulated heap reserve | sim-verified (`min_internal_free` refuses a second framebuffer) | physical: pending |
| E-ink refresh busy | sim-verified (`E_BUSY`, `max_fps` 0, previous pixels kept) | physical: pending |
| Declared expansion guard | sim-verified (quota check before allocation, JPEG stays off) | n/a |
| Host copy budget on send | sim-verified (three copies released on displayed ACK) | n/a |
| Dual-surface copy budget | sim-verified (overlay refused, main frame kept) | n/a |
| C++ framebuffer reserve | sim-verified (second buffer refused, pixels unchanged) | physical: pending |
| Artifact signature on preflight | sim-verified (mismatch rejects, no port opened) | physical: pending |
| Non-zero header flags | sim-verified (rejected before payload allocation) | n/a |
| C++ OTA freeze and resume | sim-verified (`begin` freezes, `abort` resumes) | physical: pending |
| Replayed displayed ACK | sim-verified (pending frame launches once) | n/a |
| Input before the new handshake | sim-verified (dropped; later touch still arrives) | n/a |
| C ABI frame-copy budget | sim-verified (peak counts three copies; disconnect returns to zero) | n/a |
| OTA render freeze | sim-verified (`E_FROZEN` until abort/rollback) | physical: pending |
| dual-sim overlay surface | sim-verified | n/a |
| Touch rotation 0/90/180/270 | sim-verified | physical: pending |
| Heartbeat miss reconnect | sim-verified (3 misses, then full frame before tile) | n/a |
| WebSocket message limit | sim-verified | physical: pending |
| In-flight frame replacement | sim-verified (pending slot only) | n/a |
| `mdc-sim-peer --drop-next` | sim-verified | n/a |
| Scene capability bit | sim-verified (`send_scene` → Unsupported, no Scene packet) | physical: pending |
| Linux geometry update / no-display probe | sim-verified (`update_geometry`, `mdc_linux_probe`) | physical: pending |
| pi4-hdmi / waveshare-eink profiles | sim-verified (eink `color_fps: false`) | physical: pending |
| Electron demo IPC | sim-verified (preload + color-bar logic; native `.node` optional) | physical: pending |
| Swift `MdcSession` | sim-verified when `swiftc` links `libmdc_c` | physical: pending |
| Tauri command crate | sim-verified (`mdc-tauri-demo`, no second protocol) | physical: pending |
| MIRRORFRAME / MIRRORCTRL types | sim-verified (Rust + C++ shared vector, dispatcher gate) | physical: pending |
| Mirror capability fields | sim-verified (old caps decode disabled; negotiate clamps fps/mode) | physical: pending |
| Host shadow framebuffer | sim-verified (`mdc-mirror`, bounded, lease-guarded) | n/a |
| Mirror reference sink + queue | sim-verified (`PpmSink` encode; bounded latest-wins `PublishQueue`, pacing) | n/a |
| Dirty-region mirror | sim-verified (capability-gated; patches the base frame, misaligned/earlier drops) | n/a |
| Mirror event + stats | sim-verified (`ManagerEvent::Mirror` metadata + `mirror_stats()`, no polling) | n/a |
| Node mirror surface | sim-verified (`onMirrorFrame`/`startMirror`/`takeMirrorFrame`, JS mock test) | n/a |
| Electron mirror preview | sim-verified (`decodeMirrorFrame` + panel; `demo-logic.test.js`) | n/a |
| `mdc inspect` mirror/renderer | sim-verified (JSON includes mirror + renderer capability, no pixels) | physical: pending |
| OBS file sink / TCP server | sim-verified (`PpmDirectorySink` atomic + `MirrorServer` latest-wins) | n/a |
| `mdc mirror` command | sim-verified (drives a sim board, writes PPM / serves TCP) | physical: pending |
| Config capability + CONFIG | sim-verified (`Capabilities.config`, CBOR get/set, canonical vector) | physical: pending |
| Config host (core/sim/CLI/Node) | sim-verified (capability gate, validation, secret redaction) | physical: pending |
| Pixel baking (`mdc-bake`) | sim-verified (decode/resize guard, atlas pack, `mdc bake`) | n/a |
| Scene capability + SCENE | sim-verified (`Capabilities.scene`, node set/clear, vector) | physical: pending |
| Baked sprite package | sim-verified (`bake_to_package` + device grid recorded, capability gate) | physical: pending |
| Scene C ABI + Swift | sim-verified (`mdc_set_scene`/`clearScene`) | physical: pending |
| Device atlas animation | sim-verified (clip via `animation` key; clock-indexed frame) | physical: pending |
| Device-rendered mirror | sim-verified (running atlas frame emitted as `MIRRORFRAME`) | physical: pending |
| Scene geometry + rasterizer | sim-verified (rect fill, z order, clipping; `scene-render` PPM) | physical: pending |
| Model compiler / MDC IR | sim-verified (`mdc-model`, glTF/GLB + Live2D → IR → package) | n/a |
| Renderer capability on the wire | sim-verified (`Capabilities.renderer{}`, optional; Rust+C++ shared bytes) | physical: pending |
| Runtime package transfer | sim-verified (`PACKAGE` 0x32 begin/chunk/commit; `install_package`→FakeDevice ready) | physical: pending |
| C++ runtime package + mirror | sim-verified (`package_test.cpp`: checksum/magic accept + `emit_mirror_frame`) | physical: pending |
| glTF/GLB bounds + image probe | sim-verified (accessor bufferView overrun rejected; PNG/JPEG size from header) | n/a |
| Target backends (esp32-plane / linux-mesh) | sim-verified (mesh↔sprite by capability; no board-id branch) | n/a |
| Board family skeletons (arduino/rockchip/nvidia) | architecture (`physical: pending`) | physical: pending |
| Transport scope (BLE/USB bulk/PCIe) | architecture (reserved, not implemented) | physical: pending |
| Reserved diagnostics (N303) | architecture (`0x60..0x63` parse; payload not implemented) | physical: pending |
| Reboot semantics (N304) | sim-verified (`boot_id`/`run_id`; reboot drops the previous run) | physical: pending |
| Host frame scheduler (N305/N306) | sim-verified (`mdc-schedule` pacing + bandwidth convergence test) | n/a |
| PixelFormat enum (N307) | sim-verified (protocol → CLI/Node/C/Swift; non-RGB565 rejected) | n/a |
| Surface display profile (N308) | sim-verified (`preferred_tile_size` + adaptive host tile cut) | physical: pending |
| Mirror read source (N309) | architecture (decision + hardware checklist; unmeasured) | physical: pending |
| Submodule contract (N310) | docs (`docs/submodule-contract.md`) | n/a |
| Live OBS HTTP stream (N313) | sim-verified (`/stream` multipart; loopback fixture + unit test) | physical: pending |
| `mdc mirror` pacing (N314) | sim-verified (`mdc-schedule` budget; drop stats) | n/a |
| Device mesh-package clip select (N315) | sim-verified (`config animation <index\|name>`; fixture) | physical: pending |
| Diagnostics channel (N316) | sim-verified (`Metric`/`BootInfo` + `take_diagnostics`; `mdc diag`) | physical: pending |
| Inspector JSONL series (N317) | sim-verified (`mdc-inspector --jsonl`) | n/a |
| Scene text wrap (N318) | sim-verified (word wrap + `\n`, uniform glyph scale) | physical: pending |
| C/Swift diagnostics (N319) | sim-verified (C ABI test + Swift smoke) | n/a |
| Electron live stream helpers (N320) | sim-verified (`demo-logic.test.js`) | n/a |
| Embedded glTF textures (N323/N324) | sim-verified (bytes retained; `mdc package` auto-uses them) | n/a |
| Painter depth ordering (N325) | sim-verified (near mesh overlays far) | n/a |
| glTF NORMAL + directional shading (N326/N327) | sim-verified (import f32/i8; scene shading; flat +z unchanged) | physical: pending |
| PNG delivery (N328) | sim-verified (`encode_png` decode round-trip; `/frame.png`) | n/a |
| Opt-in Core pacing (N329) | sim-verified (`Session::set_frame_pacing` -> `Throttled`) | n/a |
| Mesh clip cross-fade (N330) | sim-verified (blend window on clip change) | physical: pending |
| C++ diagnostics encoders (N331) | sim-verified (bytes match the Rust vectors) | physical: pending |
| Material BLEND alpha (N333) | sim-verified (source-over in host + device) | n/a |
| Texture wrap mode (N334) | sim-verified (glTF sampler → clamp/repeat) | n/a |
| Telemetry events (N335) | sim-verified (structured envelope on the diagnostics channel) | physical: pending |
| JPEG delivery (N336/N337) | sim-verified (`encode_jpeg` decode round-trip; `/frame.jpg`, `/stream.jpg`) | n/a |
| Scene node opacity (N338) | sim-verified (source-over alpha) | physical: pending |
| `mdc provision` (N339) | sim-verified (preflight → install → readback → rollback) | physical: pending |
| Node `connectWebSocket` (N340) | sim-verified (native smoke vs `mdc-sim-peer --ws`) | physical: pending |
| Metallic/roughness shading (N343) | sim-verified (roughness flattens shading) | n/a |
| 8-bit texture alpha (N344) | sim-verified (soft `BLEND` edges) | n/a |
| Multi-clip atlas bake (N345) | sim-verified (`bake-mesh --all-animations`) | physical: pending |
| JPEG quality / HTTP scale (N346) | sim-verified (`--quality`, `?scale=`/`?quality=`) | n/a |
| Device filtering config (N347) | sim-verified (`config filtering linear`) | physical: pending |
| Scene gradient (N348) | sim-verified (vertical linear gradient) | physical: pending |
| Node diagnostics (N349) | sim-verified (native smoke) | n/a |
| `mdc ota` (N350) | sim-verified (begin/chunk/confirm + readback/rollback) | physical: pending |
| Inspector surface profile/counters (N351) | sim-verified (JSON fields) | n/a |
| Multi-material textures (N353/N358) | sim-verified (`texture_blobs`; per-mesh sampling host + device) | physical: pending |
| `mdc ota` over TCP (N354) | sim-verified (TCP branch; in-process peer) | physical: pending |
| Scene rounded-rect (N355) | sim-verified (corner clip) | physical: pending |
| Stream scale (N356) | sim-verified (`/stream?scale=` downscales parts) | n/a |
| Telemetry events (N357) | sim-verified (`ManagerEvent::Telemetry` + C/Node mapping) | physical: pending |
| `/health` endpoint (N361) | sim-verified (JSON counters) | n/a |
| Emissive/unlit materials (N363) | sim-verified (glTF import; unlit skips shading) | physical: pending |
| `COLOR_0` vertex colours (N364) | sim-verified (interpolated on host + device) | physical: pending |
| Skinned normals (N365) | sim-verified (`skin_normals` follows the pose) | physical: pending |
| C/Swift `connectWebSocket` (N366) | sim-verified (in-process WS peer) | physical: pending |
| Crash report `0x62` (N367) | sim-verified (CBOR `CrashReport`; C/Node mapping) | physical: pending |
| Scene `clip` node (N368) | sim-verified (scissor rect, nesting) | physical: pending |
| On-disk asset store (N369) | sim-verified (`mdc asset` LRU + quota soak) | n/a |
| Host RSS sampling (N370) | sim-verified (`--rss`; `rss_bytes` series) | n/a |
| `KHR_texture_transform` (N373) | sim-verified (UV offset/rotation/scale) | physical: pending |
| `KHR_materials_emissive_strength` (N374) | sim-verified (scales emissive) | physical: pending |
| Default-scene selection (N375) | sim-verified (unreferenced nodes dropped) | physical: pending |
| Sparse accessors (N376) | sim-verified (de-sparsed) | physical: pending |
| Interleaved accessors (N377) | sim-verified (`byteStride` de-stride) | physical: pending |
| Occlusion texture (N378) | sim-verified (per-triangle AO) | physical: pending |
| Normal mapping (N379) | sim-verified (per-pixel `TANGENT`+`normalTexture`) | physical: pending |
| Atlas packing (N380) | sim-verified (`pack_material_atlas`; UV remap) | physical: pending |
| `texCoord`/`TEXCOORD_1` (N383) | sim-verified (material UV set) | physical: pending |
| `JOINTS_1`/`WEIGHTS_1` (N384) | sim-verified (eight influences) | physical: pending |
| `KHR_lights_punctual` (N385) | sim-verified (directional key light) | physical: pending |
| Smooth shading mode (N386) | sim-verified (per-pixel Gouraud) | n/a |
| Specular shading mode (N387) | sim-verified (Blinn-Phong) | n/a |
| `mdc model-info` (N388) | sim-verified (JSON import report) | n/a |
| `mdc diff` (N389) | sim-verified (manifest + structure diff) | n/a |
| `KHR_mesh_quantization` (N390) | sim-verified (integer attributes decode) | physical: pending |
| `emissiveTexture` (N393) | sim-verified (additive emissive) | physical: pending |
| Per-slot UV transforms (N395) | sim-verified (base/normal/emissive/PBR) | physical: pending |
| `metallicRoughnessTexture` (N394) | sim-verified (per-teixel roughness) | physical: pending |
| Atlas mip chain (N396) | sim-verified (`bake_mipmaps`) | n/a |
| `KHR_materials_specular`/`ior` (N397) | sim-verified (scales the highlight) | physical: pending |
| Session counters (N398) | sim-verified (`Session::stats`) | n/a |
| `mdc validate` (N399) | sim-verified (reference checks; exit code) | n/a |
| Host→device `sendInput` (N400) | sim-verified (C ABI + Node; `touch` gated) | physical: pending |
| Per-slot `texCoord` (N403) | sim-verified (base vs secondary UV sets) | physical: pending |
| Bayer dithering (N404) | sim-verified (`bayer_dither_rgb565`) | n/a |
| Palette quantization (N405) | sim-verified (`quantize_palette`, ≤256) | n/a |
| Per-pixel occlusion (N406) | sim-verified (AO sampled per pixel) | physical: pending |
| sRGB bilinear filtering (N407) | sim-verified (`with_srgb`) | n/a |
| `mdc verify` (N408) | sim-verified (checksum + size) | n/a |
| Swift `sendInput` (N409) | sim-verified (C ABI wrapper + smoke) | physical: pending |
| RSS-delta sampler (N410) | sim-verified (`--rss-delta`) | n/a |
| `pbrSpecularGlossiness` (N413) | sim-verified (converted to metallic-roughness) | physical: pending |
| Clearcoat (N414) | sim-verified (second specular lobe) | physical: pending |
| Indexed palette textures (N415) | sim-verified (`TextureBlob.palette`/`indices`) | physical: pending |
| Texture dithering (N416) | sim-verified (`dither_texture_blob`) | n/a |
| `mdc scene-lint` (N417) | sim-verified (reference + limit checks) | n/a |
| Scene hit-testing (N418) | sim-verified (`InputEvent.node`) | physical: pending |
| OTA phase checkpoint (N419) | sim-verified (`Session::ota_phase`) | physical: pending |
| `KHR_materials_variants` (N420) | sim-verified (`Model.variants`) | n/a |
| Generated tangents (N423) | sim-verified (UV-gradient tangents) | physical: pending |
| Skinned tangents (N424) | sim-verified (rotated with the pose) | physical: pending |
| `KHR_materials_sheen` (N425) | sim-verified (rim highlight) | physical: pending |
| `KHR_materials_transmission` (N426) | sim-verified (blend alpha) | physical: pending |
| C hit-node out-param (N427) | sim-verified (`poll_event_with_node`) | physical: pending |
| `package --tangents` (N428) | sim-verified (fills missing tangents) | n/a |
| Clip by name (N429) | sim-verified (`render_package_scene_clip_named`) | n/a |
| Node `connectSerial` (N430) | sim-verified (rejects an unusable path) | physical: pending |
| Variant mappings (N433) | sim-verified (`package --variant`) | n/a |
| `KHR_materials_volume` (N434) | sim-verified (thickness fades) | physical: pending |
| `KHR_materials_iridescence` (N435) | sim-verified (thin-film highlight) | physical: pending |
| Extended session stats (N436) | sim-verified (`bytes_received`, `dropped_frames`) | n/a |
| `devices --compact` (N437) | sim-verified (JSONL) | n/a |
| Package mip chain (N438) | sim-verified (`AtlasBlob.mips`) | physical: pending |
| `render-package --mip` (N439) | sim-verified (stored level) | n/a |
| C `session_stats` (N440) | sim-verified (out-params) | n/a |
| Texture-transform UV override (N443) | sim-verified (`texCoord` wins) | physical: pending |
| `KHR_materials_anisotropy` (N444) | sim-verified (brushed highlight) | physical: pending |
| `EXT_texture_webp` (N445) | sim-verified (source resolve + WebP decode) | physical: pending |
| Full session stats (N446) | sim-verified (`packages_sent`, `mirror_frames`, `pacing_dropped`) | n/a |
| Swift `stats()` (N447) | sim-verified (wrapper + smoke) | n/a |
| `mirror --scale` (N448) | sim-verified (box downscale) | n/a |
| glTF cameras (N449) | sim-verified (metadata) | n/a |
| Scene `visible` (N450) | sim-verified (draw + hit-test skipped) | physical: pending |
| PNG live stream (N453) | sim-verified (`/stream.png` parts) | n/a |
| Mesh smooth mode (N454) | sim-verified (`render_package_mesh_mode`) | n/a |
| Node `stats()` (N455) | sim-verified (session counters) | n/a |
| `KHR_materials_dispersion` (N456) | sim-verified (wider hue split) | physical: pending |
| Mirror surface guard (N457) | sim-verified (non-primary refused) | physical: pending |
| `package --report` (N458) | sim-verified (extended summary) | n/a |
| Frame rotate (N459) | sim-verified (`?rotate=90|180|270`) | n/a |
| Inspector session stats (N460) | sim-verified (JSONL field) | n/a |
| Scene animation (N463) | sim-verified (opacity/rect keyframes) | physical: pending |
| Gestures (N464) | sim-verified (tap/long-press/drag) | physical: pending |
| Overlay rendering (N465) | sim-verified (`render_overlay`) | physical: pending |
| `validate --strict` (N466) | sim-verified (warnings fail) | n/a |
| Disk chunked uploads (N467) | sim-verified (spool + rename) | n/a |
| Linear mips (N468) | sim-verified (`bake_mipmaps_with`) | n/a |
| Frame crop (N469) | sim-verified (`?crop=`) | n/a |
| Numbered frames (N470) | sim-verified (`mirror --sequence`) | n/a |
| Gesture events (N473/N474) | sim-verified (`ManagerEvent::Gesture`, C/Node) | physical: pending |
| Scene rotate (N475) | sim-verified (axis swap + gradient) | physical: pending |
| Lowercase text (N476) | sim-verified (a–z glyphs) | physical: pending |
| Animation-key budget (N477) | sim-verified (`max_anim_keys` + lint/gate) | n/a |
| Resumable uploads (N478) | sim-verified (`begin_named`/`resume`) | n/a |
| `scene-render --time` (N479) | sim-verified (animated frame) | n/a |
| Additive blend (N480) | sim-verified (saturating add) | physical: pending |
| Gesture hit node (N493) | sim-verified (core + C + Node) | physical: pending |
| Text wrap modes (N495) | sim-verified (word/char/none) | physical: pending |
| Dashed lines (N497) | sim-verified (dash length) | physical: pending |
| Mirror flip (N486/N496) | sim-verified (`?flip`, `--flip`) | n/a |
| C gesture config (N498) | sim-verified (`set_gesture_config`) | n/a |
| e-ink ghost fade (N499) | sim-verified (partial vs full) | physical: pending |
| WebSocket mirror (N500) | sim-verified (`/ws`, RFC 6455 handshake) | n/a |
| Reserved extensions (N503) | sim-verified (draco/basisu rejected; meshopt decoded) | n/a |
| Mip-level selection (N504) | sim-verified (`--mip-level`, helpers) | physical: pending |
| Multi-stop gradients (N505) | sim-verified (stop list) | physical: pending |
| Tile counters (N506) | sim-verified (core + C) | n/a |
| WS control frames (N507) | sim-verified (ping/pong/close) | n/a |
| Swift bounded mirror (N508) | sim-verified (bufferingNewest) | physical: pending |
| Heap probe (N509) | sim-verified (peak growth) | n/a |
| `mdc record` (N510) | sim-verified (sequence + manifest) | n/a |
| GPU instancing (N513) | sim-verified (expansion + install) | physical: pending |
| Automatic mip selection (N514) | sim-verified (target-sized level) | physical: pending |
| Radial gradients (N515) | sim-verified (centre bright) | physical: pending |
| Dirty-tile planning (N516) | sim-verified (coalesced rects) | n/a |
| WS heartbeat (N517) | sim-verified (ping + silent drop) | n/a |
| C mirror parity (N518) | sim-verified (surface id, skips) | n/a |
| Heap timeline (N519) | sim-verified (per-tick peak) | n/a |
| `record --play` (N520) | sim-verified (manifest check) | n/a |
| WebP probing (N523) | sim-verified (VP8/VP8L/VP8X) | n/a |
| Drawn-vertex budget (N524) | sim-verified (trim + install) | physical: pending |
| Gradient angle (N525) | sim-verified (0/90/180/270 + diagonal) | physical: pending |
| Dirty tracker (N526) | sim-verified (accumulate + escalate) | n/a |
| WS fragmentation (N527) | sim-verified (continuation frames) | n/a |
| Node mirror parity (N528) | sim-verified (bounded take + skips) | n/a |
| Inspector alerts (N529) | sim-verified (threshold + exit 2) | n/a |
| `record --render` (N530) | sim-verified (decode + replay) | physical: pending |
| meshopt decode (N533/N534) | sim-verified (reference golden vectors) | physical: pending |
| Radial focus (N535) | sim-verified (off-centre centre) | physical: pending |
| Tile reconciliation (N536) | sim-verified (host vs device counts) | n/a |
| WS fragment floor (N537) | sim-verified (floor + heartbeat order) | n/a |
| C mirror mode (N538) | sim-verified (`full-frame`/`dirty-region`) | n/a |
| JSONL verdict (N539) | sim-verified (one-line alert) | n/a |
| Paced replay (N540) | sim-verified (`--fps` timing) | n/a |
| Swift gestures (N483) | sim-verified (`isGesture`/`gestureKind` + smoke) | physical: pending |
| Text align (N484) | sim-verified (left/center/right) | physical: pending |
| Scene z-order (N485) | sim-verified (draw + hit order) | physical: pending |
| Frame flip (N486) | sim-verified (`?flip=h|v`) | n/a |
| `asset verify` (N487) | sim-verified (content re-hash) | n/a |
| Gesture tuning (N488) | sim-verified (`set_gesture_config`) | n/a |
| e-ink ghosting (N489) | sim-verified (partial refresh) | physical: pending |
| Texture mip chains (N490) | sim-verified (`TextureBlob.mips`) | physical: pending |
| meshopt fuzz hardening (N543) | sim-verified (no panic on malformed) | n/a |
| KTX2 header (N544) | sim-verified (size/format; no transcode) | physical: pending |
| Geometry compaction (N545) | sim-verified (lossless merge + install) | physical: pending |
| Focus animation (N546) | sim-verified (centre moves over time) | physical: pending |
| Tile snapshot (N547) | sim-verified (device metric folded in) | n/a |
| WS client fragments (N548) | sim-verified (continuation reassembly) | n/a |
| Swift mirror mode (N549) | sim-verified (mode + skips parity) | physical: pending |
| JSONL verdict tail (N550) | sim-verified (last line) | n/a |
| meshopt fuzz corpus (N553) | sim-verified (mutation + truncation) | n/a |
| KTX2 container dims (N554) | sim-verified (layer/face/depth/mips) | n/a |
| Index-order optimization (N555) | sim-verified (fewer cache misses) | physical: pending |
| Image scaling (N556) | sim-verified (nearest + bilinear) | physical: pending |
| Tile threshold (N557) | sim-verified (full-frame barrier) | n/a |
| WS compression bit (N558) | sim-verified (counted, ignored) | n/a |
| Node mirror mode (N559) | sim-verified (`normalizeMirrorMode` + `mirrorMode`) | n/a |
| JSONL file (N560) | sim-verified (`--out`) | n/a |
| meshopt branch corpus (N563) | sim-verified (guard inventory) | n/a |
| Combined optimization (N564) | sim-verified (compact then reorder) | physical: pending |
| Image flip/rotate (N565) | sim-verified (flip-h + 180°) | physical: pending |
| Tile convergence (N566) | sim-verified (full frame clears) | n/a |
| WS extension rejection (N567) | sim-verified (1002 close) | n/a |
| C/Swift mirror mode (N568) | sim-verified (`mdc_mirror_mode` + Swift) | n/a |
| JSONL rotation (N569) | sim-verified (`--append`/`--max-bytes`) | n/a |
| KTX2 metadata in IR (N570) | sim-verified (`ktx2_slices`) | n/a |
| meshopt LEB128 corpus (N573) | sim-verified (overflow + half-packet) | n/a |
| Adaptive cache (N574) | sim-verified (mesh-scaled) | physical: pending |
| Image `fit` (N575) | sim-verified (letterbox) | physical: pending |
| Tile reconciliation (N576) | sim-verified (C + Node) | n/a |
| WS decline log (N577) | sim-verified (`ws_log`) | n/a |
| Swift mirror parity (N578) | sim-verified (surface take + skips) | physical: pending |
| JSONL dir rotation (N579) | sim-verified (`--keep`) | n/a |
| KTX2 degradation doc (N580) | documented | n/a |
| meshopt round-trip (N583) | sim-verified (`decode(encode(v))`) | n/a |
| Cache tiers (N584) | sim-verified (monotonic) | physical: pending |
| Image fit align (N585) | sim-verified (left/centre/right) | physical: pending |
| Tile threshold config (N586) | sim-verified (negotiated) | n/a |
| WS log summary (N587) | sim-verified (`ws_log_last`) | n/a |
| Node mirror/tile smoke (N588) | sim-verified (real `.node`) | n/a |
| JSONL directory archive (N589) | sim-verified (`--out DIR`) | n/a |
| KTX2 verify (N590) | sim-verified (`ktx2_ok` + diff) | n/a |
| meshopt vertex round-trip (N593) | sim-verified (`decode(encode(v))`) | n/a |
| Cache mode report (N594) | sim-verified (`off`/`adaptive`/`fixed`) | n/a |
| Image fit composition (N595) | sim-verified (fit + flip/align) | physical: pending |
| Board config defaults (N596) | sim-verified (per-board) | n/a |
| WS log redaction (N597) | sim-verified (token only) | n/a |
| C mirror/tile smoke (N598) | sim-verified (aligned with Node) | n/a |
| Archive manifest (N599) | sim-verified (`manifest.json`) | n/a |
| Model KTX2 warning (N600) | sim-verified (`validate`) | n/a |
| Filter round-trip (N603) | sim-verified (decode == filter(raw)) | n/a |
| Verify locality (N604) | sim-verified (`cache_misses`) | n/a |
| Fit quarter-turn (N605) | sim-verified (box swapped) | physical: pending |
| Board defaults inspector (N606) | sim-verified (`board_config_defaults`) | n/a |
| Structured WS log (N607) | sim-verified (`kind`/`detail`) | n/a |
| Node merged smoke (N608) | sim-verified (full field set) | n/a |
| Archive history (N609) | sim-verified (`rotated`/`generatedAt`) | n/a |
| model-info KTX2 flag (N610) | sim-verified (`ktx2SizeOk`) | n/a |

## G03 sim exit

Host Core + FakeDevice path demonstrates HELLO → Capabilities → Frame → Ack without physical hardware. Marked **sim-verified**; G03 physical exit remains open.
