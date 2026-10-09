# Implementation Status

Track items with three states: `architecture`, `sim-verified`, `physical: pending`.
Do not mark physical verification from host-only or simulator tests.

## Wave progress

| Wave | Focus | State |
|---|---|---|
| 0 | Repo baseline, sim hardware baseline, quality entry | in progress |
| 1 | Protocol completeness + simulator peer | pending |
| 2 | Transport / Session / Manager wiring | pending |
| 3 | Board HAL + Runtime + SimDisplay | pending |
| 4 | CLI / Node SDK / G03 sim exit | pending |
| 5 | Discovery / Provision / Touch / OTA | pending |
| 6 | C/Swift / cache / Inspector | pending |
| 7 | Linux Runtime / Scene / 1.0 candidate docs | pending |

## Honesty rules

- Skeleton code is `architecture`, not complete.
- `cargo test` host passes are not ESP-IDF or physical proof.
- Simulator FPS / heap numbers go in `docs/hardware-validation/*-sim.md`.
- Physical board docs stay `physical: pending` until measured on hardware.
