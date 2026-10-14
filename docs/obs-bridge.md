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
                                                           └─▶ MirrorSink impl  (preview / Node / Swift)
```

## Delivery options

| Option | API | Consumer |
|---|---|---|
| Atomic PPM file | `mdc_mirror::PpmDirectorySink` | OBS "Image Source" watching `<dir>/frame.ppm` (or a `_0000..N` ring) |
| TCP fan-out | `mdc_mirror::MirrorServer` | A local reader / browser source; one PPM frame per client, latest-wins |
| Node | `onMirrorFrame` + `takeMirrorFrame` | Electron preview (see `examples/electron-demo`) |
| Swift | `mirrorFrames()` `AsyncStream` | macOS app consumer |
| C | `mdc_take_mirror_frame` | any C host |

## CLI

```sh
# Drive mirroring on a simulator board and write frames for an OBS image source.
mdc --board linux-virt mirror --frames 30 --write-dir ./mirror-out

# Or fan out over TCP for a local consumer.
mdc --board linux-virt mirror --frames 30 --serve 127.0.0.1:7777
```

The command prints JSON stats (`mirror_received`, `mirror_dropped`,
`ppm_written`, `served_clients`, `served_dropped`).

## Guarantees

- **Bounded**: the shadow buffer is capped at `MAX_SHADOW_BYTES` (8 MiB); the PPM
  sink writes through a temp file + atomic rename; the server queues at most one
  frame per client and drops older frames (latest-wins) so a slow consumer never
  grows memory.
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
