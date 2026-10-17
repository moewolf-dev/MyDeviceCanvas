# Implementation Status

Track items with three states: `architecture`, `sim-verified`, `physical: pending`.
Do not mark physical verification from host-only or simulator tests.

> **Submodule consumers:** this tree is a **host + simulator contract**. The
> physical exit tasks **D05 / G03 / H06 are unchecked**; `sim-verified` is not
> `hardware-usable`. See [`submodule-contract.md`](submodule-contract.md).

## Wave progress

| Wave | Focus | State |
|---|---|---|
| 0 | Repo baseline, sim hardware baseline, quality entry | sim-verified (architecture complete) |
| 1 | Protocol completeness + simulator peer | sim-verified |
| 2 | Transport / Session / Manager wiring | sim-verified |
| 3 | Board HAL + Runtime + SimDisplay | sim-verified (C++ host tests) |
| 4 | CLI / Node SDK / G03 sim exit | sim-verified |
| 5 | Discovery / Provision / Touch / OTA | sim-verified |
| 6 | C/Swift / cache / Inspector | sim-verified (architecture + host tests) |
| 7 | Linux Runtime / Scene / 1.0 candidate docs | sim-verified architecture complete |
| 8 | H01 pairing + I03 endpoint switch / full-frame barrier | sim-verified |
| 9 | Network display gate (TCP/WS) + IDF panel bring-up stages | architecture / sim-verified |
| 10 | Linux virt C++ TCP peer + CBOR Capabilities/Ack alignment | sim-verified |
| 11 | Node/C SDK tile + event drain alignment | sim-verified |
| 12 | Electron demo, Swift wrapper, Tauri commands, Linux geometry, Scene refusal | sim-verified |
| 13 | OTA freeze, dual surface, rotation, heartbeat reconnect, linux-accept | sim-verified |
| 14 | Copy budget, signature preflight, flags, OTA resume, stale input | sim-verified |
| 15 | N01–N07: render topology, reference SDKs, mirror upstream, shadow buffer, model IR, board families, transport scope | architecture / sim-verified |
| 16 | N08: real glTF/GLB importer (bounds + image probe), Live2D model3 importer, second target backend (linux-mesh) | sim-verified |
| 17 | N09–N18: skins/VRM + motion3, atlas baker, mirror sinks/queue + dirty-region, board grounding, real mDNS, renderer capability, esptool runner | sim-verified |
| 18 | N19–N28: runtime package container/upload/message, install end-to-end, mirror event + Node/Electron surface, board renderer section, inspector metrics | sim-verified |
| 19 | N29–N38: OBS file/TCP delivery + `mdc mirror`, `mdc package`/`install-package`, C++ package+mirror, C/Swift mirror, memory/compat tests | sim-verified |
| 20 | N39–N48: configuration host (CONFIG + core/sim/CLI/Node), live mDNS query, esptool version lock, model optimizer, VRM/Live2D metadata | sim-verified |
| 21 | N49–N58: pixel baking (`mdc-bake` + `mdc bake`), Scene contract + set/clear, Electron config panel, `inspect --watch`, bake soak | sim-verified |
| 22 | N59–N68: baked sprite package delivery, Scene C/Swift, inspector config/scene, baked/scene fixtures + soak, docs | sim-verified |
| 23 | N69–N78: atlas clips, device animation runtime, device-rendered mirror, Node/Electron animation, fixtures + soak, docs | sim-verified |
| 24 | N79–N88: scene geometry + device rasterizer, scene mirror, `scene-render`, Node/Electron scene preview, fixtures + soak, docs | sim-verified |
| 25 | N89–N98: glTF preview pixels, triangle raster, scene text/image blit | sim-verified |
| 26 | N99–N108: mesh preview blit, scene sprite, atlas fade-in | sim-verified |
| 27 | Indexed mesh preview, fade-out, 5×7 A–Z font | sim-verified |
| 28 | `bake_displaced_mesh`: per-frame triangle motion, not a cycled image | sim-verified |
| 29 | N109–N112: glTF skin/TRS/animation import + `mdc-bake` linear blend skinning bake + `mdc bake-mesh` | sim-verified |
| 30 | N113–N122: TEXCOORD_0 import, texture sampling, textured skinned bake, CLI `--texture`, punctuation font, padded-atlas gate fix | sim-verified |
| 31 | N123–N132: glTF morph targets + weights channels, `apply_morphs` before skinning, scaled device text, animation loop metric | sim-verified |
| 32 | N133–N142: offline `render_frame_at` + `mdc render-mesh`/`render-package`, `#RGB`/named scene colours, scene group transforms, deterministic scene clear | sim-verified |
| 33 | N143–N152: BMP encoder + `HttpMirrorServer` (OBS browser source), `mdc mirror --http/--format`, obs-bridge docs, HTTP compat + soak | sim-verified |
| 34 | N153–N162: animation cross-fade between clips, `stop_animation`, scene `circle`/`line` primitives, fixtures + soak + docs | sim-verified |
| 35 | N163–N172: bounded full-mesh geometry (IR + glTF), skinned aspect-fit geometry render, device geometry preview, fixtures + soak | sim-verified |
| 36 | N173–N182: material `baseColorFactor` colours, `DevicePackage.texture` blob, `mdc package --texture`, device textured geometry render, fixtures + soak | sim-verified |
| 37 | N183–N192: multi-mesh scene render, `bake_scene_*`, `render_package_mesh`, CLI whole-scene render, fixtures + soak | sim-verified |
| 38 | N193–N202: `render_package_scene`, device renders an animated mesh package scene through the mirror, fixtures + soak | sim-verified |
| 39 | N203–N212: texture `Filter` (nearest/bilinear), material-colour tint (bake + device), `render-mesh --filter`, fixtures + soak | sim-verified |
| 40 | N213–N222: Live2D `motion3.json` curve parse + sample, `mdc motion`, docs, fixtures + soak | sim-verified |
| 41 | N223–N232: geometry morph targets (blend shapes) import + render (host + device mirror), fixtures + soak | sim-verified |
| 42 | N233–N242: Node SDK `installPackage`/`packageReady`, native smoke + JS mock tests, Electron helper, `docs/node-sdk.md` | sim-verified |
| 43 | N243–N252: C ABI + Swift `installPackage`/`packageReady`/`getConfig`/`setConfig`, C ABI tests + Swift smoke, docs | sim-verified |
| 44 | N253–N262: `CUBICSPLINE` Hermite sampling + rotation normalize, `sample_channel_looped`, fixtures + soak | sim-verified |
| 45 | N263–N272: device dirty-region mirror emission + `Session` patch, capability/mode plumbing, fixtures + soak | sim-verified |
| 46 | N273–N282: multi-primitive geometry merge (`MeshGeometry::merge`), `Mesh.primitives`, fixtures + soak | sim-verified |
| 47 | N283–N292: `alphaMode`/`alphaCutoff` import, 1-bit texture alpha, masked raster cutout, CLI decode, fixtures + docs | sim-verified |
| 48 | N293–N302: `TextureBlob.alpha` + device/packaged texture cutout, `bake_texture_blob` alpha, fixtures | sim-verified |
| 49 | N303–N312: reserved diagnostics + `boot_id`/`run_id` reboot detection, `mdc-schedule` pacing, `PixelFormat` enum, surface display profile, mirror read-source + submodule contract docs | sim-verified |
| 50 | N313–N322: live multipart OBS stream, CLI pacing, device mesh-package clip selection, diagnostics channel + `mdc diag`/JSONL inspector, scene text wrap, C/Swift diagnostics, demo stream helpers, fixtures | sim-verified |
| 51 | N323–N332: glTF embedded textures + auto-use, NORMAL import + directional shading, painter depth order, PNG delivery, opt-in Core pacing, mesh clip cross-fade, C++ diagnostics encoders | sim-verified |
| 52 | N333–N342: BLEND alpha, texture wrap mode, Telemetry events, JPEG encoder + `/frame.jpg` + `/stream.jpg`, scene opacity, `mdc provision`, Node `connectWebSocket`, demo helpers | sim-verified |
| 53 | N343–N352: metallic/roughness shading, 8-bit texture alpha, multi-clip atlas bake, JPEG quality + HTTP scale, device filtering config, scene gradient, Node diagnostics, `mdc ota`, inspector profile/counters | sim-verified |
| 54 | N353–N362: multi-material textures, OTA over TCP, scene rounded-rect, stream scale, telemetry events, multi-texture fixtures/soak, `/health` | sim-verified |
| 55 | N363–N372: emissive/unlit, `COLOR_0` vertex colours, skinned normals, C/Swift `connectWebSocket`, `Crash` report, scene `clip`, on-disk asset store, host RSS sampling | sim-verified |
| 56 | N373–N382: `KHR_texture_transform`, `KHR_materials_emissive_strength`, default-scene selection, sparse + interleaved accessors, occlusion texture, normal mapping, texture atlas packing | sim-verified |
| 57 | N383–N392: `texCoord`/`TEXCOORD_1`, `JOINTS_1`/`WEIGHTS_1`, `KHR_lights_punctual`, smooth/specular shading modes, `KHR_mesh_quantization`, `mdc model-info`/`diff` | sim-verified |
| 58 | N393–N402: `emissiveTexture`, per-slot `KHR_texture_transform`, `metallicRoughnessTexture`, atlas mip chain, `KHR_materials_specular`/`ior`, session counters, `mdc validate`, host→device `sendInput` | sim-verified |
| 59 | N403–N412: per-slot `texCoord`, Bayer dithering, palette quantization, per-pixel occlusion, sRGB filtering, `mdc verify`, Swift `sendInput`, RSS-delta sampler | sim-verified |
| 60 | N413–N422: `pbrSpecularGlossiness`, clearcoat, indexed palette textures, texture dithering, `scene-lint`, scene hit-testing, OTA phase checkpoint, `KHR_materials_variants` | sim-verified |

Waves **3–14** are marked **sim-verified architecture complete**: host FakeDevice / MemoryLink / C++ SimDisplay / `mdc_linux_peer` / SDK paths exercise the design. Physical board work remains `physical: pending`. D05, G03, and H06 stay unchecked until a physical board is measured.

## Honesty rules

- Skeleton code is `architecture`, not complete.
- `cargo test` host passes are not ESP-IDF or physical proof.
- Simulator FPS / heap numbers go in `docs/hardware-validation/*-sim.md`.
- Physical board docs stay `physical: pending` until measured on hardware.
