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
| `GltfImporter` | glTF 2.0 `.gltf` (JSON) and `.glb` (binary container) | nodes (+ TRS/matrix), meshes, accessors, materials, textures (embedded bytes retained, N323), animations (+ timed channels), `skins` (+ joint nodes, inverse bind matrices), `JOINTS_0`/`WEIGHTS_0`, VRM humanoid; buffer/bufferView/accessor bounds; embedded PNG/JPEG header for real pixel size | image pixel decode, external buffer files, morph targets, VRM spring bones |
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
  preview stays a 64-vertex summary; geometry is the real mesh. N223: the same
  primitive's morph `targets[].POSITION` deltas become
  `MeshGeometry.morph_targets` (per-target, bounded by `MAX_MORPH_TARGETS`), so a
  blend shape deforms the full mesh, not just the preview. N273/N274: **every**
  primitive with POSITION is merged into one `Mesh.geometry` (`MeshGeometry::merge`
  rebases indices by the running vertex count), so a multi-material mesh keeps its
  full silhouette; `Mesh.primitives` records how many were merged.
- N174: `pbrMetallicRoughness.baseColorFactor` becomes `Material.color`
  (8-bit RGB; alpha ignored); `Material::color_rgb565()` packs it. N283:
  `alphaMode` / `alphaCutoff` become `Material.alpha_mode` / `alpha_cutoff`
  (`is_masked()`, `cutoff()`). N284/N285: a decoded texture can carry a 1-bit
  alpha mask (`TextureMap::with_alpha`); a mesh whose material is `MASK` skips
  texels whose bit is clear (cutout) — the cutoff is applied at decode time
  (threshold 128), not per-material. `mdc render-mesh` / `mdc bake-mesh` decode
  the PNG alpha this way. N293/N294: a `TextureBlob` also carries the mask
  (`alpha_at`), and `render_package_scene` attaches it to the texture, so the
  device double cuts out transparent texels of a mesh package too. The geometry
  and preview renders prefer this colour over their fallback when no texture is
  sampled. N177: a mesh package may also carry a `TextureBlob` (RGB565 + dims),
  built by `bake_texture_blob` / `mesh_package_with_texture` and sampled by a
  strong device through the geometry UVs. N323: the importer retains the embedded
  image bytes (`Texture.data`, bounded by `max_texture_bytes`) and exposes
  `Model::embedded_texture` / `Model::material_texture`. N324: `mdc package`
  auto-uses a material's embedded `baseColorTexture` (`auto_texture_blob` /
  `resolve_texture_blob`); an explicit `--texture tex.png` still overrides, and an
  embedded image that fails to decode is reported (`textureError`) while the
  package is still produced untextured. N333: `alphaMode: BLEND` with
  `baseColorFactor` alpha (index 3) renders as source-over transparency in the
  scene/package/single renders and the device double; `OPAQUE` ignores the alpha
  factor and `MASK` still cuts out. N344: `decode_rgba8_native` /
  `texture_map_with_alpha8` keep the 8-bit alpha channel, and a `BLEND` material
  folds it into the material alpha (`TextureMap::alpha_value_at`) so edges fade
  instead of cutting out; the 1-bit mask still backs `MASK` and the device blob.
  N334: the glTF sampler wrap (`CLAMP_TO_EDGE` → `clamp`) is recorded on
  `Texture.wrap` (`Model::material_texture_wrap`) and applied by
  `TextureMap::with_wrap`; `mdc render-mesh --wrap` overrides. N353:
  `mesh_package_with_textures` / `attach_model_textures` attach one `TextureBlob`
  per material (`DevicePackage.texture_blobs`, indexed by `Material.texture`; a
  missing image is an empty placeholder), and the package-scene renderer samples
  each mesh's own texture (falling back to the single `texture`). `render_package_mesh`
  still uses the single texture.
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
- `import_motion` reads one `motion3.json` (`Meta` duration/fps/loop/fade +
  curve count) into `MotionInfo`; `import_bundle` compiles the manifest together
  with the supplied motions, replacing the per-group stubs with one animation per
  motion carrying its real curve count. Motion keyframes are not expanded.
- N213: `import_motion` also parses each `Curves[].Segments` stream (bounded to
  `MAX_MOTION_SEGMENTS`) into `MotionInfo.curves`. `MotionCurve::sample(t)`
  evaluates linear/step/inverse-step/bezier segments and `MotionInfo::value_at`
  looks a parameter up by id — see [live2d-motion.md](live2d-motion.md). The
  `.moc3` binary is still not decoded, so this is a scalar parameter value, not a
  mesh deformation.

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
  (`STEP`, `LINEAR`, slerp for rotation; N253 `CUBICSPLINE` uses the per-key
  in/out tangents as a cubic Hermite, normalizing a rotation result) into
  per-node local matrices and composes them through the node hierarchy
  (`world = parent_world × local`; a cycle cannot loop forever).
  `sample_channel_looped` (N255) wraps `time` into `[start, end)` for a looping
  clip instead of clamping.
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
N225/N226: the same path deforms the **full geometry** — `render_geometry_at`,
`render_scene_at` and `render_package_scene` sample the mesh node's `weights`
channel and morph `MeshGeometry.positions` before skinning, so a blend shape
animates the real mesh (and, on a strong device, the mirrored frame).

This is a host-side approximation: only the capped preview vertices are deformed,
there is no texture lookup, and it is not a full skinned-mesh device renderer.

## Textured bake (N113–N117)

`TextureMap` holds RGB565 pixels and samples UVs with a wrapping lookup:
nearest by default, or bilinear via `Filter::Linear` / `with_filter` (N203).
`raster_textured` draws the mesh triangles, interpolating UVs barycentrically and
sampling the texture per pixel, multiplied by an optional tint (N204, the glTF
`baseColorFactor`) — a white tint is the identity, so a plain texture shows
through unchanged; a mesh with no UVs (or no texture) fills a solid fallback
colour instead and never silently samples texel 0.

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

N184/N185: `render_scene_at` walks **every** mesh node — skinned meshes deform,
plain meshes are placed by their node world transform — against one bind-pose box
so multi-part models compose into one frame (each mesh uses its material colour or
texture). `bake_scene_frames` / `bake_scene_to_package` bake the whole scene.
`render_package_mesh` renders a mesh-mode `DevicePackage` (geometry + its texture
blob) so `mdc render-package` works for mesh packages, not only baked atlases.
`mdc render-mesh` renders the whole scene by default and a single mesh with
`--single`. N325: meshes are drawn far-to-near (painter's algorithm, ordered by
mean world-space z, stable for ties), so a nearer mesh overlays a farther one in
both `render_scene_at` and `render_package_scene`; there is no z-buffer.
N326/N327: glTF `NORMAL` is imported into `MeshGeometry.normals`, and scene
renders apply a per-triangle Lambert shade from a fixed key light (`+z`, ambient
0.45) — a surface facing the light is unshaded, so a flat +z quad is unchanged,
while a side-facing surface darkens. N365: skinned vertices also transform their
normals by the joint matrices (linear part; approximate under non-uniform scale). N343: glTF `metallicFactor`/`roughnessFactor` are imported;
roughness raises the ambient floor (a rough surface shades flatter), while
`metallicFactor` is recorded but unlit without an environment. N363: `emissiveFactor`
(lifts the ambient) and `KHR_materials_unlit` (no directional shading) are imported.
N373: `KHR_texture_transform` on the base-colour texture (`offset`/`rotation`/`scale`)
is imported and applied to the UV before sampling. N374: `KHR_materials_emissive_strength`
scales `emissiveFactor` before it lifts the ambient. N376: sparse accessors are
de-sparsed into a tight byte image before decoding, so POSITION/NORMAL/UV/COLOR_0
(and indices) honour their `sparse` block. N377: interleaved accessors
(`bufferView.byteStride` + accessor `byteOffset`) are de-strided into the same
tight image, so shared-stride vertex layouts decode correctly. N378: `occlusionTexture`
is imported and multiplies each triangle's shade (sampled at the triangle centroid)
on the package/device renderer; `occlusionStrength` scales the effect. N379:
`TANGENT` and `normalTexture` are imported and, on the package/device renderer,
shade each pixel from the tangent-space normal map (per-pixel Lambert); without a
normal map (or tangents) the per-triangle shade is used as before.

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
