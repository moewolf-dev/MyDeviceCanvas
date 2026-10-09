# Architecture

## Layers

- `mdc-protocol` owns wire types and codec. It is the only place that defines
  header layout, CBOR control payloads, and binary Frame/Tile payloads.
- `mdc-transport` owns byte channels. It never parses business messages.
- `mdc-core` owns Session, DeviceManager, Surfaces, negotiated Capabilities, and
  bounded Frame/Tile queues. It does not hard-code board pin maps.
- CLI and SDKs consume Core public APIs only.
- Device runtimes (`runtime/esp32`, `runtime/linux`) and `tests/simulator` implement
  the same protocol independently against shared vectors.

## Lifecycle status (three-state)

Each stage is tracked as:

| State | Meaning |
|---|---|
| `architecture` | Module boundaries and public types exist |
| `sim-verified` | End-to-end checks pass on simulator / virtual board |
| `physical: pending` | Optional real hardware; never implied by sim |

Default release gate is `sim-verified`. Physical boards do not block progress.

## 0.1 path

macOS host → Core → Serial / WebSocket / Memory / TCP → simulator or device peer.
HELLO supplies stable device ID and capabilities. Port names and IPs are not IDs.

RGB565 little-endian, tightly packed rows is the only display pixel format in the
current architecture wave. Other formats return a clear error.

## Later modules

Discovery, provisioning, Touch, OTA, C/Swift ABI, asset cache, Inspector, Linux
runtime, and Scene live in dedicated crates/docs. Protocol message types may exist
early so peers receive explicit `Unsupported` when capabilities are off. Empty
BLE / WASM / .NET packages are not created.
