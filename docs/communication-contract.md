# Communication Contract (SIM-R01)

Status: **baseline frozen; SIM-R01–R10 are implemented for the simulator.**
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

- Control payload: 4 KiB maximum; checked by length before CBOR decode.
- Frame: 1 MiB maximum.
- Chunk: `min(local, negotiated max_chunk)` minus this packet's metadata overhead.
- Runtime package: 16 MiB wire maximum (aligned with the C++ runtime); the app's
  local file cap must not be mistaken for a larger wire allowance.
- All offset/length arithmetic is checked. BEGIN lengths must match the declared
  surface geometry before allocation.

Implemented in R07: `decode_control` rejects >4 KiB before `serde_cbor`; chunk
coverage lists are capped at 256 disjoint ranges in both Rust and C++.

## 5. Receive fairness

- Target per poll: at most 64 KiB or 32 packets, whichever comes first.
- RX buffering is bounded; a burst cannot force one poll to consume an unbounded
  stream before parsing or ticking.
- Oversized control messages are rejected by length before allocation/decoding.

Implemented in R07: `Session::poll` reads at most 64 KiB, parses at most 32
packets / 64 KiB, and stops reading when the unparsed buffer reaches 2 MiB.
Reads that arrive larger are parsed over later polls.

## 6. Integrity

- Frame/Package BEGIN declares a length; it must match the legal object and local
  cap before allocation.
- Chunks track covered byte ranges. Identical duplicate chunks are idempotent;
  partial overlaps are rejected.
- COMMIT succeeds only when ranges cover the full expected length. A missing
  chunk never displays zero-filled data.
- A transaction accepts at most 256 disjoint coverage ranges; the next chunk is
  rejected (`E_CHUNK` / `TooManyRanges`) and a later BEGIN starts fresh.
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
- With reconnect disabled, a transport failure is terminal: the device and link
  state are cleared immediately, so the next call cannot read stale capability.
- Events carry a session generation; an old generation cannot update the new
  session.
- Bounded containers (R07): Manager events 128 (move coalescing/critical
  eviction), diagnostics 64, finished-ACK window 4096, displayed timings 256,
  active pointers 16, config requests by the 32-slot control window.
- `Session::container_counts()` reports every bounded container for soaks
  (SIM-R09); the long-run gate samples RSS/threads/FDs and requires owned
  allocations to return to baseline after idle.

Implemented in R04/R06: disconnect/replace/switch clear transactions, mirror
inbox and base, gestures, config cache and deadlines through one
`clear_link_state`; serial `close` drops the OS handle and rejects post-close
I/O; WebSocket close is idempotent and drops the socket; accepted TCP streams get
bounded timeouts.

R06 closes the transport contract with one shared matrix
(`crates/mdc-transport/tests/close_contract.rs`) over Memory, TCP, WebSocket and
a real unix pty: repeated `close`/`cancel` is idempotent and bounded to 500 ms,
every adapter rejects I/O after close, an idle read returns within its timeout,
a short write resumes only the unaccepted tail, and a serial pty can be reopened
after close (the OS handle was actually released). WebSocket message and frame
limits are installed in `WebSocketConfig` at handshake time (2 MiB default) and
`set_max_message` updates the live framing layer, so an oversized inbound frame
is rejected at its header instead of after buffering; the same cap applies to
outbound writes.

## 8. SDK execution

- Node must not block the JS event loop while waiting for native I/O.
- Under a silent TCP peer, a 10 ms timer must not be delayed more than 100 ms;
  cancellation must settle within 500 ms.
- N-API exposes the same capability DTO and typed errors as Rust; a failed
  connection must not auto-create a FakeDevice.
- The app must use only the public SDK.

Implemented in R05: every public native method now returns a Promise backed by a
libuv worker task; the JS thread only creates the task. The native session also
owns a real-clock tick (ack/heartbeat deadlines) and disables reconnect, so a
dead peer becomes a terminal `Transport`/`AckTimeout` error instead of waiting
for a replacement transport the SDK cannot supply. A silent-peer handshake
is bounded to 2 s, and `dispose` flips shared atomic flags so an in-flight
handshake aborts on its next poll (transport reads use a 400 ms timeout).
Measured by `native-smoke.test.js` "silent peer does not block the JS event loop
and cancels promptly". After a real endpoint is requested, a failed handshake
does not fall back to a FakeDevice; the next call reports `no active connection`.

## 9. Capabilities and identity

- Missing/zero/duplicate/contradictory device or surface fields have explicit
  rejection rules.
- Effective limits are the intersection of both peers.
- A Pong only clears a miss when it matches the outstanding Ping request id.
- A Config reply is applied only when its key has an outstanding request; the
  original request slot is released on that reply.
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
| SIM-T06 | per-request deadline | implemented `per_request_deadline_is_not_extended_by_a_new_request`; Pong/Config replies must match an outstanding request (R04) |
| SIM-T07 | Node silent peer does not block timer | implemented `native-smoke.test.js` silent-peer timer/cancel test (R05) |
| SIM-T08 | receive burst budget | implemented `poll_handles_at_most_32_packets`, `poll_handles_at_most_64_kib_per_call`, `poll_stops_reading_after_the_read_budget` (R07) |
| SIM-T09 | control decode >4 KiB rejected before decode | implemented `oversized_control_is_rejected_before_cbor` (R07) |
| SIM-T10 | peer close converges state | implemented `transport_close_converges_away_from_ready` (R04) |
| SIM-T11 | disconnect drops old mirror frames | implemented `disconnect_drops_the_mirror_inbox_and_base` (R04) |
| SIM-T12 | unsolicited Capabilities cannot set Ready | implemented `unsolicited_capabilities_do_not_make_a_session_ready`; Node events also stamp `generation` (R04) |
| SIM-T13 | 100000 ACKs do not grow history | implemented `finished_ack_history_is_bounded` (R07) |
| SIM-T14 | close/cancel/limits across transports | implemented: shared `crates/mdc-transport/tests/close_contract.rs` matrix over memory/TCP/WS/serial pty (idempotent close/cancel ≤500 ms, post-close rejection, bounded idle read, pty handle release), WebSocket limits installed at handshake and on `set_max_message`, bounded accepted TCP reads/timeouts (R06), Serial PTY peer-exit, Node cancel and C++ peer exit (R08) |
| SIM-T15 | multi-device isolation | implemented `one_device_exiting_does_not_close_another` (R08, two processes) |
| SIM-T16 | Rust/C++ differential byte boundaries | shared vectors + `cxx_peer` chunked frame/tile with exact pixel dump (R08) |

## 14. Cross-process fault matrix

`tools/mdc-sim-peer` is the real device process used by
`tools/mdc-sim-peer/tests/faults.rs`. Each case spawns the binary, drives the
public Rust `Session` over a real TCP/WebSocket link, and asserts exact state
and byte-level outcomes:

| Fault | Flag | Expected |
|---|---|---|
| lost request / ACK | `--lose-after N` | `AckTimeout`, no `displayed` ACK |
| delayed request | `--delay-ticks N` | handshake/frame still completes |
| corrupt ACK | `--corrupt-after N` | `Protocol` rejection, no display |
| duplicate ACK | `--duplicate-after N` | one ACK, one timing sample |
| split outbound packet | `--split-after N` | reassembled, one ACK |
| split every write | `--split-all` | full frame still reassembles |
| out-of-order ACKs | `--swap-next-acks` | both surfaces complete once |
| mid-link capability change | `--recaps-after-ms N --recaps-fps F` | new value adopted, link stays Ready |
| peer exit | `--exit-after-ms N` | `Transport` error, leaves Ready |
| half-open/stall | `--stall-after-ms N` | `AckTimeout` |
| device reboot | `--reboot-after-ms N` | new boot/run id, frame recovers |
| multi-device A/B | two peers | A exit leaves B Ready and interactive |
| WebSocket lost request | `--ws --lose-after N` | `AckTimeout`, no ACK |
| Serial PTY corrupt ACK | peer `--corrupt-after N` | `Protocol` rejection on Node |
| exact frame pixels | `--dump-path PATH` | 307200 bytes, every byte matches |
| C++ runtime process | `tools/cxx-peer-check.sh` | chunked frame + tile, exact pixels |
| C++ short BEGIN | `ShortOnce` wrapper | resume completes, one ACK |
| C++ peer exit | kill child | `Transport` error, leaves Ready |

Node drives the same TCP/WebSocket/PTY paths; its silent-peer timer/cancel,
TCP peer-exit, Serial PTY peer-exit and PTY corruption cases live in
`sdk/node/native-smoke.test.js`. The C++ runtime is covered as a separate
process over `runtime/linux/main/mdc_linux_peer.cpp`.

The matrix exposed and fixed a real C++ display bug: after `present()`, the next
partial `flush_rect` wrote into a blank back buffer and erased the rest of the
frame. `SimDisplay::present` now copies the front buffer to the back buffer, and
the C++ peer test asserts both tile rows and the untouched pixels.

This contract is additive to `docs/protocol.md`; a protocol change must update
both files and the shared vectors.
