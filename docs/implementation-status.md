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
| 61 | N423–N432: generated/skinned tangents, `KHR_materials_sheen`, `KHR_materials_transmission`, C hit-node out-param, `package --tangents`, clip-by-name, Node `connectSerial` | sim-verified |
| 62 | N433–N442: variant mappings + `--variant`, `KHR_materials_volume`, `KHR_materials_iridescence`, extended session stats, `devices --compact`, package mip chain, `render-package --mip`, C `session_stats` | sim-verified |
| 63 | N443–N452: texture-transform UV override, `KHR_materials_anisotropy`, `EXT_texture_webp`, full session stats, Swift stats, `mirror --scale`, glTF cameras, scene `visible` | sim-verified |
| 64 | N453–N462: PNG live stream, mesh smooth mode, Node stats, `KHR_materials_dispersion`, mirror surface guard, `package --report`, frame rotate, inspector session stats | sim-verified |
| 65 | N463–N472: keyframed scene animation, gesture tracker, overlay rendering, `validate --strict`, disk chunked uploads, linear mips, frame crop, numbered mirror frames | sim-verified |
| 66 | N473–N482: gesture events (C/Node), scene rotate + additive blend, lowercase glyphs, animation-key budget, resumable uploads, `scene-render --time` | sim-verified |
| 67 | N483–N492: Swift gestures, text align, scene z-order, frame flip, `asset verify`, gesture tuning, e-ink ghosting, texture mip chains | sim-verified |
| 68 | N493–N502: gesture nodes, text wrap/punctuation, dashed lines, mirror flip, C gesture config, e-ink ghost fade, WebSocket mirror `/ws` | sim-verified |
| 69 | N503–N512: reserved-extension diagnostics, mip-level selection, multi-stop gradients, tile counters, WS control frames, Swift bounded mirror, heap probe, `mdc record` | sim-verified |
| 70 | N513–N522: GPU instancing, automatic mip selection, radial gradients, dirty-tile planning, WS heartbeat, C mirror parity, heap timeline, `record --play` | sim-verified |
| 71 | N523–N532: WebP probing, drawn-vertex budget, gradient angle, dirty tracker, WS fragmentation, Node mirror parity, inspector alerts, `record --render` | sim-verified |
| 72 | N533–N542: meshopt decoding (vertex v0/v1, index v0/v1, sequences, filters), radial focus, tile reconciliation, WS fragment floor, C mirror mode, JSONL verdicts, paced replay | sim-verified |
| 73 | N543–N552: meshopt fuzz/bounds hardening, KTX2 header reader, lossless geometry compaction, gradient focus animation, tile stats snapshot, WS client fragment reassembly, Swift mirror mode parity, JSONL verdict tail | sim-verified |
| 74 | N553–N562: meshopt fuzz corpus, KTX2 container dims, cache-friendly index order, scene image scaling, tile threshold barrier, WS compression-bit handling, Node mirror mode, JSONL file output | sim-verified |
| 75 | N563–N572: meshopt branch coverage + guard corpus, combined geometry optimization, image flip/rotate, tile-threshold convergence, WS extension rejection (1002), C/Swift mirror mode, JSONL append + rotation, KTX2 metadata in IR | sim-verified |
| 76 | N573–N582: meshopt LEB128/half-packet corpus, adaptive vertex cache, image `fit`, tile reconciliation to C/Node, WS decline log, Swift mirror parity recheck, JSONL directory rotation, KTX2 degradation docs | sim-verified |
| 77 | N583–N592: meshopt index-sequence round-trip, cache tier table, image `fit` align, config-negotiated tile threshold, `/health` WS log summary, Node native mirror/tile smoke, JSONL directory archive, KTX2 verify/diff consistency | sim-verified |
| 78 | N593–N602: meshopt vertex-buffer round-trip, cache-mode report, image `fit` composition, per-board config defaults, WS log redaction, C mirror/tile smoke, archive manifest, model-level KTX2 size warning | sim-verified |
| 79 | N603–N612: meshopt filter round-trip, verify/diff locality, image `fit` quarter-turn, inspector board config defaults, structured WS log, Node merged smoke, archive rotation history, `model-info` KTX2 size flag | sim-verified |
| 80 | N613–N622: meshopt v0 round-trip + partial-block bounds fix, manifest locality metric, fit transform matrix, advertised config defaults, filterable WS log, Swift tile parity, archive check, strict KTX2 validation | sim-verified |
| 81 | N623–N632: meshopt v1 control-mode round-trip, diff locality lines, `image` fit align+rotate (double-rotation fix), `config --defaults`, archive byte cap, `validate` KTX2 summary | sim-verified |
| 82 | N633–N642: meshopt nibble deltas, shared vertex cache, image vertical align, shared defaults table, bounded WS notes, C/Swift reconciliation fields, archive byte report, validate key-order contract | sim-verified |
| 83 | N643–N652: meshopt two-bit groups, cache hit statistics, image fit padding, defaults-only inspect, per-kind WS summary, reconciliation invariants, archive exit contract, validate summary order | sim-verified |
| 84 | N653–N662: meshopt width matrix, reorder hit rate, per-side image padding, defaults key, note kinds in health, Swift invariants, archive kinds, validate snapshot | sim-verified |
| 85 | N663–N672: meshopt 8-bit verbatim group, package order report, pad+rotate composition, defaults key filter, health escaping, compact archive report, report cache savings | sim-verified |
| 86 | N673–N682: meshopt v0 group widths, optimization hit/miss + saved, pad+align composition, merged config defaults, bounded WS log kinds, SDK reconciliation invariants, archive key snapshot, diff cache savings | sim-verified |
| 87 | N683–N692: meshopt verbatim partial groups, sprite fit/pad on shared fit box, package_order report, inspect defaults alias, configurable WS kinds, archive JSON alias, verify locality | sim-verified |
| 88 | N693–N702: v0 verbatim tail bounds, diff package order, sprite rotate/flip parity, defaults key parity, WS kind visibility, SDK smoke invariants, compact archive contract, strict verify | sim-verified |
| 89 | N703–N712: v0 multi-lane verbatim corpus, package-order manifest summary, sprite bilinear filter, default provenance/conflicts, structured health WS notes, Swift tile send, archive prune, shared KTX2 summary | sim-verified |
| 90 | N713–N722: v0 multi-block corpus, package-order reconciliation, sprite fit+pad+linear, inspect source filter, health kind filter, C tile sequence, prune manifest refresh, KTX2 compatibility row | sim-verified |
| 91 | N723–N732: scene polygon/polyline, easing, pinch gestures, DQS, HTTP conditionals, OTA resume, chunk dedup, clip playback modes, per-material cutoff, adaptive dirty threshold | sim-verified |
| 92 | N733–N742: arc/pie/path, pan+pinch, DQS normals, ranged OTA, disk chunk store, texture trim, text ellipsis/valign, adaptive pacing, HEAD | sim-verified |
| 93 | N743–N752: path command streams, mask node, three-finger swipe, DQS mesh package, disk compaction, HTTP Range, trim mips, joint tuning, text background/underline, texture-report | sim-verified |
| 94 | N753–N762: arc/smooth paths, mask invert/feather, multi-tap gestures, DQS weight normalisation, RLE chunks, shared surface budgets, group skew, atlas trim, If-Modified-Since, texture-report batch | sim-verified |
| 95 | N763–N772: relative/smooth paths, tap-chain boundaries, DQS scale, LZ chunks, weighted budgets, exact skew hits, atlas mips, HTTP preconditions, texture-report JSON | sim-verified |
| 96 | N773–N782: H/V paths, tap radius, non-uniform DQS, LZ fuzz/ratios, config weights, sheared clips, atlas mip selection, If-Range, single-file JSON | sim-verified |
| 97 | N783–N792: implicit path groups, multi-tap radius SDK, DQS inverse-transpose, LZ hash chains, weight credit, sheared images, alpha mips, weak ETags, texture-report archive | sim-verified |
| 98 | N793–N802: path stroke width, multi-tap window SDK, multi-joint DQS, LZ lazy matches, weight reset, sheared flip/rotate, indexed mips, weak ETag output, single-file archive | sim-verified |
| 87 | N683–N692: verbatim partial-group cases, sprite fit/pad on a shared fit box, package_order report, inspect --defaults alias, configurable WS kind limit, archive --json alias, verify manifest locality | sim-verified |

Waves **3–14** are marked **sim-verified architecture complete**: host FakeDevice / MemoryLink / C++ SimDisplay / `mdc_linux_peer` / SDK paths exercise the design. Physical board work remains `physical: pending`. D05, G03, and H06 stay unchecked until a physical board is measured.

## Honesty rules

- Skeleton code is `architecture`, not complete.
- `cargo test` host passes are not ESP-IDF or physical proof.
- Simulator FPS / heap numbers go in `docs/hardware-validation/*-sim.md`.
- Physical board docs stay `physical: pending` until measured on hardware.
