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

These are **sim / host** gates only — they do not measure ESP32 heap or PSRAM.
