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

## G03 sim exit

Host Core + FakeDevice path demonstrates HELLO → Capabilities → Frame → Ack without physical hardware. Marked **sim-verified**; G03 physical exit remains open.
