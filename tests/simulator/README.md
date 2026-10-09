# Simulator integration notes

Host-side fake device peers live in the **`mdc-simulator`** crate:

```text
crates/mdc-simulator
```

Run its unit/integration tests (MemoryLink + VirtualClock + FaultInjector):

```sh
cargo test -p mdc-simulator
```

Or with the rest of the workspace:

```sh
cargo test --workspace
```

## What it covers

| Piece | Role |
|---|---|
| `FakeDevice` | Protocol peer: HELLO → Capabilities (board profile dims, default 480×320) |
| Frame | Accepts legacy (phase 0) and chunked begin/chunk/commit |
| Tile | Applies RGB565 patch after `base_frame_id` check |
| `FaultInjector` | `drop_next`, virtual `delay`, `disconnect` |
| `VirtualClock` | Deterministic `tick` counter (no wall clock) |
| Pixel buffer | Assertable via `pixels()` / `assert_pixels_eq` |

Physical hardware is out of scope here; this path is the default `sim-verified` gate.
