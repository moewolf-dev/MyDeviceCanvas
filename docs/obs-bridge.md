# OBS Bridge (Form B)

Status: **sim-verified** (host logic); physical board delivery is
`physical: pending`. Form B is the device-renders → mirrors back mode
(`docs/render-topology-decision.md`). This document describes how the mirrored
frames reach a consumer such as OBS. `mdc-mirror` never links OBS itself.

## Data path

```text
Device renderer ── MIRRORFRAME ──▶ Session inbox ──▶ MirrorReceiver
                                       │                   │
                                       │            ShadowFramebuffer (BGRA, bounded)
                                       ▼                   │
                              ManagerEvent::Mirror         ├─▶ PpmDirectorySink  (file watcher)
                              (metadata, no pixels)        ├─▶ MirrorServer      (TCP fan-out)
                                                           ├─▶ HttpMirrorServer  (OBS browser source)
                                                           └─▶ MirrorSink impl  (preview / Node / Swift)
```

## Delivery options

| Option | API | Consumer |
|---|---|---|
| Atomic PPM file | `mdc_mirror::PpmDirectorySink` | OBS "Image Source" watching `<dir>/frame.ppm` (or a `_0000..N` ring) |
| BMP file | `mdc_mirror::encode_bmp` + `mdc mirror --format bmp` | OBS "Image Source" watching `<dir>/frame.bmp` |
| PNG file | `mdc_mirror::encode_png` + `mdc mirror --format png` | OBS "Image Source" watching `<dir>/frame.png` (N328, no compression dependency) |
| HTTP | `mdc_mirror::HttpMirrorServer` | OBS "Browser Source" at `http://127.0.0.1:<port>/stream` (live `multipart/x-mixed-replace` push) or `/` (auto-refreshing `<img>`); `/frame.bmp` and `/frame.ppm` for scripts |
| TCP fan-out | `mdc_mirror::MirrorServer` | A local reader / browser source; one PPM frame per client, latest-wins |
| Node | `onMirrorFrame` + `takeMirrorFrame` | Electron preview (see `examples/electron-demo`) |
| Swift | `mirrorFrames()` `AsyncStream` | macOS app consumer |
| C | `mdc_take_mirror_frame` | any C host |

## CLI

```sh
# Drive mirroring on a simulator board and write frames for an OBS image source.
mdc --board linux-virt mirror --frames 30 --write-dir ./mirror-out
mdc --board linux-virt mirror --frames 30 --write-dir ./mirror-out --format bmp

# Serve the latest frame to an OBS Browser Source (live push + a scriptable still).
mdc --board linux-virt mirror --frames 300 --http 127.0.0.1:8080
#   http://127.0.0.1:8080/stream      -> live multipart/x-mixed-replace (BMP parts, N313)
#   http://127.0.0.1:8080/stream.jpg  -> live multipart/x-mixed-replace (JPEG parts, N337)
#   http://127.0.0.1:8080/            -> auto-refreshing page
#   http://127.0.0.1:8080/frame.bmp   -> image/bmp
#   http://127.0.0.1:8080/frame.ppm   -> image/x-portable-pixmap
#   http://127.0.0.1:8080/frame.png   -> image/png (N328)
#   http://127.0.0.1:8080/frame.jpg   -> image/jpeg (N336)
#   http://127.0.0.1:8080/health      -> JSON counters (N361)
# Optional query on /frame.*: ?scale=2 (integer 1..=8), ?quality=70 (JPEG 1..=100, N346)
# /stream[.jpg]?scale=2 downscales each streamed part (N356)

# Or fan out over TCP for a local consumer.
mdc --board linux-virt mirror --frames 30 --serve 127.0.0.1:7777
```

The command prints JSON stats (`mirror_received`, `mirror_dropped`,
`ppm_written`, `served_clients`, `served_dropped`, `http_frames`).

`HttpMirrorServer` is a small HTTP/1.1 responder with no JPEG dependency: it
caches the newest frame as BMP (bottom-up 24-bit) and PPM, accepts at most
`max_clients`, and answers one short-lived request per connection. A slow reader
never blocks `pump`; it simply receives the frame that is current when it asks.
`/stream` (N313) keeps the connection open and pushes a BMP part per published
frame (`Content-Type: multipart/x-mixed-replace; boundary=mdcframe`); `/stream.jpg`
(N337) pushes JPEG parts instead. Each streaming client buffers at most one part,
and frames that arrive while a slow client is still writing are dropped
(`clipped_frames()`), so `pump` never blocks and memory stays bounded. N336 adds a
dependency-free baseline JPEG encoder (`encode_jpeg`) served at `/frame.jpg` and
`mdc mirror --format jpg --quality N`. N346: the one-shot `/frame.*` endpoints
accept `?scale=N` (nearest integer downscale, 1..=8) and `?quality=N` (JPEG) from
the raw BGRA frame. N486 adds `?flip=h|v|hv` to mirror a frame. N500 adds `/ws`,
a WebSocket endpoint (RFC 6455) that pushes one binary JPEG message per published
frame — use it from a browser or any WebSocket client when multipart is awkward.
N507 answers client ping frames with a pong (echoing the payload) and closes the
connection cleanly on a close frame; `/health` reports `ws`, `ws_pings`, and
`ws_closes`. N517 adds a heartbeat: `mirror --http ADDR --ws-heartbeat N
[--ws-timeout M]` pings every N published frames and drops a peer that has been
silent for M frames (`/health` also reports `ws_timeouts`). N527 adds
`--ws-fragment N`, which splits each WebSocket message into a first binary frame
plus continuation frames of at most `N` bytes (the client reassembles them).
N537 floors `N` at 64 bytes and keeps heartbeats out of the middle of a message
(control frames are written only once the fragments have drained). N637 clamps a
note's `detail` to `WS_NOTE_DETAIL_MAX` (256 bytes) and N647 exposes
`ws_log_counts()` (the retained notes per kind). N657 exposes that map as
`ws_log_kinds` in `/health` (with `ws_log` staying the entry count); the
`mdc mirror` JSON summary uses the same split. N548 also
reassembles **client → server** messages: a text/binary message split across
fragmentation and continuation frames is completed (control frames may interleave),
bounded at 64 KiB per message, and counted in `/health`'s `ws_messages` field and
`mdc mirror`'s `ws_messages`. N558 handles the reserved (compression) bit:
`permessage-deflate` is never negotiated, so a frame that sets it is counted
(`ws_reserved`) and ignored instead of being mis-read as plain data. N567 records
extension offers (`ws_offered`, never echoed) and, with
`--ws-reject-reserved` / `with_ws_reject_reserved(true)`, fails the connection
with a `1002` close instead of ignoring the frame. N577 keeps a bounded
`ws_log()` of these decisions (`declined extension: …`,
`reserved bit set: closed 1002`); `/health` reports `ws_log` (a count),
`ws_log_last` (an escaped, 48-char summary of the latest note) and `mdc mirror`
reports `ws_log` (the entries). N597/N607: each entry is a structured
`{ kind, detail }` (`extension`/`reserved`), and an extension `detail` is its
token only (`permessage-deflate`), never its parameters, so a peer address or
window size never enters the log. N617: `WS_NOTE_VERSION` pins the entry shape
and `ws_log_of_kind(kind)` filters the log. N667: `/health`'s `ws_log_last` is escaped by the shared `escape_json`, so a
host-supplied note with quotes/newlines cannot break the body. N677: a note's `kind` is clamped to `WS_NOTE_KIND_MAX` and the `/health`
kind map reports at most `WS_LOG_KINDS_MAX` kinds in sorted (stable) order. N687: the cap is configurable with
`with_ws_kind_limit(n)` (clamped to `1..=WS_LOG_KINDS_MAX`). N697: `mdc mirror` exposes the same `ws_log_kinds` object in its JSON summary. N707: `/health` also carries `ws_log_entries`, the oldest-first array of `{kind, detail}` notes (bounded by `WS_LOG_MAX`, strings escaped), so a monitor sees the reasons rather than only counts. N717: `/health?kind=extension` narrows only `ws_log_entries` to that kind (an unknown kind yields `[]`); the counter fields stay global.

## Guarantees

- **Bounded**: the shadow buffer is capped at `MAX_SHADOW_BYTES` (8 MiB); the PPM
  sink writes through a temp file + atomic rename; the TCP server queues at most
  one frame per client and drops older frames (latest-wins); the HTTP server
  caches a single BMP+PPM pair and refuses to cache a pair over the cap. A slow
  consumer never grows memory.
- **Pacing boundary**: the host, not the device, decides how often a consumer is
  fed; the same front buffer may be published more than once (device 15 FPS vs
  OBS 30 FPS).
- **Staleness**: a dropped or corrupt mirror frame leaves the previous complete
  frame in place; a consumer never sees a partial frame.
- **Capability-gated**: mirroring only starts when the device advertises a mode
  (`MIRRORCTRL` is refused otherwise). An absent capability falls back to Form A.

## Non-goals

- No OBS binary/plugin integration in the library (`mdc-mirror` links no OBS).
- No dirty-region default for 3D/Live2D (full-frame is the safe default).
- No physical-board delivery claim; D05/G03 remain open until a real device is
  measured.
- 60-minute process RSS is out of scope; the simulator soak is
  `cargo test -p mdc-memory-tests`.
