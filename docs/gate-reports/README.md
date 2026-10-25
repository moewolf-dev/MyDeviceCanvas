# Delivery gate reports

Committed reports are copies of `target/gates/<timestamp>-<mode>/` produced by
`tools/gates.sh`. Each report records the repo commit, whether the tree was
dirty, toolchain, platform, limits, per-step status/logs, RSS growth and
transaction count.

| Report | Commit | Result |
|---|---|---|
| `20261025T082224Z-rc` | `52aff99` | rc passed: clean tree, fmt/clippy/workspace tests, Python tools, board YAML, C++, Node 20, fault matrix, C++ peer, native smoke, long report, release native SHA-256, App check/native/pin |

The long-soak evidence is `../soak-reports/r09-4311e15.json` (commit `4311e15`,
ancestor of `52aff99`). `validate-long-report.py --allow-ancestor` accepted it
because the only diff between the two commits is under `tests/`, `tools/`,
`docs/`, `.github/` plus a `Cargo.lock` entry for a dev-dependency. The ancestor
summary is in `long-soak.log`.

These reports are simulator/host evidence only. They do not claim physical
hardware, real OBS, signing/notarization, store review or production deployment.
