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

## Non-goals

- No on-device image decode (the atlas arrives as RGB565).
- No animation player on the device yet; the device records the grid, it does not
  yet index frames against a clock.
- No physical device required; D05/G03 remain open.
