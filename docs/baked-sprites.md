# Baked Sprites (Form B delivery)

Status: **sim-verified** (host logic); physical delivery is `physical: pending`.

This closes the loop between baking and delivery: a source model or texture set
is baked into a sprite atlas, sealed into an MDC runtime package, uploaded to a
device over the `PACKAGE` path, and the device records the render grid.

## Pipeline

```text
source images ── mdc-bake ──▶ baked atlas (RGB565) ──▶ AtlasBlob
                                   │                        │
                                   ▼                        ▼
                            atlas.json manifest      DevicePackage{mode: baked-sprites}
                                                            │  MdcPackage::seal
                                                            ▼
                                        PACKAGE begin/chunk/commit ──▶ device
                                                            │  accepted_by(sprite caps)
                                                            ▼
                                             package_ready + atlas grid recorded
```

## Commands

```sh
# Bake a preview atlas (atlas.ppm + atlas.json)
mdc bake a.png b.png --frames 8 --sprite 64 --out ./baked

# Bake straight into a runtime package and install it on a simulator board
mdc bake a.png --frames 8 --sprite 64 --package baked.mdcpkg
mdc --board esp32-jc3248w535-sim install-package baked.mdcpkg
```

## Guarantees

- **Bounded**: the baker reads PNG/JPEG dimensions before decoding (a bomb is
  refused before allocation), and the package body is capped by
  `MAX_PACKAGE_BYTES`.
- **Gated**: a baked package is only accepted by a device that advertises sprite
  support; the atlas pixel length must match `sprite_w × sprite_h × frames × 2`.
- **Integrity**: `MdcPackage` checksum + length are verified on receive.
- **No silent accept**: a wrong atlas size or a non-sprite device is refused with
  `Unsupported`; the host surfaces the error.

## Device playback

A baked package carries one clip per motion (`AtlasBlob.clips`: name, start,
frames, fps, loop, fades). The device plays a clip on request through the
`animation` configuration key:

```sh
mdc --board linux-virt install-package baked.mdcpkg
mdc --board linux-virt config set animation idle    # play
mdc --board linux-virt config set animation off     # stop
```

While a clip runs, the device indexes the current frame from its clock
(`current_atlas_frame`), renders it into the local framebuffer, and — when
mirroring — emits it as a `MIRRORFRAME` so the host/OBS sees the device-rendered
result. An unknown clip or a device with no atlas is refused. `animation_loops()`
reports how many times a looping clip has wrapped.

N154: switching from one clip to another cross-fades the frame frozen at the
switch over the new clip's `fade_in_ms` (capped at 2 s), so a transition does not
snap. A zero `fade_in_ms` switches immediately; `config set animation off` clears
the frozen frame.

## Non-goals

- No on-device image decode (the atlas arrives as RGB565).
- No skeletal/morph playback on the device: the host bakes poses to frames and the
  device indexes them (strong devices receive a mesh package instead).
- No physical device required; D05/G03 remain open.
