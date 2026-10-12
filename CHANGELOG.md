# Changelog

All notable host / simulator changes for MyDeviceCanvas. Physical board verification stays out of scope until measured on hardware.

## [0.1.2] — Simulator batch without physical gates

### Added

- C ABI `poll_event`, `simulate_input`, and `connect_tcp`
- Swift `AsyncStream` over the C event queue
- Node `connectTcp` and an Electron `tcp:` endpoint
- Simulator OTA rollback that keeps the installed firmware on failure
- Shared protocol vector check in the C++ host test
- `AllocationLedger` short soak and a Linux systemd unit contract
- `mdc sim-accept`: handshake, full frame, tile, bounded inspect, and a failed OTA against `mdc-sim-peer` without changing firmware

## [0.1.3] — Simulator acceptance batch

### Added

- `tools/check.sh` runs `mdc sim-accept` against `mdc-sim-peer` and the ESP-IDF host stub
- `MDC_PAIRING_STORE` so pairing files do not share one temp path
- `mdc ota-confirm` installs firmware `2.0.0` on the same connection
- TCP reconnect blocks Tile until a displayed full frame
- `mdc-sim-peer --emit-input` delivers one touch after the full frame
- Node TCP smoke, Swift `MDC_TCP_ADDR` hello, and Electron `planConnection`
- WebSocket accept path: unpaired display is refused, then a full frame is acknowledged

## [0.1.4] — Simulator behavior batch

### Added

- FakeDevice rejects Frame and Tile with `E_FROZEN` while an OTA begin is active, and accepts them again after abort or rollback
- `dual-sim` board advertises `main` and `overlay`; an unknown surface is rejected
- Touch coordinates go through `transform_touch` using the board rotation; out-of-range points are dropped
- Three missed heartbeats reconnect with 1/2/4 s backoff, and the next Tile waits for a displayed full frame
- A third full frame replaces only the unsent pending slot
- WebSocket binary payloads larger than the configured limit are rejected before decode
- `mdc-sim-peer --drop-next` drops one packet without changing pixels
- `mdc linux-accept` checks the 800×480 linux-virt gate: Tile fails before the full frame and succeeds after it

## [0.1.1] — Wave 12 host gaps

### Added

- Electron demo connects explicitly, draws RGB565 color bars, and sends Frame/Tile through the public Node SDK
- Swift `MdcSession` wrapper over the C ABI, including Scene refusal
- `mdc-tauri-demo` command layer: endpoint switch requires a full frame; inspector stores counts only
- Linux runtime `update_geometry` and `mdc_linux_probe` (no display session / zero size)
- Board profiles `pi4-hdmi` and `waveshare-eink` (`color_fps: false` on e-ink)
- `Session::send_scene` returns Unsupported and does not emit a Scene packet

## [0.1.0] — Waves 0–7 architecture (sim-verified)

### Added

- Protocol v1 host crates: `mdc-protocol`, `mdc-transport`, `mdc-core`, `mdc-simulator`
- CLI `mdc` with `simulate`, `devices`, `inspect`, `send-image --sim`, `benchmark --sim`, `--address` TCP
- Node native `NativeManager` FakeDevice session (`devices`, `sendFrame`, `dispose`)
- Discovery `MockMdnsProvider`, provision `tools/flasher` SimFlasher
- InputEvent → `ManagerEvent::Input`; OTA capability-gated in Session + FakeDevice
- C ABI (`sdk/c`), Swift wrap notes, optional `mdc-assets` LRU cache
- Inspector metrics JSON from last simulate; Linux runtime peer + board templates
- Compatibility + memory lifecycle tests (major mismatch, low limits, connect/disconnect ×1000)

### Notes

- G03 exit criterion: **sim-verified** end-to-end on FakeDevice / MemoryLink (see `docs/compatibility-matrix.md`).
- Physical: pending for all board profiles.
