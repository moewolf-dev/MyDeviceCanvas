# Submodule Contract (N310)

Status: **host + simulator contract**. Physical hardware is unverified.

This document is the contract a downstream project accepts when it vendors
MyDeviceCanvas as a git submodule. It says what is stable, what is **not** yet
proven, and how to pin it safely.

## 1. One-sentence scope

> `MyDeviceCanvas` is a **host + simulator contract**: protocol, transport, Core,
> model compiler and mirror are exercised against in-process/loopback doubles and
> host C++ tests. **`sim-verified` is not `hardware-usable`.** The physical exit
> tasks **D05 / G03 / H06 are unchecked** and stay unchecked until measured on a
> real board.

## 2. What is safe to depend on

- **Dependency direction** (no cycles): `mdc-protocol` and `mdc-transport` have no
  upper-layer dependency; `mdc-core → protocol + transport`; `mdc-model →
  protocol` (does **not** depend on Core); `mdc-mirror → protocol + transport`;
  `mdc-schedule` depends on nothing (`mdc-core` uses it only for opt-in pacing,
  N329). `mdc-mirror` never links OBS / Electron /
  Tauri and never renders. A downstream GUI/agent app pulls in only what it uses.
- **Wire protocol v1**: header layout, magic, CBOR control, binary images, the
  message types listed in `docs/protocol.md`, and the shared vectors in
  `protocol/vectors/` (checked byte-for-byte by both the Rust and C++ tests).
- **Additive compatibility**: new capability fields are `serde` defaulted and
  omitted on the wire when unset; new message types are reserved values. An old
  peer decodes a new peer's control maps without failing. See `CHANGELOG`.
- **Capability negotiation**, not board-type branching: everything new is gated by
  an advertised capability; unknown capability bits are refused with an explicit
  error rather than a silent no-op.

## 3. What is NOT proven (do not treat as usable)

| Area | Why it is not proven | Where |
|---|---|---|
| Physical link (Serial / Wi-Fi / USB) | No board was measured; D05 open | `docs/hardware-validation/` |
| Release gate | G03 open | TODO §8 |
| End-to-end install → provision → OTA | H06 open | TODO §10 |
| ESP32 runtime on IDF | Only host C++/stub + `SimDisplay`; no IDF build/run evidence | `runtime/esp32` |
| DMA / stride / alignment | Board profiles carry documented facts, not measured behaviour | `docs/compatibility-decisions.md` |
| Partial-refresh timing | e-ink / partial LCD timing is modelled, not measured | `docs/hardware-validation/` |
| **Mirror read source** | draw-buffer vs final framebuffer is an open decision | `render-topology-decision.md` §4.3 |
| Diagnostics over a separate link | message types reserved, **payload not implemented** | `transport-scope.md` |
| BLE / USB-bulk / PCIe | reserved, **not implemented** | `transport-scope.md` |
| OBS | HTTP / file delivery only; **no OBS binary plugin** | `docs/obs-bridge.md` |

## 4. Reserved-but-not-implemented surface

The following exist so a downstream pin of the wire/API is stable, but they are
**not features**: `MessageType::{Telemetry, Metric, Crash, BootInfo}` (payload not
decoded); `Hello.boot_id`, `Capabilities.boot_id/run_id`, `Ack.run_id/event_seq`
(the fields are acted on for reboot detection, the diagnostic *payloads* are not);
`PixelFormat::{Rgb888, Rgba8888, Gray1, Gray4, Other}` (decode, but the host/frame
path only implements RGB565 and refuses the rest with an explicit error);
`Surface::{preferred_tile_size, supports_partial_update, supports_full_framebuffer}`;
BLE / native USB bulk / PCIe transports. "A reserved interface is not a finished
feature."

## 5. Pinning guidance

1. Pin a **commit SHA**, not a branch. The wire is additive-stable within major 1,
   but new capability fields and reserved types land continuously.
2. Gate on **capabilities**, never on `device_id` / board id.
3. Surface `sim-verified` vs `physical: pending` to *your* users as well; do not
   relabel simulator results as hardware support.
4. If you run the model compiler or mirror, read `docs/model-compiler.md` and
   `docs/obs-bridge.md`: the shadow buffer is host-bounded (8 MiB) and the model
   pipeline refuses decode bombs before allocation.
5. Re-run `./tools/check.sh` after every pin bump; it is the host+sim gate, not a
   hardware gate.
