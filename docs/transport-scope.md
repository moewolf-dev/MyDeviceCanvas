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
| BLE GATT | `docs/reference/nimble` (Apache NimBLE) | MTU + notify credit flow; pairing/credentials overlap with `mdc-discovery` | **No** |
| Native USB bulk | `docs/reference/libusb` | Needs a device-side USB stack and endpoint contract; serial/JTAG is the 0.x path | **No** |
| PCIe | Rockchip / Jetson BSPs (`docs/reference/rockchip-mpp`, `docs/reference/nvidia-cuda-samples`) | Board bring-up, not a host-side adapter; DMA/bar mapping is board-specific | **No** |

Rules for reserved transports:

1. A reserved transport **must not** be selectable in the CLI/SDK and must not be
   reported as a capability. Choosing an unimplemented transport returns an
   explicit error.
2. Reference source must be on disk (N02) before any compatibility detail is
   written. Guessing register maps, GATT UUIDs or endpoint layouts is forbidden.
3. When a reserved transport lands it becomes one more `Transport` adapter with
   the shared contract tests; it must not change the protocol or Core.

## 3D / high-end path

The 3D rendering path for high-end Linux/Jetson boards is **not** a transport.
It is a `mdc-model` target backend plus Form B mirror
(`docs/render-topology-decision.md` §4–§5). PCIe is only relevant as the board's
link to a discrete GPU; the Host still speaks Frame / MirrorFrame over the byte
channel.

## Evidence

Reference trees live under `docs/reference/` (N02) and are recorded in
`references.lock` and `THIRD_PARTY_NOTICES`. They are read-only design
references; none is vendored into the build.
