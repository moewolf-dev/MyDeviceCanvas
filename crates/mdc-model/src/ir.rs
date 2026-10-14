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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub mesh: Option<usize>,
    pub children: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mesh {
    pub name: String,
    pub vertices: u32,
    pub indices: u32,
    pub material: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Texture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Material {
    pub name: String,
    pub texture: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skeleton {
    pub name: String,
    pub bones: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Animation {
    pub name: String,
    pub channels: u32,
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl Model {
    pub fn total_vertices(&self) -> u64 {
        self.meshes.iter().map(|m| u64::from(m.vertices)).sum()
    }
    pub fn texture_bytes(&self) -> u64 {
        self.textures.iter().map(|t| u64::from(t.bytes)).sum()
    }
}
