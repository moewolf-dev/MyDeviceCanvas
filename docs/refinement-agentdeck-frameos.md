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

## Wave: esptool + serial discover + display adapter

| Item | Location |
|---|---|
| EspToolFlasher dry-run argv `@0x0` / `--no-stub` | `tools/flasher/src/esptool.rs` |
| SerialDiscovery + denylist | `crates/mdc-discovery/src/serial_scan.rs` |
| BoardDisplayAdapter + geometry dirty | `runtime/esp32/include/mdc_display_adapter.hpp` |
| Host surface geometry write-back | `Device::update_surface_geometry` |
| CLI | `mdc discover`, `mdc flash-sim <merged.bin>` |

Live esptool: set `MDC_ESPTOOL_LIVE=1` and pass `--port`. Default remains dry-run.

## Wave: DTR/RTS reset + WS discovery + IDF skeleton

| Item | Location |
|---|---|
| Parse/apply `D0\|R1\|W100\|R0` | `mdc-transport` `parse_reset_sequence` / `SerialTransport::apply_post_write_reset` |
| Combined serial+WS discovery | `mdc-discovery` `WsDiscovery` / `CombinedDiscovery` |
| ESP-IDF AXS15231B entry | `runtime/esp32/idf/` (pins header + app_main stub) |

## Wave: reconnect + TCP peer + H06 CLI

| Item | Location |
|---|---|
| Session `awaiting_transport` + `provide_transport` | `mdc-core` |
| FakeDevice over `Box<dyn Transport>` | `mdc-simulator` |
| TCP peer binary | `tools/mdc-sim-peer` |
| EspTool live reset → `apply_post_write_reset` | `tools/flasher` (feature `serial`) |
| `mdc h06-demo` | `tools/mdc-cli` |

## Wave: H01 pairing + I03 connection switch

| Item | Location |
|---|---|
| `PairingStore` (pair/revoke/verify/authorize) | `crates/mdc-discovery/src/pairing.rs` |
| WS requires pairing; Serial/Memory do not | `PairingStore::authorize` |
| Session `needs_full_frame` + `switch_transport` | `mdc-core` |
| Tile gated until displayed full-frame ACK | `Session::send_tile` → `NeedFullFrame` |
| Manager `EndpointSwitched` | `DeviceManager::connect_session` |
| CLI | `mdc pair` / `unpair` / `authorize` / `switch-demo` |

## Wave: network display gate + IDF panel bring-up

| Item | Location |
|---|---|
| `Endpoint::Tcp` + pairing required | `mdc-discovery` `PairingStore` |
| `authorize_display_control` after HELLO | CLI `send-image --address` |
| Panel stages bus→Canvas→READY | `runtime/esp32/idf/port/mdc_panel_init.h` |
| Host `panel_bringup_test` | `runtime/esp32/idf/tests/` |

## Wave: Linux virt C++ TCP peer

| Item | Location |
|---|---|
| CBOR Capabilities / Ack (serde_cbor-compatible) | `runtime/esp32/src/cbor_control.cpp` |
| `SessionDispatcher` HELLO → CBOR caps | `dispatcher.cpp` |
| `mdc_linux_peer` TCP binary | `runtime/linux/main/mdc_linux_peer.cpp` |
| `linux-virt` 800×480 BoardProfile | `mdc-simulator` + `boards/sim/linux-virt` |
