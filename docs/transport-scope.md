# Transport Scope (N07)

Status: **architecture / reserved**. This document defines the transport
surface. It does not add empty implementation packages — per the plan,
"接口预留不等于功能完成" (a reserved interface is not a finished feature).

## Implemented

| Transport | Adapter | Status | Notes |
|---|---|---|---|
| In-memory | `MemoryLink` | sim-verified | Tests, FakeDevice host path |
| Serial | `SerialTransport` (macOS) | sim-verified (PTY) | `physical: pending`; DTR/RTS + backoff policy |
| WebSocket | `WebSocketTransport` | sim-verified | binary messages limited at the adapter |
| TCP | `TcpTransport` | sim-verified (loopback) | also carries Wi-Fi and gigabit Ethernet |

TCP is transport-agnostic: Wi-Fi and gigabit Ethernet both terminate in a TCP
byte stream, so they reuse `TcpTransport` unchanged. No separate Ethernet
adapter is created. The only difference is endpoint discovery and MTU/bandwidth
accounting, both of which live in `mdc-discovery` and the board profile, not the
byte channel.

All adapters implement the same `mdc_transport::Transport` trait
(`write`/`read`/`close`/`cancel`). Contract tests are shared; an adapter that
cannot satisfy short-write, close-idempotency and cancel semantics is not ready.

## Reserved (not implemented)

| Transport | Engine to port from | Why reserved | Advertise? |
|---|---|---|---|
| BLE GATT | workspace `docs/nimble` (Apache NimBLE) | MTU + notify credit flow; pairing/credentials overlap with `mdc-discovery` | **No** |
| Native USB bulk | workspace `docs/libusb` | Needs a device-side USB stack and endpoint contract; serial/JTAG is the 0.x path | **No** |
| PCIe | Rockchip / Jetson BSPs (workspace `docs/rockchip-mpp`, `docs/nvidia-cuda-samples`) | Board bring-up, not a host-side adapter; DMA/bar mapping is board-specific | **No** |

Rules for reserved transports:

1. A reserved transport **must not** be selectable in the CLI/SDK and must not be
   reported as a capability. Choosing an unimplemented transport returns an
   explicit error.
2. Reference source must be on disk (N02) before any compatibility detail is
   written. Guessing register maps, GATT UUIDs or endpoint layouts is forbidden.
3. When a reserved transport lands it becomes one more `Transport` adapter with
   the shared contract tests; it must not change the protocol or Core.

## Reserved diagnostics / telemetry (N303) — not implemented

The HIL report asks for structured diagnostic events (`TEST_RESULT` / `METRIC` /
`CRASH` / `BOOT_INFO`) and a `boot_id` / monotonic sequence so a device reboot does
not let old logs be read as the current run's result. So the wire enum reserves
the message types now:

| Type | Code | Status |
|---|---|---|
| `Telemetry` | `0x60` | **reserved, not implemented** — payload not decoded |
| `Metric` | `0x61` | **reserved, not implemented** |
| `Crash` | `0x62` | **reserved, not implemented** |
| `BootInfo` | `0x63` | **reserved, not implemented** |

`MessageType::try_from` (Rust) and the C++ `Parser::known_type` accept these
values so a downstream pin of the wire enum is stable, but neither side decodes a
payload yet. `mdc-schedule` is unrelated; diagnostics are **not** sent on the link
under test:

1. A diagnostic/telemetry channel **must be independent of the link under test**.
   A crashing or saturated Frame/MirrorFrame link must not be able to lose the
   results of that same test. The Python + pytest HIL controller (report 1) is the
   intended consumer; it drives the Core through the stable CLI/JSON/CBOR boundary,
   not by linking Rust.
2. `boot_id` (device, on `Capabilities`) and the host `boot_id` (on `Hello`) are
   reserved fields; a changed device `boot_id` makes `Session` discard the previous
   run's pending ACKs and held frames. `Ack.run_id` / `Ack.event_seq` carry the run
   identity and a monotonic sequence. All three default to 0 and are omitted on the
   wire when unset, so pre-N304 peers are byte-identical.

## 3D / high-end path

The 3D rendering path for high-end Linux/Jetson boards is **not** a transport.
It is a `mdc-model` target backend plus Form B mirror
(`docs/render-topology-decision.md` §4–§5). PCIe is only relevant as the board's
link to a discrete GPU; the Host still speaks Frame / MirrorFrame over the byte
channel.

## Evidence

Reference trees live in the **workspace `docs/`** tree (N02) and are recorded in
`docs/reference/references.lock` and `THIRD_PARTY_NOTICES`. They are read-only
design references outside this repo; none is vendored into the build.
