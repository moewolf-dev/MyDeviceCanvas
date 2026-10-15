# Live2D motion3 curves

Status: **sim-verified** (host logic); physical playback is `physical: pending`.

This documents the `motion3.json` curve support added in N213–N217. It is a
**host-side parser and scalar sampler**, not a `.moc3` deformer runtime.

## What is parsed

`Live2DImporter::import_motion` reads one `motion3.json` into a `MotionInfo`:

| Field | Source |
|---|---|
| `name`, `duration_sec`, `fps`, `fade_in_sec`, `fade_out_sec`, `looping` | `Meta` (existing) |
| `channels` | `Curves.len()` |
| `curves` | N213: one `MotionCurve` per `Curves[]` entry |

`MotionCurve { target, id, segments }`:

- `target` — `Parameter` or `Model`.
- `id` — e.g. `ParamAngleX`.
- `segments` — the raw Cubism number stream, capped at `MAX_MOTION_SEGMENTS`
  (4096 floats). A longer stream is truncated rather than refused (it is a
  preview budget, not an asset).

The manifest importer still treats a `model3.json` motion group as metadata; the
curve data comes from the supplied `motion3.json` files in `Live2DBundle.motions`.

## Segment format

`Segments` is a start value followed by typed segments:

```text
Segments[0]            = value at t = 0
then repeated:
  type 0 (linear)      : t, v
  type 1 (bezier)      : c1.t, c1.v, c2.t, c2.v, t, v
  type 2 (step)        : t, v          # hold the previous value until t
  type 3 (inverse step): t, v          # jump to v immediately
```

## Sampling (`MotionCurve::sample`, N214)

`sample(time)` walks the segments and interpolates the one that contains `time`:

- `LINEAR` — linear interpolation between the previous and segment value.
- `STEP` — the previous value until the segment end.
- `INVERSE_STEP` — the segment value immediately.
- `BEZIER` — solve the cubic in `time` by bisection (24 iterations), then
  evaluate the value cubic.

Before `t = 0` the start value is returned; past the last segment the final value
is held. A malformed or truncated stream returns the running value and never
panics.

## CLI

```sh
mdc motion idle.motion3.json                 # list curves + metadata (JSON)
mdc motion idle.motion3.json --param ParamAngleX --samples 8
```

## Non-goals

- No `.moc3` mesh/deformer/parameter binding: the curve is a scalar, not a
  deformation of a mesh.
- No device-side Live2D runtime; a Live2D model still bakes to a sprite atlas
  (weak device) or is driven through its glTF-exported form where one exists.
- No physical-board playback claim; D05/G03 remain open.
