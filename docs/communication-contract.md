# Communication Contract (SIM-R01)

Status: **baseline drafted; R02 is implemented, R03–R10 remain open.**
This is the host/simulator reliability contract from Core TODO §108. It is not a
hardware claim and does not replace `Core:D05/G03/H06`.

## 1. API success semantics

| Stage | Meaning |
|---|---|
| queued | bytes are in a local bounded queue; nothing is on the wire yet |
| written | the transport accepted the complete packet (not necessarily a peer ACK) |
| received | the peer returned `ACK(received=true)` |
| displayed / ready | the peer returned `ACK(displayed=true)` or `package_ready` |

- A returned request ID means "accepted locally", not "displayed".
- Every job completes exactly once. Errors carry the device/session generation
  and request ID.
- A zero pending count has a typed empty state; it is never confused with a
  request that has not been sent.
- Public Rust/Node/C APIs use these four stages; current gaps are tracked in
  SIM-R03/R04/R05.

## 2. Time

- Defaults are frozen as: connect/HELLO 20 s, per-request ACK 5 s, heartbeat 5 s,
  reconnect backoff 1/2/4/8/16/30 s with injectable jitter.
- Each request stores its own send time and deadline. A newer request must not
  extend an older request's deadline.
- Production uses a monotonic clock; tests inject a virtual clock. The same state
  machine consumes both.
- Reconnect exhaustion reaches a terminal error, never an indefinite wait.

Implemented in R04: per-request deadlines. Remaining: the Node SDK still pumps
synchronously inside request calls (SIM-R05 tick owner).

## 3. Send and backpressure

- Per Surface: at most one in-flight full frame and one pending full frame.
- Control queue: at most 32 in-flight requests.
- All packets for a session use one serial writer. A short write resumes only the
  unaccepted tail; other packets cannot interleave inside a packet.
- Frame/Tile/control/package transactions save phase, packet offset, remaining
  chunks and deadline. Unknown written-prefix errors close the old link and
  rebuild the transaction rather than guessing an offset.
- A full queue is a retryable condition with an explicit error; it cannot be
  reported as success.

Implemented in R03: resumable Frame BEGIN/CHUNK/COMMIT transactions, one control
window slot per PACKAGE (COMMIT only), and link abandonment when a package write
fails mid-flight. Every message type shares `write_packet_with_id` /
`write_bytes`; Tile is a single packet and resumes from `unsent`.

## 4. Lengths and budgets

- Control payload: 4 KiB maximum; checked before CBOR decode.
- Frame: 1 MiB maximum.
- Chunk: `min(local, negotiated max_chunk)` minus this packet's metadata overhead.
- Runtime package: 16 MiB wire maximum (aligned with the C++ runtime); the app's
  local file cap must not be mistaken for a larger wire allowance.
- All offset/length arithmetic is checked. BEGIN lengths must match the declared
  surface geometry before allocation.

Current gap: SIM-R07 (receive budgets and all growing containers).

## 5. Receive fairness

- Target per poll: at most 64 KiB or 32 packets, whichever comes first.
- RX buffering is bounded; a burst cannot force one poll to consume an unbounded
  stream before parsing or ticking.
- Oversized control messages are rejected by length before allocation/decoding.

Current gap: SIM-R07 (Rust BurstTransport currently returns 4 MiB in one poll).

## 6. Integrity

- Frame/Package BEGIN declares a length; it must match the legal object and local
  cap before allocation.
- Chunks track covered byte ranges. Identical duplicate chunks are idempotent;
  partial overlaps are rejected.
- COMMIT succeeds only when ranges cover the full expected length. A missing
  chunk never displays zero-filled data.
- Conflicting/oversized chunks clear the transaction and require a new BEGIN.

Implemented in R02: C++ parser types `0x12/0x31/0x32`, BEGIN geometry/length,
range coverage, duplicate/overlap behavior, unknown surface rejection.
R03 adds host-side package failure handling: a short/IO error abandons the link
instead of resuming against an unknown written prefix.

## 7. Lifecycle

- `cancel`, `close`, `dispose` are idempotent.
- After cancel, no business frame or old result is delivered.
- On disconnect/replace, clear transactions, mirror base/inbox, input gestures,
  unfinished requests, config cache and old capabilities as specified.
- Events carry a session generation; an old generation cannot update the new
  session.

Implemented in R04/R06: disconnect/replace/switch clear transactions, mirror
inbox and base, gestures, config cache and deadlines through one
`clear_link_state`; serial `close` drops the OS handle and rejects post-close
I/O; WebSocket close is idempotent and drops the socket; accepted TCP streams get
bounded timeouts. Remaining: session-generation
tags on queued events and Node async cancellation (R05).

## 8. SDK execution

- Node must not block the JS event loop while waiting for native I/O.
- Under a silent TCP peer, a 10 ms timer must not be delayed more than 100 ms;
  cancellation must settle within 500 ms.
- N-API exposes the same capability DTO and typed errors as Rust; a failed
  connection must not auto-create a FakeDevice.
- The app must use only the public SDK.

Implemented in R05: every public native method now returns a Promise backed by a
libuv worker task; the JS thread only creates the task. A silent-peer handshake
is bounded to 2 s, and `dispose` flips shared atomic flags so an in-flight
handshake aborts on its next poll (transport reads use a 400 ms timeout).
Measured by `native-smoke.test.js` "silent peer does not block the JS event loop
and cancels promptly". After a real endpoint is requested, a failed handshake
does not fall back to a FakeDevice; the next call reports `no active connection`.

## 9. Capabilities and identity

- Missing/zero/duplicate/contradictory device or surface fields have explicit
  rejection rules.
- Effective limits are the intersection of both peers.
- The host never infers a capability from a board name.
- Unknown optional features return `Unsupported`, never a false success.
- `device_id` and `board_id` are distinct: a serial pathname or IP address is not
  an identity.

## 10. Network trust boundary

- Simulator services listen on loopback by default.
- The FNV checksum detects transport corruption, not authentication.
- Exposing a non-loopback listener requires a separate pairing/auth threat model;
  this contract makes no network-security claim.

## 11. Multi-device

- Each Session has its own budget; the Manager also has a total device/budget cap.
- A slow device A never blocks device B; cancelling A does not close B.
- The current Node Manager owns one active Session; multi-device aggregation is
  the caller's responsibility until a shared scheduler is implemented.

## 12. Message support matrix

| Type | Parser | Dispatcher | Notes |
|---|---|---|---|
| HELLO/CAPABILITIES/PING/PONG/ACK/ERROR | yes | yes | baseline |
| FRAME/TILE | yes | yes | range-covered chunk assembly (R02) |
| MIRRORFRAME 0x12 | yes (R02) | device→host, no device handler | mirror emit path tested separately |
| MIRRORCTRL 0x31 | yes (R02) | yes, capability-gated | |
| PACKAGE 0x32 | yes (R02) | yes | package handler remains |
| INPUT/OTA | yes | limited/stateful | |
| TELEMETRY/METRIC/CRASH/BOOTINFO | yes | payload decoding incomplete | reserved |

## 13. Test IDs

| ID | Expected behavior | Status / test |
|---|---|---|
| SIM-T01 | 0x12/0x31/0x32 survive Parser | implemented `runtime/esp32/tests/parser_test.cpp` |
| SIM-T02 | duplicate half frame cannot display | implemented same test |
| SIM-T03 | oversized BEGIN rejected before allocation | implemented same test |
| SIM-T04 | short write resumes BEGIN+CHUNK+COMMIT | implemented `short_chunk_resumes_*`, `short_commit_resumes_*` (R03) |
| SIM-T05 | large package upload stays within window | implemented `large_package_upload_stays_in_the_control_window` (R03) |
| SIM-T06 | per-request deadline | implemented `per_request_deadline_is_not_extended_by_a_new_request` (R04) |
| SIM-T07 | Node silent peer does not block timer | implemented `native-smoke.test.js` silent-peer timer/cancel test (R05) |
| SIM-T08 | receive burst budget | open (R07) |
| SIM-T09 | control decode >4 KiB rejected before decode | open (R07) |
| SIM-T10 | peer close converges state | implemented `transport_close_converges_away_from_ready` (R04) |
| SIM-T11 | disconnect drops old mirror frames | implemented `disconnect_drops_the_mirror_inbox_and_base` (R04) |
| SIM-T12 | unsolicited Capabilities cannot set Ready | implemented `unsolicited_capabilities_do_not_make_a_session_ready`; Node events also stamp `generation` (R04) |
| SIM-T13 | 100000 ACKs do not grow history | open (R07) |
| SIM-T14 | close/cancel/limits across transports | partial: memory/TCP/WS/serial close idempotence, post-close rejection, accepted-stream timeouts, WebSocket limit (R06); Serial PTY close matrix and 500 ms cancel settle open (R05/R08) |
| SIM-T15 | multi-device isolation | open (R08) |
| SIM-T16 | Rust/C++ differential byte boundaries | partial shared vectors; open (R02/R08) |

This contract is additive to `docs/protocol.md`; a protocol change must update
both files and the shared vectors.
