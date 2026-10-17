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
the raw BGRA frame.

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
