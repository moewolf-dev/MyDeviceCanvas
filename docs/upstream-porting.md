# Upstream Porting

This project consulted local snapshots under the parent workspace `docs/` tree.

| Upstream | License | How we use it |
|---|---|---|
| AgentDeck | MIT | **Reimplement** algorithms and freeze board parameters with attribution. Do not paste entire upstream source files. |
| FrameOS | AGPL-3.0 | **Design rules only.** No FrameOS source, comments, or generated tables are included. Re-express rules in our own Rust/C/YAML. |

## AgentDeck → MyDeviceCanvas (this refinement)

| Source → reason | This project | Evidence |
|---|---|---|
| `esp32-serial.ts` `serialOpenFailureBackoffMs` | `mdc-transport::serial_open_failure_backoff_ms` + `SerialOpenGuard` | unit tests for 10s…300s cadence |
| Port patterns / exclude Bluetooth | `is_candidate_serial_port` | unit tests |
| Post-write `D0\|R1\|W100\|R0` | `POST_WRITE_RESET_SEQUENCE` / `mdc-flasher::post_write_reset_sequence` | constant + docs |
| Flash lease (read-time expiry) | `tools/flasher` `FilePortLease` | unit tests |
| `esp32PreflightVerdict` chip / geometry / unknown flash-id | `mdc-provision::preflight_verdict` | unit tests (`OkUnknownFlash`, `ChipMismatch`) |
| `board_35_ips.h` + `esp32-boards.ts` ips_35 | `boards/esp32/jc3248w535` (`reference-unverified`) + sim profile | `validate_board.py` |
| ips_35 stub axis unknown | `flash_stub: false`, `stub_axis: unknown` on physical yaml | validator keeps reference-unverified failed |

## FrameOS → MyDeviceCanvas (rules only, AGPL)

| Rule | This project | Evidence |
|---|---|---|
| Packed buffer length must match geometry | `mdc-protocol::rgb565_bytes` / C++ `mdc_display_budget.hpp` | unit tests |
| Half-PSRAM chooses 4 vs 2 bpp canvas | `canvas_bytes_per_pixel(..., share=2)` | unit tests |
| Dual full-screen RGB565 budget | `dual_rgb565_budget` → 614400 for 480×320 | board `max_framebuffer_bytes` |
| Do not copy 1536 KiB Nim/JS reserve | Board `min_internal_*` instead | hardware-baseline.md |
| I2C panel must not steal SPI/QSPI pins | `validate_board.py` pin collision check | unit tests |
| Cross-runtime ABI: borrow scalars/POD only | existing SDK Buffer copy + C opaque handles | sdk/node, sdk/c |

## Explicitly not ported

- AgentDeck LVGL UI, creature/aquarium, Wi-Fi product protocol, voice/photo frames
- FrameOS Nim drivers, pixie GC patterns, QuickJS heap model, AGPL source text

When a parameter changes, update this table, the board profile, and a boundary test together.
