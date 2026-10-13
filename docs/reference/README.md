# Reference SDK Snapshots (N02)

The 2026-10-13 instruction required upstream SDK / protocol sources on disk so
board and transport compatibility is written from facts, not from memory. Each
repository is a **read-only snapshot** cloned into its own subdirectory here.

- The snapshots themselves are git-ignored (third-party trees, not our code).
- `references.lock` records every snapshot's URL, branch/tag, commit hash and
  license. `fetch-references.sh` reproduces the set.
- `THIRD_PARTY_NOTICES` and `docs/upstream-porting.md` carry the licensing rule.

Reproduce or update:

```sh
docs/reference/fetch-references.sh            # all
docs/reference/fetch-references.sh esp-idf    # one
```

## Purpose per repository

| Directory | Upstream | License | Used for |
|---|---|---|---|
| `esp-idf` | espressif/esp-idf @ v5.3.2 | Apache-2.0 | ESP32-S3 LCD/QSPI panel bring-up, `esp_lcd`, OTA partition rules |
| `esp-bsp` | espressif/esp-bsp | Apache-2.0 | mainstream ESP32-S3 LCD board examples (board support packages) |
| `raspberrypi-userland` | raspberrypi/userland | BSD-3-Clause | Raspberry Pi display/DRM reference; HDMI framebuffer peer |
| `arduino-core-avr` | arduino/ArduinoCore-avr | LGPL-2.1 | Arduino MCU family reference (`boards/arduino/`) |
| `rockchip-mpp` | rockchip-linux/mpp | Apache-2.0 OR MIT | RK3399 Media Process Platform; display/GPU path |
| `nvidia-cuda-samples` | NVIDIA/cuda-samples | BSD-3-Clause | Jetson 3D rendering path reference (Form B backend) |
| `mdns` | mjansson/mdns | Unlicense | mDNS candidate discovery reference |
| `websocket-ws` | websockets/ws | MIT | WebSocket behavior reference (limits, ping/pong) |
| `libusb` | libusb/libusb | LGPL-2.1 | Reserved native USB bulk transport reference |
| `nimble` | apache/mynewt-nimble | Apache-2.0 | Reserved BLE GATT transport reference |

## Licensing rules

- These are **design references**. No source from them is copied into a
  MyDeviceCanvas crate or runtime without a `THIRD_PARTY_NOTICES` entry.
- FrameOS remains **AGPL-3.0 and design-only**; no FrameOS source is downloaded
  or copied here.
- Board profiles and compatibility code must cite the exact file and commit
  from `references.lock` before claiming a hardware detail.
- A snapshot existing on disk does **not** make its board or transport
  `physical-verified` or even implemented. See `docs/transport-scope.md`.
