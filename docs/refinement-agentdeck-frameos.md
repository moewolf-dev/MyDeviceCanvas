# Refinement: AgentDeck + FrameOS → task detail

This note tracks the first refinement pass after A–L architecture scaffolding.

## Task focus

| Task family | Upstream input | MDC change |
|---|---|---|
| C02 Serial | AgentDeck open backoff / port filter / generation | `mdc-transport` `serial_policy` + CLI `--port` |
| H02 Preflight | AgentDeck chip / geometry / unknown flash-id | `mdc-provision` `preflight_verdict` |
| H03 Flasher lease / reset | AgentDeck disk lease + `D0\|R1\|W100\|R0` | `mdc-flasher` `FilePortLease` |
| A02 / D01 Board SSOT | `board_35_ips.h` + ips_35 | `boards/esp32/jc3248w535` reference-unverified |
| D02 Memory | FrameOS packed size / half-PSRAM bpp (rules only) | `rgb565_bytes`, `canvas_bytes_per_pixel`, C++ budget header |
| Bus isolation | FrameOS I2C vs SPI note | `validate_board.py` pin collision |

## License posture

- AgentDeck: reimplement under Apache-2.0 with attribution in `upstream-porting.md`.
- FrameOS: **no source copy** (AGPL). Only numeric/ownership rules rewritten by us.
