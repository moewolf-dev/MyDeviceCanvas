# Upstream Porting

This project consulted local snapshots under the parent workspace `docs/` tree.
No AgentDeck or FrameOS source files are copied into this repository.

| Upstream | License | Local snapshot role | What we reuse |
|---|---|---|---|
| AgentDeck | MIT | Design reference for ESP32 board facts, serial reset, flash preflight | Parameters and failure modes only |
| FrameOS | AGPL-3.0 | Design reference for display buffer sizing and driver ABI caution | Ideas only; no AGPL code |

## Parameter inheritance records

| Source → reason | This project value | Evidence |
|---|---|---|
| AgentDeck `board_35_ips.h` pins / 320×480 native / rotation 1 | `boards/sim/esp32-jc3248w535-sim` 480×320 landscape RGB565 | Documented in board.yaml `source` field; validated by `validate_board.py` |
| AgentDeck `esp32-boards.ts` ips_35: ESP32-S3, 16MB flash, native USB, 460800 upload baud | sim profile flash 16 MiB, baud 115200 default / 460800 flash | hardware-baseline.md |
| AgentDeck serial open triggers DTR/RTS | `SerialConfig` optional DTR/RTS; mutual exclusion for flash | transport serial adapter |
| FrameOS display buffer sizing / PSRAM reserve | Board Profile `min_internal_*` and dual RGB565 budget 614400 B | architecture + board.yaml memory section |
| FrameOS ABI ownership across runtimes | SDK Buffer copy rules; C ABI opaque handles | sdk/node, sdk/c |

When a parameter changes, update this table, the board profile, and a boundary test together.
