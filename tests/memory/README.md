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

`AllocationLedger` in `mdc-core` is the simulator soak: it counts outstanding bytes for a short run (`MDC_SOAK_ITERS`, default 64) and requires the counter to return to zero. A 60-minute process RSS sample is outside this simulator batch.

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
