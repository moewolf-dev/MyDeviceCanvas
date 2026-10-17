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
    /// N326: NORMAL, parallel to `positions` (empty when absent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub normals: Vec<[f32; 3]>,
    /// N364: COLOR_0, parallel to `positions` (RGBA 0..1; empty when absent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colors: Vec<[f32; 4]>,
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
}

impl Model {
    pub fn total_vertices(&self) -> u64 {
        self.meshes.iter().map(|m| u64::from(m.vertices)).sum()
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
