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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Mesh {
    pub name: String,
    pub vertices: u32,
    pub indices: u32,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Material {
    pub name: String,
    pub texture: Option<usize>,
    /// N173: `pbrMetallicRoughness.baseColorFactor` as 8-bit RGB. `None` means
    /// the factor was absent (renderers use their own default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
}

impl Material {
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
}
