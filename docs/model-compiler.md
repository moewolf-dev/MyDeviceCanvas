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
| `GltfImporter` | glTF 2.0 `.gltf` (JSON) and `.glb` (binary container) | nodes (+ TRS/matrix), meshes, accessors, materials, textures, animations (+ timed channels), `skins` (+ joint nodes, inverse bind matrices), `JOINTS_0`/`WEIGHTS_0`, VRM humanoid; buffer/bufferView/accessor bounds; embedded PNG/JPEG header for real pixel size | pixels (no image decode), external buffer files, morph targets, VRM spring bones |
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
- N109: a node's local transform is read from `translation`/`rotation`/`scale`
  (defaults identity) or decomposed from a `matrix`. `skins[].joints` becomes
  `Skeleton.joint_nodes`; `inverseBindMatrices` (MAT4 f32, column-major) becomes
  `Skeleton.inverse_bind`. A mesh primitive's `JOINTS_0` (u8/u16 VEC4) and
  `WEIGHTS_0` (f32 or normalized u8/u16 VEC4) become `Mesh.skin_joints` /
  `skin_weights`, parallel to the raw `Mesh.preview_positions`.
- N110: each animation channel becomes an `AnimationTrack` (node, path,
  interpolation, `times` + flat `values`, f32 only). Missing or non-f32 sampler
  data is skipped rather than guessed; animation metadata (`channels` count)
  still imports for a track-less stub.
- N113: `TEXCOORD_0` (VEC2 f32, or normalized u8/u16) becomes `Mesh.preview_uv`,
  parallel to `preview_positions`. Empty means a solid-colour bake.
- N163: a primitive with embedded bytes also imports bounded **full geometry**
  into `Mesh.geometry` — POSITION/indices/`TEXCOORD_0`/`JOINTS_0`/`WEIGHTS_0`,
  capped at `MAX_GEOMETRY_VERTICES` (4096) and `MAX_GEOMETRY_INDICES` (16384). The
  preview stays a 64-vertex summary; geometry is the real mesh.
- N174: `pbrMetallicRoughness.baseColorFactor` becomes `Material.color`
  (8-bit RGB; alpha ignored); `Material::color_rgb565()` packs it. The geometry
  and preview renders prefer this colour over their fallback when no texture is
  sampled. N177: a mesh package may also carry a `TextureBlob` (RGB565 + dims),
  built by `bake_texture_blob` / `mesh_package_with_texture` and sampled by a
  strong device through the geometry UVs (`mdc package --texture tex.png`).
- N123: a primitive's `targets[].POSITION` morph deltas (capped at
  `MAX_MORPH_TARGETS`, 16 per mesh) become `Mesh.morph_targets`, and
  `mesh.weights` becomes `Mesh.morph_weights`. N124: an animation channel with
  path `weights` becomes an `AnimationTrack` whose `values` are a flat
  `key × target` scalar stream (a channel that is not a clean multiple is
  skipped).

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

## Skeletal skinning bake (N109–N112)

Once a glTF skin is present, `mdc-bake` deforms the preview mesh instead of
cycling a source image:

- `pose_world_matrices(nodes, animation, time)` samples each transform channel
  (`STEP`, `LINEAR`, slerp for rotation; `CUBICSPLINE` sampled as `LINEAR` on the
  key value) into per-node local matrices and composes them through the node
  hierarchy (`world = parent_world × local`; a cycle cannot loop forever).
- `skin_preview(mesh, skeleton, worlds)` runs linear blend skinning: each preview
  vertex is `Σ wᵢ · (world(jointᵢ) × inverseBindᵢ) · position`, normalized by the
  weight sum. A vertex with no usable weight keeps its bind position.
- `bake_skinned_mesh(model, animation, frames, sprite, color)` samples the
  animation's time span into `frames` poses, projects the deformed points into the
  sprite and rasters them (triangle list when `preview_indices` is present, else a
  fan). `bake_skinned_to_package` seals the frames as one `bake-mesh` clip in a
  `BakedSprites` package.
- `mdc bake-mesh <model.gltf> [--frames N] [--sprite N] [--animation N] [--texture tex.png] [--out DIR] [--package OUT.mdcpkg]`
  writes `mesh_frame_NN.ppm` files or a runtime package, reporting `skinned` /
  `textured` / `morphTargets`.

Morph targets deform before skinning: `apply_morphs(base, targets, weights)`
adds `Σ wᵢ · deltaᵢ`; `morph_weights_at(animation, node, time)` samples a
`weights` channel (component count derived from the value/key ratio). Missing or
extra weights are handled without guessing — an out-of-range weight is ignored.

This is a host-side approximation: only the capped preview vertices are deformed,
there is no texture lookup, and it is not a full skinned-mesh device renderer.

## Textured bake (N113–N117)

`TextureMap` holds RGB565 pixels and samples UVs with a wrapping nearest-
neighbour lookup. `raster_textured` draws the mesh triangles, interpolating UVs
barycentrically and sampling the texture per pixel; a mesh with no UVs (or no
texture) fills a solid fallback colour instead — the UV-missing path never
silently samples texel 0.

- `bake_textured_mesh(model, animation, frames, sprite, texture, color)` runs the
  skinning bake and samples the texture through `Mesh.preview_uv`.
- `bake_textured_to_package` seals the frames as one `bake-mesh` clip.
- `decode_rgb565_native(bytes, limits)` decodes a PNG/JPEG at its native size
  (read-header guard first) for the CLI `--texture` path.

```text
mdc bake-mesh rig.glb --frames 8 --sprite 64 --texture skin.png --package rig.mdcpkg
```

Offline single-frame rendering (no device):

```text
mdc render-mesh rig.glb --texture skin.png --time 0.5 --sprite 64 --out frame.ppm
mdc render-package rig.mdcpkg --frame 2 --out frame.ppm
```

`render_frame_at(model, animation, time, sprite, texture, color)` poses the model
at one time (morph → skin → project → raster); `mdc render-mesh` wraps it and
`mdc render-package` extracts one atlas frame from a sealed `MdcPackage`.

When a mesh carries full geometry, `render_geometry_at` skins `Mesh.geometry`,
fits the bind-pose XY box into the target (aspect preserved, Y flipped, centered
so frames do not jitter) and rasterises the real triangle list with UVs.
`bake_geometry_frames` / `bake_geometry_to_package` do the same across an
animation. `mdc render-mesh` uses the geometry path automatically when present;
the device double (`FakeDevice`) projects and draws the accepted geometry too.

The package gate validates a baked atlas against its whole `columns × rows`
grid, so a padded grid (frame count not a perfect square) is accepted while a
mis-sized pixel buffer is still rejected.

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

`cargo test -p mdc-model` covers: glTF JSON + GLB import, node TRS/skin
(joints, weights, inverse bind) import, timed animation tracks, PNG/JPEG header
probing, accessor/bufferView overrun rejection, malformed GLB rejection, a
decode-bomb refusal, Live2D manifest import, version/moc rejection, part budget,
weak→sprite / strong→mesh degradation, capability auto-selection, and JSON
round-trip of both packages. `cargo test -p mdc-bake` adds the skinning math
(TRS composition, posed joints), `skin_preview`, `bake_skinned_mesh` frame
distinctness and the `bake_skinned_to_package` round trip. All are host logic; no
physical claim.
