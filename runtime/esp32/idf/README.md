# ESP-IDF port skeleton (AXS15231B / JC3248W535)

Host-side portable code lives in `runtime/esp32/{include,src}` and is tested on
macOS without IDF. This directory is the **on-device** entry that links the same
parser / FrameStore / Display HAL against ESP-IDF + a QSPI panel driver.

## Status

`architecture` + host bring-up unit test. Physical flash remains `physical: pending`.
Real QSPI drivers are not linked in host CI.

## Layout

| Path | Role |
|---|---|
| `CMakeLists.txt` | IDF project when `IDF_PATH` set; else host stub + test |
| `main/mdc_app_main.c` | app_main / host anchor: panel bring-up → session loop stub |
| `port/axs15231b_qspi.h` | Pin / Canvas requirements from AgentDeck `board_35_ips.h` |
| `port/mdc_panel_init.h` | Ordered stages: bus → panel → Canvas → backlight → READY |
| `tests/panel_bringup_test.c` | Host assert Canvas-before-READY |
| `partitions.csv` | Placeholder single-app table (OTA slots later) |

## Build (when ESP-IDF is installed)

```sh
# From a configured ESP-IDF environment:
cd runtime/esp32/idf
idf.py set-target esp32s3
idf.py build
```

Without IDF, rely on host tests:

```sh
cmake -S . -B /tmp/mdc-idf-stub && cmake --build /tmp/mdc-idf-stub && ctest --test-dir /tmp/mdc-idf-stub --output-on-failure
c++ -std=c++17 -I ../include ../src/sim_display.cpp ../tests/adapter_test.cpp -o /tmp/mdc_adapter_tests
/tmp/mdc_adapter_tests
```

## Board constraints (do not guess)

- Controller: AXS15231B QSPI; **requires Canvas wrapper** (direct QSPI → black)
- Native 320×480, logical 480×320 @ rotation 1
- Pins: see `boards/sim/esp32-jc3248w535-sim/board.yaml` / `boards/esp32/jc3248w535`
- Flash: explicit 16MB DIO 80m; stub axis unknown → prefer `--no-stub`
