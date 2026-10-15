# Render Topology Decision (N01)

Status: **architecture** (decision document, no runtime code). N03–N05 must not
start before this file exists.

This document resolves the contradiction recorded in the 2026-10-13 audit: the
2026-10-09 architecture assumed Host push only, while the memory/rendering plan
assumed device-side rendering with a mirror back to the Mac. Both are real
product modes. They are **parallel, Capability-negotiated modes**, never an
implicit assumption and never a `if board == x` branch.

## 1. Decision

| | Form A — Host render → push frames | Form B — Device render → mirror back |
|---|---|---|
| Who renders | Mac produces the final image | Device renders locally |
| Wire direction | Host → Device (Frame / Tile) | Device → Host (MirrorFrame) |
| Device role | Passive presenter | Compute + presenter |
| Typical boards | Weak MCU (ESP32-S3), e-ink | Linux/Jetson class with GPU |
| Host role | Compose + downscale + push | Compile model + shadow buffer + OBS |
| 0.1 state | Implemented | Not implemented (0.6, tasks N03–N05) |

A single device advertises which modes it supports through Capabilities. A host
selects a mode by intersecting its own support with the device's advertised
`mirror` capability. Form A remains the universal baseline: every device must
present Frame/Tile. Form B is additive.

## 2. Capability model (normative for N03)

`Capabilities` gains three top-level fields plus one nested `mirror` map. All are
optional on the wire (`serde(default)`) so older devices that never sent them
still decode with mirror disabled.

```text
Capabilities:
  ...existing fields...
  local_display_fps: u16          # device LCD refresh; may differ from mirror_fps
  mirror_fps: u16                 # device→host mirror ceiling; 0 means no mirror
  mirror_bandwidth: u64           # advertised mirror budget, bytes/sec; 0 = unknown
  mirror:
    mode: [string]                # subset of ["full-frame", "dirty-region"]
    max_fps: u16
    max_width: u16
    max_height: u16
    pixel_formats: [string]       # e.g. ["RGB565"]
    recommended_transport: Optional[string]
  renderer: Optional[               # N17, optional on the wire
    sprite_2d: bool
    mesh2d: bool
    mesh3d: bool
    skeleton: bool
    max_texture: u32
    max_vertices: u32
  ]
```

Rules:

1. `mirror.mode` empty (or `mirror_fps == 0`) means **Form B unsupported**. The
   host must reject a mirror start with `Unsupported`; it must not send
   `MirrorFrame`.
2. `local_display_fps` and `mirror_fps` are independent. A board may display at
   30 FPS and mirror at 10 FPS. Neither is a promise for other boards.
3. The negotiated mirror FPS is `min(host_request, device.mirror.max_fps,
   device.mirror_fps)`. The negotiated geometry is
   `min(request, max_width, max_height)`; scaling to the shadow buffer is a host
   responsibility.
4. Mode intersection: a request for `dirty-region` is downgraded to `full-frame`
   when the device only advertises `full-frame` (3D/Live2D defaults to
   `full-frame`, see §5). If the device advertises neither, reject.
5. `mirror_bandwidth` is an advisory upper bound, not an FPS promise. FPS is
   measured from displayed ACKs, never from a theoretical link rate.

## 3. Form A data flow (implemented)

```text
Mac frame (RGB565/JPEG source)
        │  host: scale + convert to RGB565_LE
        ▼
  mdc-core Session ── FRAME(begin/chunk/commit) or TILE ──▶ Device
                                                              │
                                             display flush ───┤
                                                              ▼
                                                       ACK(received)
                                                       ACK(displayed)
```

Failure/fallback:

- Device lacks Tile → host sends full Frame only.
- Device lacks Frame → not a valid display device (rejected at HELLO).
- Link too slow / queue full → latest-frame-wins replaces the pending frame; the
  in-flight transaction is never truncated.

## 4. Form B data flow (0.6 target)

```text
Source model (Live2D / glTF / VRM)
        │  mdc-model: Importer → MDC IR → Target backend
        ▼
  Device Runtime Package ──(provision/asset)──▶ Device
                                                 │
                                       device renderer
                                                 ├──▶ local LCD (local_display_fps)
                                                 └──▶ MIRRORFRAME (mirror_fps)
                                                          │  device→host, binary
                                                          ▼
                                            mdc-mirror Shadow Framebuffer
                                            (front/back BGRA8888, bounded)
                                                          ├──▶ OBS source
                                                          └──▶ preview
```

Message flow (host-initiated):

1. Host reads `Capabilities.mirror`. If unsupported → stop (Form A only).
2. Host sends `MIRROR_CTRL { action: "start", surface_id, mode, max_fps, format }`.
3. Device streams `MIRRORFRAME` (phase 0 full / phase 1–3 chunked), identical
   binary framing to `FRAME`, with its own `frame_id` sequence and an
   `ACK(received)` per committed frame. It does **not** wait for a displayed ACK
   from the host.
4. Host may send `MIRROR_CTRL { action: "stop" }` or change `max_fps` at any
   time; the device applies the new ceiling on the next frame.
5. On reconnect the host re-reads Capabilities and restarts the mirror if it was
   active. The shadow buffer keeps the last complete frame across the gap so OBS
   does not go black during a re-handshake.

Failure/fallback:

- Mirror frame dropped/corrupt → shadow buffer keeps the previous complete frame;
  the drop counter increments. OBS never receives a partial frame.
- Bandwidth pressure → device lowers `mirror_fps` (never local display FPS) and
  reports the new ceiling on the next Capabilities refresh.
- Device stops advertising mirror after an upgrade → host falls back to Form A
  and surfaces a diagnosable event; no unconditional downgrade.

### 4.1 Device-side mesh rendering (simulator, N193+)

The Form B "device renderer" is exercised by `mdc-simulator`: on `PACKAGE`
commit it keeps the whole `DevicePackage`. When that package carries mesh
geometry, `render_preview` / `emit_mirror_frame` call
`mdc_bake::render_package_scene(package, time, size)` at the device clock — every
mesh node is skinned (or placed by its node world transform), posed by the
package's first animation, and drawn with its material colour / the package
texture, against a bind-pose box so the mirror does not jitter. This is a host
test double of a strong (mesh-capable) device, not the ESP32 path (weak devices
still receive a baked sprite atlas, Form A). It reuses `mdc-bake` and so is not a
separate renderer implementation; a real device would implement the same math in
its own runtime.

## 5. Mode selection and defaults
- Default mode is decided by capability intersection, not by board family.
- `full-frame` is the safe default for 3D/Live2D/mesh deformation because
  computing dirty regions costs more than sending the frame on those pipelines.
- `dirty-region` is an optional optimization for pixel characters, clocks, and
  mostly-static UI. A `dirty-region` device still accepts `full-frame`; a
  `full-frame` device cannot accept `dirty-region`. `MirrorReceiver` implements
  the host side: a dirty commit patches the last complete base frame (pixel-
  aligned ranges only) and a dirty update before any base frame is dropped.
- Mixed surfaces are allowed: a device may mirror `main` and present `overlay`
  locally. `MIRROR_CTRL` is per `surface_id`.

## 6. Memory budgets

Host side (Form B, in `mdc-mirror`, counted by the allocation ledger):

| Item | Budget | Note |
|---|---|---|
| Shadow front buffer | `w*h*4` (BGRA8888) | 480×480 → 900 KiB |
| Shadow back buffer | `w*h*4` | second buffer for pacing |
| Total shadow | `2*w*h*4` ≤ **8 MiB hard cap** | 480×480 → ~1.8 MiB |
| Mirror receive staging | ≤ 1 MiB | one in-flight frame, latest-wins |
| OBS publish copy | consumer-owned | no third copy inside the library |

Device side (Form B) is separate and lives in the board profile: one render
buffer can feed both the LCD DMA and the mirror sender (single source, two
consumers), so the device does not double its framebuffer. Its budget is
recorded per board; it is never inferred from the host budget.

## 7. OBS delivery

`mdc-mirror` exposes a consumer interface, not an OBS dependency:

```text
pub trait MirrorSink {
    fn on_frame(&mut self, frame: &ShadowFrame);  // BGRA8888, immutable during call
}
```

- The sink borrows the front buffer for the call duration; the library reuses the
  buffer only after every lease is released.
- An OBS plugin/adapter (later wave) or the Node/Electron demo implements
  `MirrorSink`. `mdc-mirror` never links OBS, Electron, or Tauri.
- Reference sinks ship in `mdc-mirror`: `PpmSink` encodes the leased frame as a
  binary PPM, and `QueueSink` copies into a bounded latest-wins `PublishQueue`
  for a consumer slower than the device.
- Frame pacing is a host concern: the sink may be called with the same front
  buffer more than once (OBS 30 FPS vs device 15 FPS); the shadow buffer is the
  pacing boundary between the device clock and the OBS clock.

## 8. Versioning

- `MIRRORFRAME` and the `mirror` capability are a **v1.x minor addition**. No
  existing field is renamed.
- Older devices remain Form A only; they are not broken by the new types.
- Unknown message types are still rejected before payload allocation.

## 9. Non-goals

- No Scene/3D runtime on the host is added here; the host only compiles models
  (N05) and buffers mirrored frames (N04).
- No OBS binary integration in this wave.
- No physical device is required for N01–N07; all are `sim-verified` or
  `physical: pending` per the honesty rules.

## 10. Consequences for the N-series

| Task | Depends on this decision for |
|---|---|
| N03 | capability fields, `MIRRORFRAME` type, mode intersection rules |
| N04 | shadow buffer size, `MirrorSink`, frame lifecycle |
| N05 | target backend selection driven by `mirror` / renderer capability |
| N02 | board facts for Linux/Jetson Form B boards |
