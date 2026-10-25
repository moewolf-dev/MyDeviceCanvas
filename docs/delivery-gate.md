# Simulator delivery gate (SIM-R10)

`tools/gates.sh` is the repeatable entry point. Each run writes logs and a
machine-readable report to `target/gates/<UTC timestamp>-<mode>/`:

- `report.json` — commit, dirty flag, toolchain, platform, limits, duration,
  per-step status/log, peak RSS growth and transaction count when available
- `<step>.log` — full stdout/stderr for that step

The report is evidence for the *simulator/host* scope only. It does not claim
physical hardware, real OBS, signing/notarization, store review or production
deployment.

## Tiers

| Tier | Command | Contents |
|---|---|---|
| quick | `tools/gates.sh quick` | `cargo fmt --check`, clippy, Core/protocol/transport/simulator/memory tests, C++ parser/CBOR/adapter/package/linux/vector suites, Node wrapper + native smoke |
| cross | `tools/gates.sh cross` | fault matrix (`mdc-sim-peer`), C++ runtime peer (Rust host ↔ C++ process), native smoke (TCP/WS/PTY) |
| long | `tools/gates.sh long` | R09 soak: 300 s warmup, 3600 s steady, 60 s idle, 10 MiB RSS budget |
| rc | `tools/gates.sh rc` | clean-tree check, quick + cross + long, release native build + SHA-256, App `check`, App native rebuild/verify, App pin == Core HEAD |
| all | `tools/gates.sh all` | quick + cross + long (allows a dirty tree) |

Environment knobs:

```sh
CARGO_BIN=... CXX=c++ NODE_BIN=/usr/local/bin/node NPM_BIN=npm \
MDC_GATE_REPORT_DIR=target/gates \
MDC_SOAK_WARMUP_SECS=300 MDC_SOAK_SECS=3600 MDC_SOAK_IDLE_SECS=60 \
MDC_RSS_BUDGET_KIB=10240 \
  tools/gates.sh long
```

Node must be the declared Node 20 line; `node_smoke` refuses other majors so the
App environment contract cannot silently pass on Node 24.

## Release candidate

`tools/gates.sh rc` is the only tier that requires:

1. `git status --porcelain` empty in `MyDeviceCanvas`
2. quick, cross and the full long soak passing
3. a release `mdc-node-native` artifact with a recorded SHA-256
4. `moetuber` `npm run check` (lint + typecheck + tests + build)
5. `moetuber` release native rebuild and `npm run verify:native`
6. `CORE_PIN_SHA` equal to the Core HEAD commit

Because the App pin is amended per batch locally, run `rc` after the App pin is
committed. Updating the pin after `rc` invalidates the report and requires a new
run.

## Current record

| Gate | Status |
|---|---|
| quick | scripted; run from a clean commit before release |
| cross | scripted; fault matrix 15 + C++ peer 3 + native smoke 14 |
| long quick probe (5 s + 30 s + 5 s) | passed: 21385 transactions, RSS +128 KiB, threads/FDs flat |
| long 60-minute gate | **not yet recorded** |
| rc | **not yet run** |

R09/R10 stay open until the 60-minute report and a clean `rc` report are attached
to the task record. Runtime heap/PSRAM measurements are still out of scope.
