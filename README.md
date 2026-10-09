# MyDeviceCanvas

**Push live RGB565 frames from a macOS host to secondary ESP32 / Linux displays** over Serial, WebSocket, or TCP — with a protocol-first Core, board profiles, and a simulator that does not require physical hardware to develop or verify.

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Status](https://img.shields.io/badge/acceptance-sim--verified-green.svg)](docs/compatibility-matrix.md)

> 中文说明见下方 [中文简介](#中文简介)。

---

## Why this exists

Building a “second screen” product (dashboard, companion UI, VTuber / assistant display, lab instrument panel) usually means reinventing:

- a **stable wire protocol** that survives half-packets and noisy serial logs
- **device identity** that is not a port name or IP
- **bounded memory** on both host and MCU (checked sizes, queues, staging)
- **board differences** (pins, flash, DMA) without hard-coding them into the app

MyDeviceCanvas is the **Core + Runtime + SDK** layer for that problem. Applications talk to Devices and Surfaces through public APIs; they do not parse packets or own session state themselves.

## What works today

| Capability | Status |
|---|---|
| Protocol v1 (HELLO / Capabilities / Frame / Tile / Ping / Ack / Input / OTA types) | sim-verified |
| Host Core (`Session`, `DeviceManager`, bounded Frame/Tile queues) | sim-verified |
| In-process FakeDevice + Memory / TCP transports | sim-verified |
| CLI `mdc simulate` / send-image / benchmark on sim boards | sim-verified |
| Node / Rust / C SDK surfaces | architecture + host tests |
| ESP32 & Linux portable runtimes + `SimDisplay` HAL | host C++ tests |
| Physical ESP32-S3 LCD (e.g. JC3248W535 class) | **physical: pending** |

Default acceptance is **simulator / virtual board**. Physical results are documented separately and never implied by sim tests.

## Quick start

**Requirements:** Rust stable, Node.js 20+ (for Node SDK tests).

```sh
git clone https://github.com/moewolf-dev/MyDeviceCanvas.git
cd MyDeviceCanvas

# Full host / sim gate
./tools/check.sh

# Or the short path
cargo test --workspace
cargo run -p mdc -- simulate --board esp32-jc3248w535-sim
```

Example CLI:

```sh
cargo run -p mdc -- simulate --board esp32-jc3248w535-sim
cargo run -p mdc -- --sim devices
cargo run -p mdc -- --sim inspect
cargo run -p mdc -- --sim send-image ./path/to/image.png
cargo run -p mdc -- --sim benchmark 100
cargo run -p mdc-inspector -- --fresh
```

Minimal Rust example: [`examples/rust-hello`](examples/rust-hello).  
Node facade: [`sdk/node`](sdk/node). Electron demo: [`examples/electron-demo`](examples/electron-demo).

More commands: [`docs/development.md`](docs/development.md).

## Architecture

```text
  App / CLI / Electron / Tauri
            │
     public SDK API
            ▼
     ┌─────────────┐
     │  mdc-core   │  Session · DeviceManager · Frame/Tile queues
     └──────┬──────┘
            │
   ┌────────┴────────┐
   ▼                 ▼
mdc-protocol    mdc-transport     (Memory · Serial · WebSocket · TCP)
   │                 │
   │                 ▼
   │         Device peer bytes
   ▼
runtime/esp32 · runtime/linux · mdc-simulator
   (same schema + protocol/vectors)
```

**Rules**

- `mdc-transport` never parses business messages.
- `mdc-core` never hard-codes board pin maps (those live in `boards/`).
- Device runtimes and the simulator implement the **same** protocol independently.
- Discovery / provision / OTA / Touch are separate modules; capabilities gate unsupported features with an explicit error — not silent no-ops.

Design notes: [`docs/architecture.md`](docs/architecture.md) · Status: [`docs/implementation-status.md`](docs/implementation-status.md).

## Repository layout

| Path | Role |
|---|---|
| `crates/mdc-protocol` | Wire types, codec, shared test vectors |
| `crates/mdc-transport` | Byte channels (Memory, Serial, WebSocket, TCP) |
| `crates/mdc-core` | Session, DeviceManager, Surfaces, queues |
| `crates/mdc-simulator` | FakeDevice peer, faults, virtual clock |
| `crates/mdc-discovery` | Endpoint candidates (mDNS / serial filters, mocks) |
| `crates/mdc-provision` | Artifact registry and install state machine |
| `crates/mdc-assets` | Optional bounded asset cache (LRU / quota) |
| `runtime/esp32` | Portable parser, FrameStore, Display HAL + SimDisplay |
| `runtime/linux` | Linux / virt framebuffer peer |
| `boards/` | Board profiles (`esp32/`, `sim/`, templates) |
| `protocol/` | Schema + shared vectors |
| `tools/mdc-cli` | `mdc` command-line tool |
| `tools/flasher` | Install / sim flasher adapters |
| `tools/inspector` | Metrics JSON from simulate sessions |
| `sdk/node`, `sdk/rust`, `sdk/c`, `sdk/swift` | Language bindings |
| `tests/` | Simulator notes, compatibility, memory lifecycle |
| `examples/` | rust-hello, electron-demo, tauri-demo |

## Simulator boards

| Board id | Profile | Notes |
|---|---|---|
| `esp32-jc3248w535-sim` | [`boards/sim/esp32-jc3248w535-sim`](boards/sim/esp32-jc3248w535-sim/board.yaml) | 480×320 RGB565; parameters referenced from AgentDeck JC3248W535 docs — **not** claimed as measured on a purchased unit |
| `linux-virt` | [`boards/sim/linux-virt`](boards/sim/linux-virt/board.yaml) | Virtual framebuffer peer for Host API parity |

Validation write-ups: [`docs/hardware-validation/`](docs/hardware-validation/). Compatibility matrix: [`docs/compatibility-matrix.md`](docs/compatibility-matrix.md).

## Documentation map

| Doc | Content |
|---|---|
| [`docs/architecture.md`](docs/architecture.md) | Layers and lifecycle states |
| [`docs/protocol.md`](docs/protocol.md) | Protocol v1 overview |
| [`protocol/schema/v1.yaml`](protocol/schema/v1.yaml) | Field-level schema |
| [`docs/hardware-baseline.md`](docs/hardware-baseline.md) | Sim baseline fields and sources |
| [`docs/development.md`](docs/development.md) | Build / test commands |
| [`docs/upstream-porting.md`](docs/upstream-porting.md) | What we learned from AgentDeck / FrameOS (no code copy) |
| [`docs/scene-spec.md`](docs/scene-spec.md) | Reserved Scene capability (later) |
| [`docs/release-1.0-candidate.md`](docs/release-1.0-candidate.md) | 1.0 candidate gates |
| [`CHANGELOG.md`](CHANGELOG.md) | Release notes |

## Contributing

1. Prefer **one focused change** (protocol, transport, board profile, docs).
2. Run `./tools/check.sh` (or at least `cargo test --workspace`) before opening a PR.
3. Do not mark physical hardware as verified from simulator results.
4. Do not add empty BLE / WASM / .NET packages “for completeness”.
5. Board profiles must cite sources for pins and flash facts; unknown stays unknown.

Board template: [`boards/templates/board.yaml.example`](boards/templates/board.yaml.example) · Guide: [`docs/board-contribution.md`](docs/board-contribution.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

```text
Copyright 2026 moewolf-dev
```

Third-party design references (AgentDeck MIT, FrameOS AGPL) are documented in [`THIRD_PARTY_NOTICES`](THIRD_PARTY_NOTICES) and [`docs/upstream-porting.md`](docs/upstream-porting.md). **No FrameOS source is included** in this repository.

## Security

Please report security issues privately to the repository owner. Do not open public issues for unfixed vulnerabilities in protocol parsers or flasher paths.

---

## 中文简介

**MyDeviceCanvas** 是一套面向「主机推图 → 副屏显示」的开源 Core：

- **macOS 主机**通过 Serial / WebSocket / TCP 把 **RGB565 Frame / Tile** 推到 ESP32 或 Linux 副屏
- 应用只经公开 SDK 操作 Device / Surface，不自己解析协议或维护 Session
- 板型差异放在 `boards/` Profile 与 Runtime Adapter；默认用**模拟器**做开发与验收（`sim-verified`），实物板可选（`physical: pending`）

快速体验：

```sh
cargo test --workspace
cargo run -p mdc -- simulate --board esp32-jc3248w535-sim
```

协议与架构细节见 [`docs/`](docs/)。许可证为 **Apache-2.0**。
