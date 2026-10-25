# Memory / lifecycle stress notes

Host-side ownership checks live in `mdc-core` unit tests (not a separate harness process):

| Test | Location | What it proves |
|---|---|---|
| `repeated_transactions_release_owned_staging` | `crates/mdc-core` | 100 000 FrameAssembler begin/chunk/commit cycles drop staging |
| `connect_disconnect_loop_is_cheap` | `crates/mdc-core` | 1 000 Session connect → Ready → disconnect cycles on MemoryLink |

```sh
cargo test -p mdc-core connect_disconnect_loop_is_cheap
cargo test -p mdc-core repeated_transactions_release_owned_staging
```

`AllocationLedger` in `mdc-core` counts outstanding bytes and requires the
counter to return to zero.

## SIM-R09 container and long-run soak

`Session::container_counts()` exposes every bounded container (RX, pending
requests/frames/deadlines, ACK window, timings, held frames, diagnostics, config
cache, pointers, mirror inbox/base, frame transactions, manager events, devices).

| Test | Gate | Result |
|---|---|---|
| `completed_transactions_keep_containers_bounded` | `MDC_TRANSACTION_SOAK_ITERS` (default 2000) | 100000 frames + 2000 tiles: all containers capped, pending/held zero, `copy_outstanding` zero |
| `long_soak_keeps_resources_flat` | `MDC_LONG_SOAK=1` | samples RSS/threads/FDs/containers after warmup, midpoint, end and idle; RSS budget 10 MiB |

Quick run recorded on 2026-10-25 (macOS arm64, debug build):

```text
warmup 5 s, steady 30 s, idle 5 s
transactions 21385
RSS growth 128 KiB (budget 10240 KiB)
threads 2 -> 2
fds 4 -> 4
containers: pending/held/frame_tx 0, ACK <= 4096, timings 256,
            manager events <= 128, devices 1
package installs every 3000 transactions: ready
```

The full gate is:

```sh
MDC_LONG_SOAK=1 MDC_SOAK_WARMUP_SECS=300 MDC_SOAK_SECS=3600   MDC_SOAK_IDLE_SECS=60 cargo test -p mdc-memory-tests --test long_soak -- --nocapture
```

That 60-minute run has **not** been recorded yet; R09 stays open until it is.
These are host/sim gates only — they do not measure ESP32 heap or PSRAM.

These are **sim / host** gates only — they do not measure ESP32 heap or PSRAM.

## Form B soak crate (N36)

`tests/memory` is now a workspace crate (`mdc-memory-tests`) covering the mirror
and runtime-package paths:

| Test | What it proves |
|---|---|
| `shadow_framebuffer_soak_is_bounded` | 5000 present/lease cycles never grow the dual shadow buffer |
| `session_mirror_and_package_soak_returns_to_zero` | 100 connect → install package → mirror 4 frames → disconnect rounds; harness-held frame buffers release to zero and the peak stays within one frame |
| `mirror_receiver_soak_is_bounded` | 2000 full-frame feeds keep the shadow bytes constant |

```sh
cargo test -p mdc-memory-tests
```
