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

Waves **3–14** are marked **sim-verified architecture complete**: host FakeDevice / MemoryLink / C++ SimDisplay / `mdc_linux_peer` / SDK paths exercise the design. Physical board work remains `physical: pending`. D05, G03, and H06 stay unchecked until a physical board is measured.

## Honesty rules

- Skeleton code is `architecture`, not complete.
- `cargo test` host passes are not ESP-IDF or physical proof.
- Simulator FPS / heap numbers go in `docs/hardware-validation/*-sim.md`.
- Physical board docs stay `physical: pending` until measured on hardware.
