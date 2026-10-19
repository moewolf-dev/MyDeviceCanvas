//! MDC IR — the single intermediate representation every importer produces and
//! every target backend consumes. No source format (glTF, Live2D) is recognised
//! past the importer boundary, and no model format ever reaches the device
//! runtime (see `docs/render-topology-decision.md` §4).
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Work done while compiling: 8 MiB source / resolved buffers, 4 MiB textures,
/// 1024 nodes, 65536 vertices. These are intermediate-buffer limits, not
/// product promises. Every importer checks them before allocating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportLimits {
    pub max_input_bytes: usize,
    pub max_texture_bytes: u64,
    pub max_nodes: usize,
    pub max_vertices: u64,
}
impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 8 * 1024 * 1024,
            max_texture_bytes: 4 * 1024 * 1024,
            max_nodes: 1024,
            max_vertices: 65_536,
        }
    }
}

/// N163: the most full-mesh vertices imported into `MeshGeometry`.
pub const MAX_GEOMETRY_VERTICES: usize = 4096;
/// N163: the most triangle-list indices imported into `MeshGeometry`.
pub const MAX_GEOMETRY_INDICES: usize = 16_384;

/// N163: a bounded full-mesh geometry for a strong device or the offline
/// renderer. Only the first primitive with embedded bytes is imported; the
/// arrays are parallel where noted. Kept small enough to stay a preview budget,
/// not a full asset pipeline.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MeshGeometry {
    /// Local-space POSITION, up to `MAX_GEOMETRY_VERTICES`.
    pub positions: Vec<[f32; 3]>,
    /// Triangle-list indices into `positions`, up to `MAX_GEOMETRY_INDICES`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indices: Vec<u32>,
    /// TEXCOORD_0, parallel to `positions` (empty when absent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uv: Vec<[f32; 2]>,
    /// N383: TEXCOORD_1, a second UV set (empty when absent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uv2: Vec<[f32; 2]>,
    /// N326: NORMAL, parallel to `positions` (empty when absent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub normals: Vec<[f32; 3]>,
    /// N364: COLOR_0, parallel to `positions` (RGBA 0..1; empty when absent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colors: Vec<[f32; 4]>,
    /// N379: TANGENT, parallel to `positions` (xyzw; w is the handedness sign).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tangents: Vec<[f32; 4]>,
    /// N384: JOINTS_1 (up to four more skinning influences).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skin_joints2: Vec<[u16; 4]>,
    /// N384: WEIGHTS_1, parallel to `skin_joints2`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skin_weights2: Vec<[f32; 4]>,
    /// JOINTS_0, parallel to `positions` (empty when absent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skin_joints: Vec<[u16; 4]>,
    /// WEIGHTS_0, parallel to `positions` (empty when absent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skin_weights: Vec<[f32; 4]>,
    /// N223: per-target POSITION deltas for the full geometry (blend shapes),
    /// parallel to `positions`. Capped by `MAX_MORPH_TARGETS`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub morph_targets: Vec<Vec<[f32; 3]>>,
}

impl MeshGeometry {
    /// N614: simulate a FIFO vertex cache of `cache_size` entries over the index
    /// order and count misses. Lives on the IR so a package manifest can carry the
    /// locality metric without depending on the bake crate.
    pub fn cache_misses(&self, cache_size: usize) -> u64 {
        let size = cache_size.max(1);
        let mut misses = 0u64;
        let mut cache: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
        for triangle in self.indices.chunks_exact(3) {
            for &vertex in triangle {
                if cache.contains(&vertex) {
                    continue;
                }
                misses += 1;
                if cache.len() == size {
                    cache.pop_front();
                }
                cache.push_back(vertex);
            }
        }
        misses
    }

    /// N273: append another primitive's geometry, rebasing its indices by this
    /// geometry's vertex count. UV/skin arrays are merged only when both sides
    /// carry them; a morph merge only happens when the target counts match.
    pub fn merge(&mut self, other: MeshGeometry) {
        let base = self.positions.len() as u32;
        self.positions.extend(other.positions);
        self.indices
            .extend(other.indices.into_iter().map(|i| i.saturating_add(base)));
        if !other.uv.is_empty() {
            if self.uv.is_empty() {
                self.uv.resize(base as usize, [0.0, 0.0]);
            }
            self.uv.extend(other.uv);
        }
        if !other.uv2.is_empty() {
            if self.uv2.is_empty() {
                self.uv2.resize(base as usize, [0.0, 0.0]);
            }
            self.uv2.extend(other.uv2);
        }
        if !other.normals.is_empty() {
            if self.normals.is_empty() {
                self.normals.resize(base as usize, [0.0, 0.0, 1.0]);
            }
            self.normals.extend(other.normals);
        }
        if !other.colors.is_empty() {
            if self.colors.is_empty() {
                self.colors.resize(base as usize, [1.0, 1.0, 1.0, 1.0]);
            }
            self.colors.extend(other.colors);
        }
        if !other.tangents.is_empty() {
            if self.tangents.is_empty() {
                self.tangents.resize(base as usize, [1.0, 0.0, 0.0, 1.0]);
            }
            self.tangents.extend(other.tangents);
        }
        if !other.skin_joints2.is_empty() {
            if self.skin_joints2.is_empty() {
                self.skin_joints2.resize(base as usize, [0, 0, 0, 0]);
            }
            self.skin_joints2.extend(other.skin_joints2);
        }
        if !other.skin_weights2.is_empty() {
            if self.skin_weights2.is_empty() {
                self.skin_weights2
                    .resize(base as usize, [0.0, 0.0, 0.0, 0.0]);
            }
            self.skin_weights2.extend(other.skin_weights2);
        }
        if !other.skin_joints.is_empty() {
            if self.skin_joints.is_empty() {
                self.skin_joints.resize(base as usize, [0; 4]);
            }
            self.skin_joints.extend(other.skin_joints);
        }
        if !other.skin_weights.is_empty() {
            if self.skin_weights.is_empty() {
                self.skin_weights.resize(base as usize, [0.0; 4]);
            }
            self.skin_weights.extend(other.skin_weights);
        }
        if !other.morph_targets.is_empty() && self.morph_targets.len() == other.morph_targets.len()
        {
            for (target, extra) in self.morph_targets.iter_mut().zip(other.morph_targets) {
                target.extend(extra);
            }
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("input is {0} bytes, over the {1}-byte limit")]
    InputTooLarge(usize, usize),
    #[error("asset exceeds a compile budget: {0}")]
    Budget(String),
    #[error("unsupported or malformed model: {0}")]
    Unsupported(String),
    #[error("json: {0}")]
    Json(String),
}

/// N388: a compact, serialisable import report for a model.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelSummary {
    pub source: String,
    pub nodes: usize,
    pub nodes_with_mesh: usize,
    pub meshes: usize,
    pub vertices: u64,
    pub indices: u64,
    pub primitives: u64,
    pub materials: usize,
    pub textures: usize,
    pub skeletons: usize,
    pub animations: usize,
    pub sprites: usize,
    pub layers: usize,
    pub lights: usize,
    pub variants: usize,
    pub cameras: usize,
    pub morph_targets: usize,
    pub has_normals: bool,
    pub has_colors: bool,
    pub has_tangents: bool,
    pub has_second_uv: bool,
    pub has_second_skin: bool,
    /// N544: textures stored as a KTX2/Basis container (metadata only; the host
    /// does not transcode them).
    pub ktx2_textures: usize,
    /// N570: total KTX2 image slices (`layer * face` summed over KTX2 textures).
    pub ktx2_slices: u64,
}

/// Node local transform. glTF applies `T * R * S` to the node's children and
/// mesh. Values are the file's defaults (identity) when the node omits them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub mesh: Option<usize>,
    pub children: Vec<usize>,
    #[serde(default = "identity_translation", skip_serializing_if = "is_zero3")]
    pub translation: [f32; 3],
    /// Quaternion in glTF order `[x, y, z, w]`.
    #[serde(
        default = "identity_rotation",
        skip_serializing_if = "is_identity_quat"
    )]
    pub rotation: [f32; 4],
    #[serde(default = "identity_scale", skip_serializing_if = "is_one3")]
    pub scale: [f32; 3],
}

impl Default for Node {
    fn default() -> Self {
        Self {
            name: String::new(),
            mesh: None,
            children: Vec::new(),
            translation: identity_translation(),
            rotation: identity_rotation(),
            scale: identity_scale(),
        }
    }
}

pub(crate) fn identity_translation() -> [f32; 3] {
    [0.0, 0.0, 0.0]
}
pub(crate) fn identity_rotation() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}
pub(crate) fn identity_scale() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}
fn is_zero3(v: &[f32; 3]) -> bool {
    v == &[0.0, 0.0, 0.0]
}
fn is_one3(v: &[f32; 3]) -> bool {
    v == &[1.0, 1.0, 1.0]
}
fn is_identity_quat(v: &[f32; 4]) -> bool {
    v == &[0.0, 0.0, 0.0, 1.0]
}
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Mesh {
    pub name: String,
    pub vertices: u32,
    pub indices: u32,
    /// N279: glTF primitive count merged into `geometry` (0 when not glTF).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub primitives: u32,
    pub material: Option<usize>,
    /// First mesh primitive's POSITION samples, projected to pixel space.
    /// Empty when the accessor bytes are not embedded. Capped at 64 points.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview_xy: Vec<(i16, i16)>,
    /// Triangle list into `preview_xy`. Empty means draw a fan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview_indices: Vec<u16>,
    /// N109: raw local-space preview positions (VEC3, file units) parallel to
    /// `preview_xy`, so a skinned bake can deform them before projection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview_positions: Vec<[f32; 3]>,
    /// N109: per-preview-vertex joint indices (JOINTS_0), parallel to
    /// `preview_positions`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skin_joints: Vec<[u16; 4]>,
    /// N109: per-preview-vertex joint weights (WEIGHTS_0), parallel to
    /// `preview_positions`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skin_weights: Vec<[f32; 4]>,
    /// N113: per-preview-vertex texture coordinates (TEXCOORD_0), parallel to
    /// `preview_positions`. Empty means a solid-colour bake.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview_uv: Vec<[f32; 2]>,
    /// N123: morph targets (blend shapes). Each entry is one target's POSITION
    /// deltas, parallel to `preview_positions`. Capped by the importer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub morph_targets: Vec<Vec<[f32; 3]>>,
    /// N123: default morph weights (`mesh.weights`), one per target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub morph_weights: Vec<f32>,
    /// N163: bounded full geometry when the primitive bytes are embedded and fit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<MeshGeometry>,
    /// N433: `KHR_materials_variants` mappings as `[variant_index, material_index]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variant_materials: Vec<[usize; 2]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Texture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub bytes: u32,
    /// N323: the embedded image bytes (bufferView or `data:` URI), retained
    /// within `ImportLimits::max_texture_bytes`. `None` when the source image is
    /// not on disk or would exceed the budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<u8>>,
    /// N334: glTF sampler wrap ("repeat" | "clamp") for this texture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<String>,
}

impl Texture {
    /// N544: whether this texture is a KTX2/Basis container (recognised from the
    /// declared media type or the embedded magic bytes).
    pub fn is_ktx2(&self) -> bool {
        self.format.eq_ignore_ascii_case("image/ktx2")
            || self.data.as_deref().is_some_and(crate::ktx2::is_ktx2)
    }

    /// N570: the KTX2 container metadata (layer/face/depth/mips), derived from the
    /// retained bytes. Keeping the bytes in the IR means the metadata is preserved
    /// through the package without a second, duplicated field.
    pub fn ktx2_header(&self) -> Option<crate::ktx2::Ktx2Header> {
        self.data
            .as_deref()
            .and_then(crate::ktx2::Ktx2Header::parse)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Material {
    pub name: String,
    pub texture: Option<usize>,
    /// N173: `pbrMetallicRoughness.baseColorFactor` as 8-bit RGB. `None` means
    /// the factor was absent (renderers use their own default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    /// N283: glTF `alphaMode` (`OPAQUE` | `MASK` | `BLEND`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha_mode: Option<String>,
    /// N283: glTF `alphaCutoff` (used by `MASK`; default 0.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha_cutoff: Option<f32>,
    /// N333: `baseColorFactor` alpha (0..1). `None` means opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f32>,
    /// N343: glTF `metallicFactor` (0..1; spec default 1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metallic: Option<f32>,
    /// N343: glTF `roughnessFactor` (0..1; spec default 1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f32>,
    /// N363: glTF `emissiveFactor` as 8-bit RGB (lifts the shading ambient).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissive: Option<[u8; 3]>,
    /// N363: `KHR_materials_unlit` — draw without directional shading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlit: Option<bool>,
    /// N373: `KHR_texture_transform` on the base-colour texture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv_transform: Option<UvTransform>,
    /// N374: `KHR_materials_emissive_strength` (`emissiveStrength`, default 1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissive_strength: Option<f32>,
    /// N378: `occlusionTexture.index` (ambient occlusion map).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occlusion_texture: Option<usize>,
    /// N378: `occlusionTexture.strength` (0..1; default 1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occlusion_strength: Option<f32>,
    /// N379: `normalTexture.index` (tangent-space normal map).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_texture: Option<usize>,
    /// N379: `normalTexture.scale` (default 1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_scale: Option<f32>,
    /// N383: which mesh UV set this material samples (`texCoord`; default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv_set: Option<u8>,
    /// N403: the UV set for the secondary maps (normal/occlusion/emissive/
    /// metallicRoughness). Defaults to `uv_set`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv_set2: Option<u8>,
    /// N393: `emissiveTexture.index` (added to the shaded colour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissive_texture: Option<usize>,
    /// N395: `emissiveTexture`'s `KHR_texture_transform`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissive_uv_transform: Option<UvTransform>,
    /// N395: `normalTexture`'s `KHR_texture_transform`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_uv_transform: Option<UvTransform>,
    /// N395: `occlusionTexture`'s `KHR_texture_transform`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occlusion_uv_transform: Option<UvTransform>,
    /// N394: `metallicRoughnessTexture.index` (G = roughness, B = metallic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness_texture: Option<usize>,
    /// N395: `metallicRoughnessTexture`'s `KHR_texture_transform`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness_uv_transform: Option<UvTransform>,
    /// N397: `KHR_materials_specular` `specularFactor` (0..1; default 1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specular_factor: Option<f32>,
    /// N397: `KHR_materials_specular` `specularColorFactor` as 8-bit RGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specular_color: Option<[u8; 3]>,
    /// N397: `KHR_materials_ior` index of refraction (default 1.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ior: Option<f32>,
    /// N414: `KHR_materials_clearcoat` `clearcoatFactor` (0..1; default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clearcoat_factor: Option<f32>,
    /// N414: `KHR_materials_clearcoat` `clearcoatRoughnessFactor` (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clearcoat_roughness: Option<f32>,
    /// N425: `KHR_materials_sheen` `sheenColorFactor` as 8-bit RGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheen_color: Option<[u8; 3]>,
    /// N425: `KHR_materials_sheen` `sheenRoughnessFactor` (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheen_roughness: Option<f32>,
    /// N426: `KHR_materials_transmission` `transmissionFactor` (0..1; default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transmission: Option<f32>,
    /// N434: `KHR_materials_volume` `thicknessFactor` (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_thickness: Option<f32>,
    /// N434: `KHR_materials_volume` `attenuationDistance` (default infinite).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_attenuation_distance: Option<f32>,
    /// N435: `KHR_materials_iridescence` `iridescenceFactor` (0..1; default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iridescence_factor: Option<f32>,
    /// N435: `KHR_materials_iridescence` `iridescenceIor` (default 1.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iridescence_ior: Option<f32>,
    /// N444: `KHR_materials_anisotropy` strength (0..1; default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anisotropy_strength: Option<f32>,
    /// N444: `KHR_materials_anisotropy` rotation in radians (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anisotropy_rotation: Option<f32>,
    /// N456: `KHR_materials_dispersion` (>= 0; default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispersion: Option<f32>,
}

/// N373: glTF `KHR_texture_transform`. `rotation` is in radians; the transform is
/// `scale → rotate → offset` applied to the UV.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct UvTransform {
    #[serde(default)]
    pub offset: [f32; 2],
    #[serde(default)]
    pub rotation: f32,
    #[serde(default = "unit_pair")]
    pub scale: [f32; 2],
}

fn unit_pair() -> [f32; 2] {
    [1.0, 1.0]
}

impl Default for UvTransform {
    fn default() -> Self {
        Self {
            offset: [0.0, 0.0],
            rotation: 0.0,
            scale: unit_pair(),
        }
    }
}

impl UvTransform {
    /// The identity transform (leaves UVs unchanged).
    pub fn is_identity(&self) -> bool {
        self.offset == [0.0, 0.0] && self.rotation == 0.0 && self.scale == unit_pair()
    }

    /// Apply the transform to a UV coordinate.
    pub fn apply(&self, uv: [f32; 2]) -> [f32; 2] {
        let x = uv[0] * self.scale[0];
        let y = uv[1] * self.scale[1];
        let (sin, cos) = self.rotation.sin_cos();
        [
            cos * x - sin * y + self.offset[0],
            sin * x + cos * y + self.offset[1],
        ]
    }
}

impl Material {
    /// N283: whether this material cuts out texels below its cutoff.
    pub fn is_masked(&self) -> bool {
        self.alpha_mode
            .as_deref()
            .is_some_and(|mode| mode.eq_ignore_ascii_case("MASK"))
    }
    /// N283: the cutoff in 0..1 (default 0.5).
    pub fn cutoff(&self) -> f32 {
        self.alpha_cutoff.unwrap_or(0.5).clamp(0.0, 1.0)
    }
    /// N333: whether this material is alpha-blended (`alphaMode: BLEND`).
    pub fn is_blended(&self) -> bool {
        self.alpha_mode
            .as_deref()
            .is_some_and(|mode| mode.eq_ignore_ascii_case("BLEND"))
    }
    /// N333: the source alpha as 8 bits (255 when opaque or absent).
    pub fn alpha_u8(&self) -> u8 {
        let a = self.alpha.unwrap_or(1.0).clamp(0.0, 1.0);
        (a * 255.0).round() as u8
    }
    /// N343: glTF `roughnessFactor` (spec default 1.0).
    pub fn roughness(&self) -> f32 {
        self.roughness.unwrap_or(1.0).clamp(0.0, 1.0)
    }
    /// N343: glTF `metallicFactor` (spec default 1.0). Recorded; a metallic
    /// surface needs an environment to light, so the flat renderer only notes it.
    pub fn metallic(&self) -> f32 {
        self.metallic.unwrap_or(1.0).clamp(0.0, 1.0)
    }
    /// N363: `KHR_materials_unlit` is present and set.
    pub fn is_unlit(&self) -> bool {
        self.unlit.unwrap_or(false)
    }
    /// N363/N374: emissive luminance in 0..1 (from `emissiveFactor`, scaled by
    /// `KHR_materials_emissive_strength`).
    pub fn emissive_luma(&self) -> f32 {
        let base = match self.emissive {
            Some([r, g, b]) => {
                (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0
            }
            None => 0.0,
        };
        (base * self.emissive_strength()).clamp(0.0, 1.0)
    }

    /// N374: `KHR_materials_emissive_strength` (default 1.0; non-negative).
    pub fn emissive_strength(&self) -> f32 {
        self.emissive_strength.unwrap_or(1.0).max(0.0)
    }

    /// N378: the occlusion texture index and its strength (default 1.0).
    pub fn occlusion(&self) -> (Option<usize>, f32) {
        (
            self.occlusion_texture,
            self.occlusion_strength.unwrap_or(1.0).clamp(0.0, 1.0),
        )
    }

    /// N379: the normal texture index and its scale (default 1.0).
    pub fn normal_map(&self) -> (Option<usize>, f32) {
        (
            self.normal_texture,
            self.normal_scale.unwrap_or(1.0).max(0.0),
        )
    }

    /// N383: which mesh UV set this material samples (default 0).
    pub fn uv_set(&self) -> u8 {
        self.uv_set.unwrap_or(0)
    }

    /// N403: the UV set for the secondary maps (defaults to `uv_set`).
    pub fn uv_set2(&self) -> u8 {
        self.uv_set2.unwrap_or_else(|| self.uv_set())
    }

    /// N393: the emissive texture index and its strength (default 1.0).
    pub fn emissive_map(&self) -> (Option<usize>, f32) {
        (self.emissive_texture, self.emissive_strength())
    }

    /// N395: the emissive texture's UV transform (identity when absent).
    pub fn emissive_uv_transform(&self) -> UvTransform {
        self.emissive_uv_transform.unwrap_or_default()
    }

    /// N395: the normal texture's UV transform (identity when absent).
    pub fn normal_uv_transform(&self) -> UvTransform {
        self.normal_uv_transform.unwrap_or_default()
    }

    /// N395: the occlusion texture's UV transform (identity when absent).
    pub fn occlusion_uv_transform(&self) -> UvTransform {
        self.occlusion_uv_transform.unwrap_or_default()
    }

    /// N394: the metallicRoughness texture index (G = roughness).
    pub fn roughness_texture(&self) -> Option<usize> {
        self.roughness_texture
    }

    /// N395: the metallicRoughness texture's UV transform (identity when absent).
    pub fn roughness_uv_transform(&self) -> UvTransform {
        self.roughness_uv_transform.unwrap_or_default()
    }

    /// N397: `KHR_materials_specular` `specularFactor` (0..1; default 1.0).
    pub fn specular_factor(&self) -> f32 {
        self.specular_factor.unwrap_or(1.0).clamp(0.0, 1.0)
    }

    /// N397: `KHR_materials_ior` (default 1.5).
    pub fn ior(&self) -> f32 {
        self.ior.unwrap_or(1.5).max(1.0)
    }

    /// N414: `KHR_materials_clearcoat` factor and roughness (both default 0).
    pub fn clearcoat(&self) -> (f32, f32) {
        (
            self.clearcoat_factor.unwrap_or(0.0).clamp(0.0, 1.0),
            self.clearcoat_roughness.unwrap_or(0.0).clamp(0.0, 1.0),
        )
    }

    /// N425: `KHR_materials_sheen` strength (colour luminance) and roughness.
    pub fn sheen(&self) -> (f32, f32) {
        let strength = match self.sheen_color {
            Some([r, g, b]) => {
                (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0
            }
            None => 0.0,
        };
        (
            strength,
            self.sheen_roughness.unwrap_or(0.0).clamp(0.0, 1.0),
        )
    }

    /// N426: `KHR_materials_transmission` factor (0..1; default 0).
    pub fn transmission(&self) -> f32 {
        self.transmission.unwrap_or(0.0).clamp(0.0, 1.0)
    }

    /// N434: how opaque a volume reads, from `thicknessFactor` over
    /// `attenuationDistance` (0 when either is unset).
    pub fn volume_factor(&self) -> f32 {
        let thickness = self.volume_thickness.unwrap_or(0.0).max(0.0);
        let distance = self.volume_attenuation_distance.unwrap_or(0.0);
        if thickness <= 0.0 || distance <= 0.0 {
            return 0.0;
        }
        (thickness / distance).clamp(0.0, 1.0)
    }

    /// N435: `KHR_materials_iridescence` factor and index of refraction.
    pub fn iridescence(&self) -> (f32, f32) {
        (
            self.iridescence_factor.unwrap_or(0.0).clamp(0.0, 1.0),
            self.iridescence_ior.unwrap_or(1.3).clamp(1.0, 3.0),
        )
    }

    /// N444: `KHR_materials_anisotropy` strength and rotation (radians).
    pub fn anisotropy(&self) -> (f32, f32) {
        (
            self.anisotropy_strength.unwrap_or(0.0).clamp(0.0, 1.0),
            self.anisotropy_rotation.unwrap_or(0.0),
        )
    }

    /// N456: `KHR_materials_dispersion` (0..=10; default 0).
    pub fn dispersion(&self) -> f32 {
        self.dispersion.unwrap_or(0.0).clamp(0.0, 10.0)
    }
}

impl Material {
    /// N373: the base-colour `KHR_texture_transform`, or identity when absent.
    pub fn uv_transform(&self) -> UvTransform {
        self.uv_transform.unwrap_or_default()
    }

    /// N173: the base colour as RGB565, or `None` when unset.
    pub fn color_rgb565(&self) -> Option<u16> {
        self.color.map(|[r, g, b]| {
            ((u16::from(r) >> 3) << 11) | ((u16::from(g) >> 2) << 5) | (u16::from(b) >> 3)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Skeleton {
    pub name: String,
    pub bones: u32,
    /// N109: node index per joint; parallel to `inverse_bind`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joint_nodes: Vec<usize>,
    /// N109: inverse bind matrix per joint, column-major (glTF MAT4), 16 floats.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inverse_bind: Vec<[f32; 16]>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Animation {
    pub name: String,
    pub channels: u32,
    /// N110: sampled channels (translation/rotation/scale per node). Empty for a
    /// metadata-only animation (e.g. a Live2D expression stub).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<AnimationTrack>,
}

/// N110: one glTF animation channel: a node, a transform path and the sampler
/// keyframes. `values` is flat (3 floats per key for translation/scale, 4 for a
/// quaternion rotation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimationTrack {
    pub node: usize,
    /// `translation` | `rotation` | `scale`.
    pub path: String,
    /// `LINEAR` | `STEP` | `CUBICSPLINE`.
    pub interpolation: String,
    pub times: Vec<f32>,
    pub values: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sprite {
    pub name: String,
    pub texture: Option<usize>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    pub nodes: Vec<usize>,
}

/// The single intermediate representation every importer produces.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub source: String,
    pub nodes: Vec<Node>,
    pub meshes: Vec<Mesh>,
    pub textures: Vec<Texture>,
    pub materials: Vec<Material>,
    pub skeletons: Vec<Skeleton>,
    pub animations: Vec<Animation>,
    /// Author-supplied sprites (e.g. a Live2D texture atlas). Backends may add
    /// baked sprites on top when they degrade a mesh model.
    #[serde(default)]
    pub sprites: Vec<Sprite>,
    pub layers: Vec<Layer>,
    /// N46: free-form source metadata (VRM title/author, physics/pose counts).
    #[serde(default)]
    pub meta: std::collections::BTreeMap<String, String>,
    /// N385: `KHR_lights_punctual` lights (the first directional one is the
    /// renderer's key light).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lights: Vec<Light>,
    /// N420: `KHR_materials_variants` variant names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    /// N449: glTF cameras (metadata; the renderer fits geometry to the surface).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cameras: Vec<Camera>,
}

/// N449: a glTF camera (`perspective` or `orthographic`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub name: String,
    /// `perspective` | `orthographic`.
    pub kind: String,
    /// Perspective vertical field of view in radians.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yfov: Option<f32>,
    #[serde(default)]
    pub znear: f32,
    #[serde(default)]
    pub zfar: f32,
}

/// N385: a `KHR_lights_punctual` light source. Only `direction` affects the
/// flat renderer (a fixed key light); `color`/`intensity` are recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Light {
    pub name: String,
    /// `directional` | `point` | `spot`.
    pub kind: String,
    /// Linear RGB 0..1 as 8-bit (default white).
    #[serde(default = "white_rgb", skip_serializing_if = "is_white_rgb")]
    pub color: [u8; 3],
    /// glTF `intensity` (default 1.0).
    #[serde(default = "unit_f32", skip_serializing_if = "is_unit_f32")]
    pub intensity: f32,
    /// World-space direction the light travels (default `-z`).
    #[serde(
        default = "default_light_dir",
        skip_serializing_if = "is_default_light_dir"
    )]
    pub direction: [f32; 3],
}

fn white_rgb() -> [u8; 3] {
    [255, 255, 255]
}

fn is_white_rgb(value: &[u8; 3]) -> bool {
    *value == [255, 255, 255]
}

fn unit_f32() -> f32 {
    1.0
}

fn is_unit_f32(value: &f32) -> bool {
    (*value - 1.0).abs() < f32::EPSILON
}

fn default_light_dir() -> [f32; 3] {
    [0.0, 0.0, -1.0]
}

fn is_default_light_dir(value: &[f32; 3]) -> bool {
    *value == [0.0, 0.0, -1.0]
}

impl Default for Light {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: "directional".into(),
            color: white_rgb(),
            intensity: 1.0,
            direction: default_light_dir(),
        }
    }
}

impl Light {
    /// N385: whether this light is directional (usable as the key light).
    pub fn is_directional(&self) -> bool {
        self.kind == "directional"
    }
}

/// N399: a validation report for an imported model (problems are fatal, warnings
/// are advisory).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ValidationReport {
    pub valid: bool,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    pub summary: ModelSummary,
}

impl Model {
    pub fn total_vertices(&self) -> u64 {
        self.meshes.iter().map(|m| u64::from(m.vertices)).sum()
    }
    /// N433: the material each mesh should use for a `KHR_materials_variants`
    /// variant (`None` keeps the mesh's own material).
    pub fn variant_materials(&self, variant: usize) -> Vec<Option<usize>> {
        self.meshes
            .iter()
            .map(|mesh| {
                mesh.variant_materials
                    .iter()
                    .find(|mapping| mapping[0] == variant)
                    .map(|mapping| mapping[1])
            })
            .collect()
    }

    /// N388: a compact import report (counts and feature flags).
    pub fn summary(&self) -> ModelSummary {
        let geometry = self.meshes.iter().filter_map(|m| m.geometry.as_ref());
        let mut summary = ModelSummary {
            source: self.source.clone(),
            nodes: self.nodes.len(),
            nodes_with_mesh: self.nodes.iter().filter(|n| n.mesh.is_some()).count(),
            meshes: self.meshes.len(),
            vertices: self.total_vertices(),
            indices: self.meshes.iter().map(|m| u64::from(m.indices)).sum(),
            primitives: self.meshes.iter().map(|m| u64::from(m.primitives)).sum(),
            materials: self.materials.len(),
            textures: self.textures.len(),
            skeletons: self.skeletons.len(),
            animations: self.animations.len(),
            sprites: self.sprites.len(),
            layers: self.layers.len(),
            lights: self.lights.len(),
            variants: self.variants.len(),
            cameras: self.cameras.len(),
            morph_targets: self.meshes.iter().map(|m| m.morph_targets.len()).sum(),
            ktx2_textures: self.textures.iter().filter(|t| t.is_ktx2()).count(),
            // N570: sum the per-container slice counts reported by the header.
            ktx2_slices: self
                .textures
                .iter()
                .filter_map(|t| t.ktx2_header())
                .map(|header| u64::from(header.slices()))
                .sum(),
            ..ModelSummary::default()
        };
        for g in geometry {
            summary.has_colors |= !g.colors.is_empty();
            summary.has_tangents |= !g.tangents.is_empty();
            summary.has_second_uv |= !g.uv2.is_empty();
            summary.has_second_skin |= !g.skin_joints2.is_empty();
            summary.has_normals |= !g.normals.is_empty();
        }
        summary
    }

    /// N399: check the model's internal references and flag suspicious data.
    pub fn validate(&self) -> ValidationReport {
        let mut problems = Vec::new();
        let mut warnings = Vec::new();
        let node_count = self.nodes.len();
        let mesh_count = self.meshes.len();
        let texture_count = self.textures.len();
        for (i, node) in self.nodes.iter().enumerate() {
            if let Some(mesh) = node.mesh {
                if mesh >= mesh_count {
                    problems.push(format!("node {i} references mesh {mesh} out of range"));
                }
            }
            for child in &node.children {
                if *child >= node_count {
                    problems.push(format!("node {i} references child {child} out of range"));
                }
            }
        }
        for (i, mesh) in self.meshes.iter().enumerate() {
            if let Some(material) = mesh.material {
                if material >= self.materials.len() {
                    problems.push(format!(
                        "mesh {i} references material {material} out of range"
                    ));
                }
            }
            if mesh.geometry.is_none() && mesh.preview_positions.is_empty() {
                warnings.push(format!("mesh {i} has no geometry"));
            }
        }
        for (i, material) in self.materials.iter().enumerate() {
            for (kind, index) in [
                ("base colour", material.texture),
                ("emissive", material.emissive_texture),
                ("occlusion", material.occlusion_texture),
                ("normal", material.normal_texture),
                ("metallicRoughness", material.roughness_texture),
            ] {
                if let Some(index) = index {
                    if index >= texture_count {
                        problems.push(format!("material {i} {kind} texture {index} out of range"));
                    }
                }
            }
        }
        for (i, texture) in self.textures.iter().enumerate() {
            if texture.data.is_none() && texture.bytes == 0 {
                warnings.push(format!("texture {i} has no embedded bytes"));
            }
            // N544: KTX2/Basis is reported (size/format are known) but not
            // transcoded, so a package built from this model has no texture.
            if texture.is_ktx2() {
                warnings.push(format!(
                    "texture {i} is a KTX2/Basis container (transcoding is not supported)"
                ));
                // N600: a readable header must agree with the declared size.
                if let Some(header) = texture.ktx2_header() {
                    if header.pixel_width != texture.width || header.pixel_height != texture.height
                    {
                        problems.push(format!(
                            "texture {i} KTX2 declares {}x{} but the header says {}x{}",
                            texture.width, texture.height, header.pixel_width, header.pixel_height
                        ));
                    }
                }
            }
        }
        for (i, skeleton) in self.skeletons.iter().enumerate() {
            for joint in &skeleton.joint_nodes {
                if *joint >= node_count {
                    problems.push(format!("skeleton {i} joint {joint} out of range"));
                }
            }
        }
        if self.nodes.is_empty() && self.meshes.is_empty() && self.sprites.is_empty() {
            problems.push("model has no nodes, meshes, or sprites".into());
        }
        let valid = problems.is_empty();
        ValidationReport {
            valid,
            problems,
            warnings,
            summary: self.summary(),
        }
    }
    pub fn texture_bytes(&self) -> u64 {
        self.textures.iter().map(|t| u64::from(t.bytes)).sum()
    }

    /// N323: the raw embedded bytes of the texture at `index`, if retained.
    pub fn embedded_texture(&self, index: usize) -> Option<&[u8]> {
        self.textures.get(index)?.data.as_deref()
    }
    /// N323: the embedded bytes of the material's base-colour texture, if any.
    pub fn material_texture(&self, material: usize) -> Option<&[u8]> {
        let index = self.materials.get(material)?.texture?;
        self.embedded_texture(index)
    }
    /// N334: the wrap mode name of the material's texture ("clamp" when either
    /// axis is `CLAMP_TO_EDGE`, else "repeat"), if a texture is referenced.
    pub fn material_texture_wrap(&self, material: usize) -> Option<&str> {
        let index = self.materials.get(material)?.texture?;
        self.textures.get(index)?.wrap.as_deref()
    }
}
