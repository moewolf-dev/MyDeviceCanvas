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
normal map (or tangents) the per-triangle shade is used as before. N383: a
material's `texCoord` (`Material.uv_set`) selects the mesh UV set, and `TEXCOORD_1`
is imported into `MeshGeometry.uv2`, so a mesh can sample its texture from the
second UV channel (falling back to the first). N384: `JOINTS_1`/`WEIGHTS_1` are
imported (`MeshGeometry.skin_joints2`/`skin_weights2`) and `skin_arrays_multi`/
`skin_normals_multi` blend up to eight influences per vertex. N385:
`KHR_lights_punctual` is imported (`Model.lights`/`DevicePackage.lights`); the first
directional light overrides the renderer's fixed key light (its node rotation
orients it and `intensity` scales it). N386: `ShadeMode::Smooth` shades the package
renderer per pixel from interpolated normals (Gouraud) instead of one factor per
triangle; `render_package_scene_clip_mode` selects the mode and the device keeps
`Flat`. N387: `ShadeMode::SmoothSpecular` adds a Blinn-Phong specular highlight,
its strength/shininess derived from the material's `roughness`/`metallic`. Per-pixel
shading now also applies to untextured (solid) triangles. N390:
`KHR_mesh_quantization` integer POSITION and normalized byte/short TANGENT decode
(raw integer positions are dequantized by the node transform). N413: the legacy
`KHR_materials_pbrSpecularGlossiness` extension is converted to metallic-roughness
(diffuse → base colour, `1 - glossiness` → roughness, specular luminance → metallic).
N414: `KHR_materials_clearcoat` adds a second specular lobe from the smooth surface
normal in the per-pixel pass. N423: when a mesh omits `TANGENT` but a normal map is
present, tangents are derived from the UV gradient (`generate_tangents`), so normal
mapping works without authored tangents. N425: `KHR_materials_sheen` adds a rim
highlight (strength from `sheenColorFactor`, falloff from `sheenRoughnessFactor`).
N426: `KHR_materials_transmission` folds into the blend alpha
(`alpha * (1 - transmissionFactor)`), so transmissive materials fade toward the
background. N433: `KHR_materials_variants` per-primitive `mappings` are imported
(`Mesh.variant_materials`) and `mdc package --variant NAME` swaps in that variant's
materials (`Model::variant_materials`). N434: `KHR_materials_volume`
(`thicknessFactor` over `attenuationDistance`) deepens the transmission fade.
N435: `KHR_materials_iridescence` adds a thin-film highlight, its hue shifting with
the view angle and IOR. N444: `KHR_materials_anisotropy` adds a brushed highlight
stretched along the tangent (rotation-aware). N445: `EXT_texture_webp` is honoured
(a texture without `source` uses the extension's image) and WebP bytes decode
through the `image` crate's `webp` feature. N403: `texCoord` is
honoured per slot — `Material.uv_set` selects the base-colour UV set and
`uv_set2` (defaulting to `uv_set`) selects the secondary maps' set, so normal/
occlusion/emissive/metallicRoughness maps can sample `TEXCOORD_1`. N393:
`emissiveTexture` is imported and added (scaled by `emissiveStrength`) to the shaded
colour. N395: `KHR_texture_transform` is honoured per texture slot (base colour,
normal, emissive, metallicRoughness), each sampled with its own transform. N394:
`metallicRoughnessTexture`'s green (roughness) channel raises the per-teixel ambient
floor in the per-pixel pass. N406: `occlusionTexture` is sampled per pixel (R channel
scales the shaded colour) when the per-pixel pass runs; the per-triangle centroid
sample remains for the flat path. N380:
`pack_material_atlas` (and `mdc package --atlas MAX_PAGE`) shelf-packs a package's
per-material textures into one bounded page, remapping material indices and mesh UVs.
N760: each texture is trimmed to its opaque bounds before packing (when it carries
a 1-bit alpha mask), and the mesh UVs are remapped in two steps (original →
trimmed pixels → page), so a page can shrink without changing what the mesh
samples.

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

## Extensions

- N513: `EXT_mesh_gpu_instancing` expands a node into a mesh-less group (holding
  the node transform) with one child per instance (its own TRS), bounded by
  `MAX_INSTANCES` (64). Instances are appended so node indices stay stable.
- N503: compression extensions are detected; a document that merely lists one in
  `extensionsUsed` still imports, and `mdc model-info` reports them under
  `reserved_extensions`.
- N533/N534: `EXT_meshopt_compression` is **decoded** (a pure-Rust port of the
  meshopt reference decoder in `mdc-model::meshopt`): vertex buffers (`0xa0`/`0xa1`)
  with the octahedral/quaternion/exponential/color filters, index buffers
  (`0xe0`/`0xe1`) and index sequences (`0xd0`/`0xd1`). The port is checked against
  the reference decoder's golden fixtures. A malformed stream is a clear error,
  never silent garbage.
- N543: the meshopt decoder is fuzz/bounds hardened. A zero-alpha COLOR filter no
  longer underflows, every declared size is `checked_mul`'d, and a stream over
  `MAX_VERTEX_DECODE_BYTES`/`MAX_INDEX_DECODE_BYTES` is rejected before any
  allocation. Truncated and pseudo-random streams return an error, never panic.
  meshopt **encoding** is still a gap (no reference encoder to cross-check).
- N544: `KHR_texture_basisu` is **recognised, not transcoded**. A bounded KTX2
  header reader (`Ktx2Header::parse`) reports the real pixel size, `vkFormat` and
  supercompression scheme (`mdc model-info` lists them per texture). A document
  that merely *uses* the extension imports (metadata only); a document that
  *requires* it fails. `KHR_draco_mesh_compression` is still rejected outright.
- `KHR_draco_mesh_compression` is still **rejected** with a clear error (a
  required extension fails the import).
- N545: `compact_geometry` is a dependency-free, lossless geometry pass that
  merges bit-identical vertices (across positions, UVs, normals, colours,
  tangents, skin and morphs) and drops unreferenced ones, rewriting the index
  buffer. `mdc package --compact-geometry` reports `compacted_vertices`.
- N553: the meshopt decoder has a curated fuzz corpus (`FUZZ_CORPUS`) exercised
  with every single-byte mutation (0x00/0xFF/MSB flip) and truncation, so a
  malformed stream is always an error.
- N554: beyond width/height, the KTX2 header reports its mip count, slice count
  (`layerCount * faceCount`) and container kind (2D/array/cubemap/volume).
- N555: `optimize_index_order` reorders triangles to reduce vertex-cache misses
  (a bounded look-ahead greedy over a FIFO cache); the triangle set is unchanged.
  `mdc package --optimize-index [CACHE]` reports `index_cache_misses` and
  `reordered_meshes`.
- N563: the decoder's branch inventory and fuzz passes are documented in
  [`meshopt-coverage.md`](meshopt-coverage.md).
- N583: the index-sequence stage is round-tripped in tests
  (`decode(encode(values)) == values`) using a test-only inverse encoder.
- N593: likewise the v1 vertex-buffer raw-delta stage round-trips; `mdc package
  --report` labels the cache policy (`cache_mode`: `off`/`adaptive`/`fixed`; N594).
- N613: the v0 raw-delta stage round-trips too; a partial trailing group no longer
  over-writes the delta buffer.
- N614: `MdcPackage::seal` records `manifest.cache_misses`/`cache_size` (the
  vertex-cache locality at a 32-entry cache); `verify` recomputes it as
  `cache_misses_ok`, and `mdc verify`/`mdc diff` report `cache_misses`.
- N694: `mdc diff` exposes the package-order locality as an explicit `package_order` diff line (`meshes`, `cache_size`, `misses`).
- N700: `mdc verify --strict` fails when `cache_misses_ok` is false; ordinary verify preserves older-package compatibility.

- N664: `optimize_package_index_order` returns a `PackageOrderReport`
  (`meshes`/`reordered`/`misses_before`/`misses_after`); N669 surfaces the saving
  as `index_cache_saved` in `mdc package --report`.
- N695: sprite sampling reuses the image fit box and applies the same horizontal
  flip/quarter-turn rules, so atlas and inline-image transforms do not diverge.
- N590: `MdcPackage::verify()` sets `ktx2_ok` — a texture that carries a readable
  KTX2 header must declare the same width/height as the container, otherwise the
  package is invalid. `mdc diff` compares `ktx2_textures`/`ktx2_slices`.
- N600: `Model::validate()` (the model layer) reports the same KTX2 size mismatch
  as a problem, so `mdc validate`/`mdc diff` and the package gate agree.
- N564: `optimize_geometry`/`optimize_package_geometry` run compaction *before*
  index reordering (smaller working set); `mdc package --optimize-geometry [CACHE]`
  enables both at once.
- N574: `adaptive_cache_size` picks a vertex-cache size from the triangle count and
  `optimize_geometry_auto(0)` uses it. Because a larger cache holds a superset of
  recent vertices, more capacity never fetches *more* (N584 asserts this):

  | Triangles | Cache | Rationale |
  |---|---|---|
  | < 128 | 16 | a small mesh fits quickly; a big cache is wasted |
  | < 2048 | 32 | the common GPU/MCU vertex-cache size |
  | ≥ 2048 | 64 | keep more reuse on a large mesh |
- N570: a KTX2 texture's container metadata (mip levels, slices, 2D/array/cubemap/
  volume) is reachable from the IR via `Texture::ktx2_header()`; `ModelSummary`
  sums `ktx2_slices` and `mdc package --report` reports `ktx2_textures`/`ktx2_slices`.
- N580: KTX2/Basis is **metadata only** in the render path. `attach_model_textures`
  returns an empty blob for a KTX2 image (the host does not transcode), so the
  mesh renders with its material colour and no base-colour texture; `Model::validate`
  warns and `mdc validate`/`mdc model-info` report it. `mdc render-package`/device
  rendering therefore show a solid fill rather than the packed texture — this is a
  declared degradation, not a silent fallback to a wrong texel.
- N504: a texture with a mip chain (`--texture-mips`) can bake from a coarser
  level (`--mip-level N`); at render time the device picks the coarsest level
  that still covers the target size (`select_mip_level`).
- N523: an image's media type is taken from `mimeType` or, when absent, from its
  magic bytes; PNG/JPEG/**WebP** (VP8/VP8L/VP8X) dimensions are probed from the
  container header rather than trusted from the declared size.
- N524: `mdc package --max-draw-vertices N` trims meshes once the cumulative
  vertex count would exceed `N` (`trim_to_vertex_budget`); mesh indices stay
  stable and the nodes that referenced a trimmed mesh stop drawing it.
- N704: when a package-order pass runs, the manifest persists
  `cache_misses_before`/`cache_misses_saved` next to the recomputable
  `cache_misses`; the body/checksum are unaffected, and older packages (fields
  default to 0) still verify.
- N710: KTX2 metadata is reported through one `Ktx2Summary` (textures, slices,
  size mismatches) shared by the model summary and `DevicePackage`, so
  `mdc model-info`, `mdc verify` and `mdc diff` cannot disagree about the same
  container.
- N726: `SkinMode::{Lbs, Dqs}`. LBS remains the default; a board that advertises
  `RendererCapability.dqs` selects dual-quaternion blending for its device
  renderer, and `mdc render-mesh --dqs` selects it for offline renders. DQS
  blends the rigid joint transforms (removing the LBS candy-wrapper collapse),
  while scale/shear is reduced to the nearest rotation. N756: both sets of
  influences (`JOINTS_0/WEIGHTS_0` + `JOINTS_1/WEIGHTS_1`, up to eight) are
  summed and divided by the actual weight total before the quaternion is
  normalised, so 0.25/0.75 and 0.5/0.5 weights agree with LBS; zero weights and
  out-of-range joints leave the vertex in bind pose instead of producing NaNs.
- N736: with `SkinMode::Dqs`, normals and tangents are rotated by the same
  blended joint quaternion (the linear part is no longer averaged), so shading
  follows the DQS pose; `w` handedness is preserved.
- N746: `render_package_mesh_skinned` sends skinned mesh packages through the
  scene renderer with an explicit `SkinMode`, so DQS affects mesh-package
  normals/tangents as well as the scene path; skeleton-less packages keep the
  flat renderer.
- N749: when `trim_package_textures` shrinks a texture with an existing mip
  chain, it rebuilds the chain at the same level count, ending at 1x1.
- N739: `trim_texture_blob` crops fully transparent margins of an alpha-masked
  texture; `trim_package_textures` also rewrites the UVs of meshes using that
  texture, so the rendered pixels are unchanged while the atlas/upload shrinks.
  `mdc package --trim-textures` applies it before packing. Indexed blobs and mip
  chains are left alone.
- N731: a `MASK` material's `alphaCutoff` is applied when its texture is baked:
  `bake_texture_blob_with_cutoff`/`decode_rgba_native_with_cutoff` threshold the
  1-bit mask at `material.cutoff_u8()` instead of a fixed 128. Host and device
  both consume that mask, so the cutout is identical.
- N714: the package-order report and the sealed manifest are reconciled by test:
  `optimize_package_index_order(..., CACHE_REPORT_SIZE)` must equal
  `manifest.cache_misses`, and `set_cache_report(report.misses_before)` must
  persist `report.saved()` — the two views of one pass cannot drift.

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
