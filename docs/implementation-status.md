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

Waves **3–8** are marked **sim-verified architecture complete**: host FakeDevice / MemoryLink / C++ SimDisplay paths exercise the design. Physical board work remains `physical: pending`.

## Honesty rules

- Skeleton code is `architecture`, not complete.
- `cargo test` host passes are not ESP-IDF or physical proof.
- Simulator FPS / heap numbers go in `docs/hardware-validation/*-sim.md`.
- Physical board docs stay `physical: pending` until measured on hardware.
