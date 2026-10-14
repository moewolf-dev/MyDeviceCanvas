# Model Compiler (N05 skeleton, deepened in N08)

Status: **architecture / sim-verified** (host logic only; no physical board).

`mdc-model` is the host-side Model Compiler. It turns a source character model
into a device package. Source formats never reach the device runtime: the device
only ever receives an MDC Runtime Package (`docs/render-topology-decision.md`
§4). This crate depends on serde only — it never depends on `mdc-core`.

## Pipeline

```text
Source model (glTF / GLB / Live2D model3.json)
        │  Importer
        ▼
      MDC IR  (Node / Mesh / Texture / Material / Skeleton / Animation / Sprite / Layer)
        │  Target backend (chosen by renderer capability)
        ▼
  Device package (serde JSON), consumed by provisioning / assets
```

## Importers

| Importer | Input | Reads | Does **not** read |
|---|---|---|---|
| `GltfImporter` | glTF 2.0 `.gltf` (JSON) and `.glb` (binary container) | nodes, meshes, accessors, materials, textures, animations, `skins`, VRM humanoid; buffer/bufferView/accessor bounds; embedded PNG/JPEG header for real pixel size | pixels (no image decode), external buffer files, morph targets, VRM meta/spring bones |
| `Live2DImporter` | Cubism `model3.json` (Version 2–5) and `motion3.json` bundle | textures, motion groups, expressions, moc reference; motion curve count + `Meta` (duration/fps/loop/fade) | `.moc3` binary (mesh / deformer / parameter data), motion keyframes |

Both importers check `ImportLimits` **before allocating**: `max_input_bytes`
(also caps resolved buffers), `max_texture_bytes`, `max_nodes`, `max_vertices`.
A decode bomb is refused before any decode buffer is created.

`Model.meta` carries source metadata: VRM `vrm.title` / `vrm.author` /
`vrm.expressions` (VRM 0.x array and VRMC_vrm 1.0 object forms, plus one
`vrm-expression:<name>` animation each), and Live2D `physics.settings` /
`pose.groups` counts when the bundle supplies `physics3.json` / `pose3.json`.

### glTF details

- `.glb` is split into its JSON and BIN chunks; truncated headers, a bad version,
  and over-long chunks are rejected without reading past the declared length.
- Buffers are resolved from the GLB BIN chunk or a base64 `data:` URI. External
  URIs stay unresolved and are validated against their declared size only.
- Accessors are validated against their bufferView (`count × element size`), and
  bufferViews against their buffer. An overrun is rejected before use.
- Embedded image dimensions come from the PNG IHDR or the JPEG SOF marker; when
  bytes are not on disk the declared `width`/`height` is used.
- `skins` become IR skeletons (joints count); an out-of-range joint index is
  rejected. A VRM humanoid extension (`VRM.humanoid.humanBones` array or
  `VRMC_vrm.humanoid.humanBones` map) adds one `vrm-humanoid` skeleton.

### Live2D details

- One texture / material / node / sprite per declared texture; motion groups
  become animations and expressions become `expression:<Name>` animations.
- The `.moc3` is not decoded, so atlas geometry (sprite sizes) is 0 and a weak
  backend simply plays the author sprites.
- `import_motion` reads one `motion3.json` (`Meta` duration/fps/loop/fade + curve
  count) into `MotionInfo`; `import_bundle` compiles the manifest together with
  the supplied motions, replacing the per-group stubs with one animation per
  motion carrying its real curve count. Motion keyframes are not expanded.

## Target backends

Backend selection is **Capability-driven**, never `if board == x`.

| Backend | `target` | Requires | Behavior |
|---|---|---|---|
| `Esp32PlaneBackend` | `esp32-plane` | — | Mesh when the device can draw one, else bake to a sprite atlas (textures clamped to `max_texture`) |
| `LinuxMeshBackend` | `linux-mesh` | `mesh2d` | Keeps meshes, skeletons and animations; a model with no geometry degrades to its author sprites |

- `compile(model, caps)` uses the MCU backend (the N05 entry point).
- `compile_with(&backend, model, caps)` uses an explicit backend.
- `compile_auto(model, caps)` picks the Linux/GPU backend when a device advertises
  `mesh2d && skeleton`, otherwise the MCU backend.
- `RendererCapability::from_wire(caps.renderer)` builds the compiler capability
  from the device's wire `renderer` field (N17); a device that advertised none is
  treated as sprite-only.

A weak device never receives mesh or skeleton data; a GPU package never contains
baked sprites in mesh mode.

## Sprite atlas baker

`AtlasBaker` turns motion metadata into a deterministic `AtlasPlan`: every
motion contributes `ceil(duration × fps) + 1` frames laid on a near-square grid
clamped to the device texture limit. It is a *plan* — no pixels are decoded or
packed — and it is bounded by `max_frames`, `max_atlas_bytes` and the texture
limit before any allocation. An unknown sprite size (0), empty motion set, or an
atlas larger than the texture limit is rejected.

## Pixel baking (`mdc-bake`)

The model compiler plans; `mdc-bake` does the pixels. `decode_resize_rgb565`
reads a PNG/JPEG header first (a bomb is refused before decode), resizes to the
device target and emits RGB565 LE rows. `pack(plan, frames)` lays baked frames on
an `AtlasPlan` grid; `bake_sprites` samples frames from source images, bakes each
and packs one atlas plus a JSON `BakedManifest`. `mdc bake <image>... --out DIR`
writes `atlas.ppm` (P6) and `atlas.json`. `bake_to_package` seals the atlas into
a `BakedSprites` device package for the `PACKAGE` upload path (see
[baked-sprites.md](baked-sprites.md)).

## Optimizer

`plan_optimization(model, caps)` produces an `OptimizerPlan`: per-texture
downscale targets (clamped to `max_texture`) and a vertex decimation ratio
(`1.0` fits, `< 1.0` scales down, `0.0` when the device is sprite-only). It is
metadata only and is bounded by `MAX_OPTIMIZED_TEXTURE_BYTES` before any work.

## Runtime package container

`MdcPackage::seal(package)` wraps a `DevicePackage` in a versioned container:
a `MDCP` magic, a `u16` version, a length-prefixed JSON manifest (`target`,
`mode`, `estimated_bytes`, body length and an FNV-1a checksum) and the JSON IR
body. `compile_to_package(model, caps)` compiles with the capability-selected
backend and seals the result. `to_bytes`/`from_bytes` move it over the wire; the
body and manifest are bounded (16 MiB / 64 KiB). `from_bytes` verifies the
checksum and declared length, and `accepted_by(caps)` is the capability gate: a
mesh package needs `mesh2d`, a sprite atlas needs sprite support — checked
against the advertised capability, never a board id.

`PackageAssembler` rebuilds an `MdcPackage` from upload chunks: it validates the
header (magic/version/manifest length) as soon as the first 10 bytes arrive, is
bounded by `MAX_PACKAGE_BYTES`, and `finish()` runs the full integrity check.

## Memory and limits

The IR is metadata-level: geometry is summarised as vertex/index counts, and
textures as declared/derived sizes. Intermediate buffers are bounded by
`ImportLimits`; the decoded-buffer total is capped by `max_input_bytes`. The
package `estimated_bytes` is advisory (textures for the MCU backend; textures +
12 B/vertex for the GPU backend).

## Non-goals (this wave)

- No pixel decode, texture re-encoding, or sprite-atlas packing.
- No `.moc3`, VRM, Spine or FBX importer.
- No device-side renderer; the compiled package is data only.
- No real Live2D `live2d_*` deformers/parameters in the IR yet.

## Tests

`cargo test -p mdc-model` covers: glTF JSON + GLB import, PNG/JPEG header
probing, accessor/bufferView overrun rejection, malformed GLB rejection, a
decode-bomb refusal, Live2D manifest import, version/moc rejection, part budget,
weak→sprite / strong→mesh degradation, capability auto-selection, and JSON
round-trip of both packages. All are host logic; no physical claim.
