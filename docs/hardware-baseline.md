# Hardware Baseline

## Primary baseline: simulator

Status: **sim-baseline** (not physical).

Board id: `esp32-jc3248w535-sim`  
Profile: `boards/sim/esp32-jc3248w535-sim/board.yaml`

| Field | Value | Source |
|---|---|---|
| MCU | ESP32-S3 | AgentDeck `esp32-boards.ts` ips_35 / `board_35_ips.h` |
| Flash | 16 MiB, DIO, 80 MHz | AgentDeck ips_35 (`flashSize: 16MB`) |
| PSRAM | 8 MiB (sim budget) | AgentDeck compatibility table (8 MB PSRAM class) |
| Panel native | 320×480 AXS15231B QSPI | `board_35_ips.h` |
| Logical surface | 480×320 landscape, RGB565 LE, rotation 1 | AgentDeck rotation 1 |
| QSPI pins | CS45 CLK47 D0=21 D1=48 D2=40 D3=39 BL=1 TE=38 | `board_35_ips.h` |
| Touch | AXS15231B I2C 0x3B; SDA4 SCL8 INT11 RST12 | `board_35_ips.h` |
| USB | Native USB-Serial/JTAG | AgentDeck ips_35 `nativeUsb: true` |
| Default host baud | 115200 | Project default for Frame/Tile; flash path 460800 |
| OTA slots | true (sim) | AgentDeck ips_35 `ota: true` |
| Host / toolchain | macOS arm64 or x86_64; Rust stable; Node 20+ | Development entry |
| Memory budget | active+staging ≤ 614400 B RGB565; checked allocs | Project memory table |

Physical validation: **pending**. Do not flash unknown purchased boards from this
profile without re-identifying markings.

## Secondary baseline: Linux virt

Board id: `linux-virt`  
Profile: `boards/sim/linux-virt/board.yaml`  
Framebuffer peer for Host API parity tests. Status: sim-baseline.

## Unverified physical candidate

`boards/esp32/jc3248w535/board.yaml` remains `unverified` until a purchased unit
is identified. It must not be used as a flash target.
