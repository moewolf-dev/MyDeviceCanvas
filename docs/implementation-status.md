# Implementation Status

Track items with three states: `architecture`, `sim-verified`, `physical: pending`.
Do not mark physical verification from host-only or simulator tests.

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

Waves **3–14** are marked **sim-verified architecture complete**: host FakeDevice / MemoryLink / C++ SimDisplay / `mdc_linux_peer` / SDK paths exercise the design. Physical board work remains `physical: pending`. D05, G03, and H06 stay unchecked until a physical board is measured.

## Honesty rules

- Skeleton code is `architecture`, not complete.
- `cargo test` host passes are not ESP-IDF or physical proof.
- Simulator FPS / heap numbers go in `docs/hardware-validation/*-sim.md`.
- Physical board docs stay `physical: pending` until measured on hardware.
