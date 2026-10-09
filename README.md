# MyDeviceCanvas

Host Core and device runtimes for pushing RGB565 Frame/Tile images from macOS to
ESP32 / Linux secondary displays over Serial or WebSocket.

## Layout

| Path | Role |
|---|---|
| `crates/mdc-protocol` | Wire types, codec, shared test vectors |
| `crates/mdc-transport` | Byte channels (Memory, Serial, WebSocket, TCP) |
| `crates/mdc-core` | Session, DeviceManager, Frame/Tile queues |
| `crates/mdc-discovery` | Endpoint candidates (mDNS/serial filters) |
| `crates/mdc-provision` | Artifact registry and install state machine |
| `runtime/esp32` | Portable device parser + Display HAL (+ Sim) |
| `runtime/linux` | Linux peer runtime (sim framebuffer) |
| `boards/` | Board profiles (`esp32/`, `sim/`) |
| `tools/mdc-cli` | `mdc` CLI |
| `sdk/node`, `sdk/rust`, `sdk/c` | Language bindings |
| `tests/simulator` | Fake device peer and virtual clock |
| `protocol/` | Schema and shared vectors |

## Acceptance

Default verification is **simulator / virtual board** (`sim-verified`). Physical
hardware is optional and documented separately as `physical: pending`.

## Quick checks

See [docs/development.md](docs/development.md). License: Apache-2.0.
