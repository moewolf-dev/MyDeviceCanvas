//! `mdc-model` — Model Compiler / MDC IR skeleton (N05).
//!
//! Pipeline: source model -> Importer -> MDC IR -> Target backend -> device
//! package. Only one importer (glTF 2.0) and one target backend (ESP32 plane /
//! sprite baking) exist in this wave; the IR is shared so more importers and
//! backends can be added without touching Core.
//!
//! This crate depends only on `mdc-protocol`-adjacent data types (actually just
//! serde). It never depends on `mdc-core`, and model formats never enter the
//! device runtime.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Work done while compiling: 8 MiB source, 4 MiB textures, 1024 nodes,
/// 65536 vertices. These are intermediate-buffer limits, not product promises.
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

// ---- MDC IR ----

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

// ---- Importer ----

/// Minimal glTF 2.0 importer. It reads the JSON (`.gltf`) form and extracts
/// nodes, meshes, materials, textures/images and animations. Image bytes are
/// never decoded; only declared sizes are read, so a decode bomb is rejected
/// before any decode buffer is allocated.
pub struct GltfImporter;

impl GltfImporter {
    pub fn import(&self, bytes: &[u8], limits: &ImportLimits) -> Result<Model, ModelError> {
        if bytes.len() > limits.max_input_bytes {
            return Err(ModelError::InputTooLarge(
                bytes.len(),
                limits.max_input_bytes,
            ));
        }
        let doc: Value =
            serde_json::from_slice(bytes).map_err(|e| ModelError::Json(e.to_string()))?;
        if let Some(version) = doc
            .get("asset")
            .and_then(|a| a.get("version"))
            .and_then(Value::as_str)
        {
            if !version.starts_with("2") {
                return Err(ModelError::Unsupported(format!("glTF version {version}")));
            }
        }

        let empty = Vec::new();
        let nodes_json = doc.get("nodes").and_then(Value::as_array).unwrap_or(&empty);
        if nodes_json.len() > limits.max_nodes {
            return Err(ModelError::Budget(format!(
                "{} nodes over {} limit",
                nodes_json.len(),
                limits.max_nodes
            )));
        }
        let accessors = doc
            .get("accessors")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let accessor_count = |index: u64| -> u64 {
            accessors
                .get(index as usize)
                .and_then(|a| a.get("count"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };

        // Images -> textures (declared sizes only; no decode).
        let images = doc
            .get("images")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let buffer_views = doc
            .get("bufferViews")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let mut textures = Vec::with_capacity(images.len());
        for (i, image) in images.iter().enumerate() {
            let bytes_len = image
                .get("bufferView")
                .and_then(Value::as_u64)
                .and_then(|bv| buffer_views.get(bv as usize))
                .and_then(|bv| bv.get("byteLength"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            textures.push(Texture {
                name: json_name(image, &format!("image{i}")),
                width: image.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
                height: image.get("height").and_then(Value::as_u64).unwrap_or(0) as u32,
                format: image
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("image/png")
                    .to_string(),
                bytes: bytes_len as u32,
            });
        }
        let declared_bytes: u64 = textures.iter().map(|t| u64::from(t.bytes)).sum();
        if declared_bytes > limits.max_texture_bytes {
            return Err(ModelError::Budget(format!(
                "declared texture bytes {declared_bytes} over {} limit",
                limits.max_texture_bytes
            )));
        }

        // textures[i].source -> image index, used to resolve material textures.
        let textures_json = doc
            .get("textures")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let resolve_texture = |tex_index: u64| -> Option<usize> {
            textures_json
                .get(tex_index as usize)
                .and_then(|t| t.get("source"))
                .and_then(Value::as_u64)
                .map(|source| source as usize)
        };

        let materials_json = doc
            .get("materials")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let mut materials = Vec::with_capacity(materials_json.len());
        for (i, material) in materials_json.iter().enumerate() {
            let texture = material
                .get("pbrMetallicRoughness")
                .and_then(|p| p.get("baseColorTexture"))
                .and_then(|b| b.get("index"))
                .and_then(Value::as_u64)
                .and_then(resolve_texture);
            materials.push(Material {
                name: json_name(material, &format!("material{i}")),
                texture,
            });
        }

        let meshes_json = doc
            .get("meshes")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let mut meshes = Vec::with_capacity(meshes_json.len());
        for (i, mesh) in meshes_json.iter().enumerate() {
            let mut vertices = 0u64;
            let mut indices = 0u32;
            let mut material = None;
            if let Some(primitives) = mesh.get("primitives").and_then(Value::as_array) {
                for primitive in primitives {
                    if let Some(position) = primitive
                        .get("attributes")
                        .and_then(|a| a.get("POSITION"))
                        .and_then(Value::as_u64)
                    {
                        vertices = vertices.saturating_add(accessor_count(position));
                    }
                    if let Some(index) = primitive.get("indices").and_then(Value::as_u64) {
                        indices = indices
                            .saturating_add(accessor_count(index).min(u64::from(u32::MAX)) as u32);
                    }
                    if let Some(mat) = primitive.get("material").and_then(Value::as_u64) {
                        material = Some(mat as usize);
                    }
                }
            }
            meshes.push(Mesh {
                name: json_name(mesh, &format!("mesh{i}")),
                vertices: vertices.min(u64::from(u32::MAX)) as u32,
                indices,
                material,
            });
        }
        if meshes.iter().map(|m| u64::from(m.vertices)).sum::<u64>() > limits.max_vertices {
            return Err(ModelError::Budget("vertex count over limit".into()));
        }

        let mut nodes = Vec::with_capacity(nodes_json.len());
        for (i, node) in nodes_json.iter().enumerate() {
            let children = node
                .get("children")
                .and_then(Value::as_array)
                .map(|c| {
                    c.iter()
                        .filter_map(Value::as_u64)
                        .map(|v| v as usize)
                        .collect()
                })
                .unwrap_or_default();
            nodes.push(Node {
                name: json_name(node, &format!("node{i}")),
                mesh: node.get("mesh").and_then(Value::as_u64).map(|v| v as usize),
                children,
            });
        }

        let animations = doc
            .get("animations")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .enumerate()
                    .map(|(i, animation)| Animation {
                        name: json_name(animation, &format!("animation{i}")),
                        channels: animation
                            .get("channels")
                            .and_then(Value::as_array)
                            .map(|c| c.len() as u32)
                            .unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default();

        // MDC IR has no glTF layer concept yet: one implicit layer holds every node.
        let layer = Layer {
            name: "root".into(),
            nodes: (0..nodes.len()).collect(),
        };

        Ok(Model {
            source: "gltf".into(),
            nodes,
            meshes,
            textures,
            materials,
            skeletons: Vec::new(),
            animations,
            layers: vec![layer],
        })
    }
}

fn json_name(value: &Value, fallback: &str) -> String {
    value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

// ---- Target backend ----

/// Renderer capability a device advertises (N01 §15). Independent of board id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererCapability {
    pub sprite: bool,
    pub mesh2d: bool,
    pub skeleton: bool,
    pub max_texture: u32,
    pub max_vertices: u32,
}
impl RendererCapability {
    /// Weak MCU: sprites only.
    pub fn sprite_only() -> Self {
        Self {
            sprite: true,
            mesh2d: false,
            skeleton: false,
            max_texture: 512,
            max_vertices: 0,
        }
    }
    /// GPU board: meshes and skeleton.
    pub fn full_3d() -> Self {
        Self {
            sprite: true,
            mesh2d: true,
            skeleton: true,
            max_texture: 2048,
            max_vertices: 200_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageMode {
    Mesh,
    SpriteAtlas,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePackage {
    pub target: String,
    pub mode: PackageMode,
    pub nodes: Vec<Node>,
    pub meshes: Vec<Mesh>,
    pub textures: Vec<Texture>,
    pub materials: Vec<Material>,
    pub skeletons: Vec<Skeleton>,
    pub animations: Vec<Animation>,
    pub sprites: Vec<Sprite>,
    pub layers: Vec<Layer>,
    pub estimated_bytes: u64,
}

/// Compile MDC IR into a device package for a renderer capability. A weak
/// device degrades to a sprite atlas; a strong device keeps the mesh.
pub fn compile(model: &Model, caps: &RendererCapability) -> Result<DevicePackage, ModelError> {
    let total_vertices = model.total_vertices();
    let textures_fit = model
        .textures
        .iter()
        .all(|t| (t.width <= caps.max_texture) && (t.height <= caps.max_texture));
    let mesh_ok = caps.mesh2d && total_vertices <= u64::from(caps.max_vertices) && textures_fit;

    if mesh_ok {
        return Ok(DevicePackage {
            target: "esp32-plane".into(),
            mode: PackageMode::Mesh,
            nodes: model.nodes.clone(),
            meshes: model.meshes.clone(),
            textures: model.textures.clone(),
            materials: model.materials.clone(),
            skeletons: if caps.skeleton {
                model.skeletons.clone()
            } else {
                Vec::new()
            },
            animations: model.animations.clone(),
            sprites: Vec::new(),
            layers: model.layers.clone(),
            estimated_bytes: model.texture_bytes(),
        });
    }

    // Degrade: every mesh node becomes one sprite; textures are clamped to the
    // backend limit. No mesh/skeleton data survives in the package.
    let mut sprites = Vec::new();
    for node in &model.nodes {
        let Some(mesh_index) = node.mesh else {
            continue;
        };
        let texture = model
            .meshes
            .get(mesh_index)
            .and_then(|m| m.material)
            .and_then(|mi| model.materials.get(mi))
            .and_then(|m| m.texture);
        let (width, height) = texture
            .and_then(|ti| model.textures.get(ti))
            .map(|t| {
                (
                    t.width.min(caps.max_texture),
                    t.height.min(caps.max_texture),
                )
            })
            .unwrap_or((0, 0));
        sprites.push(Sprite {
            name: node.name.clone(),
            texture,
            width,
            height,
        });
    }
    let textures: Vec<Texture> = model
        .textures
        .iter()
        .map(|t| Texture {
            width: t.width.min(caps.max_texture),
            height: t.height.min(caps.max_texture),
            ..t.clone()
        })
        .collect();
    let estimated_bytes = textures.iter().map(|t| u64::from(t.bytes)).sum();
    Ok(DevicePackage {
        target: "esp32-plane".into(),
        mode: PackageMode::SpriteAtlas,
        nodes: model.nodes.clone(),
        meshes: Vec::new(),
        textures,
        materials: model.materials.clone(),
        skeletons: Vec::new(),
        animations: model.animations.clone(),
        sprites,
        layers: model.layers.clone(),
        estimated_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_gltf(vertices: u64, texture_bytes: u64, tex_size: u32) -> Vec<u8> {
        let doc = json!({
            "asset": {"version": "2.0"},
            "nodes": [{"name": "root", "mesh": 0}],
            "meshes": [{"name": "body", "primitives": [{"attributes": {"POSITION": 0}, "indices": 1, "material": 0}]}],
            "accessors": [{"count": vertices, "type": "VEC3"}, {"count": vertices, "type": "SCALAR"}],
            "materials": [{"name": "skin", "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}],
            "textures": [{"source": 0}],
            "images": [{"name": "skinTex", "mimeType": "image/png", "width": tex_size, "height": tex_size, "bufferView": 0}],
            "bufferViews": [{"buffer": 0, "byteLength": texture_bytes}],
            "animations": [{"name": "idle", "channels": [{"target": {"node": 0, "path": "rotation"}}]}]
        });
        serde_json::to_vec(&doc).unwrap()
    }

    #[test]
    fn imports_gltf_into_ir() {
        let bytes = sample_gltf(4, 1024, 256);
        let model = GltfImporter
            .import(&bytes, &ImportLimits::default())
            .unwrap();
        assert_eq!(model.source, "gltf");
        assert_eq!(model.nodes.len(), 1);
        assert_eq!(model.meshes[0].vertices, 4);
        assert_eq!(model.meshes[0].indices, 4);
        assert_eq!(model.textures[0].bytes, 1024);
        assert_eq!(model.materials[0].texture, Some(0));
        assert_eq!(model.animations[0].name, "idle");
        assert_eq!(model.layers[0].nodes, vec![0]);
        assert_eq!(model.total_vertices(), 4);
        assert_eq!(model.texture_bytes(), 1024);
    }

    #[test]
    fn decode_bomb_is_rejected_before_allocation() {
        // Oversized input is refused before parsing.
        let limits = ImportLimits {
            max_input_bytes: 32,
            ..ImportLimits::default()
        };
        assert!(matches!(
            GltfImporter.import(&sample_gltf(4, 1024, 256), &limits),
            Err(ModelError::InputTooLarge(_, 32))
        ));
        // A declared texture larger than the texture budget is refused before
        // any decode buffer is allocated.
        let limits = ImportLimits {
            max_texture_bytes: 512,
            ..ImportLimits::default()
        };
        let err = GltfImporter
            .import(&sample_gltf(4, 4096, 256), &limits)
            .unwrap_err();
        assert!(matches!(err, ModelError::Budget(_)), "{err:?}");
        // Too many nodes / vertices are refused.
        let limits = ImportLimits {
            max_vertices: 2,
            ..ImportLimits::default()
        };
        assert!(matches!(
            GltfImporter.import(&sample_gltf(4, 1024, 256), &limits),
            Err(ModelError::Budget(_))
        ));
    }

    #[test]
    fn weak_device_bakes_sprites_strong_device_keeps_mesh() {
        let model = GltfImporter
            .import(&sample_gltf(4, 1024, 256), &ImportLimits::default())
            .unwrap();

        let weak = compile(&model, &RendererCapability::sprite_only()).unwrap();
        assert_eq!(weak.mode, PackageMode::SpriteAtlas);
        assert!(weak.meshes.is_empty(), "no mesh survives on a weak device");
        assert!(weak.skeletons.is_empty());
        assert_eq!(weak.sprites.len(), 1);
        assert_eq!(weak.sprites[0].name, "root");
        assert_eq!(weak.sprites[0].texture, Some(0));
        assert_eq!((weak.sprites[0].width, weak.sprites[0].height), (256, 256));

        let strong = compile(&model, &RendererCapability::full_3d()).unwrap();
        assert_eq!(strong.mode, PackageMode::Mesh);
        assert_eq!(strong.meshes.len(), 1);
        assert!(strong.sprites.is_empty());
    }

    #[test]
    fn weak_texture_limit_clamps_metadata_without_touching_data() {
        let model = GltfImporter
            .import(&sample_gltf(4, 1024, 1024), &ImportLimits::default())
            .unwrap();
        let caps = RendererCapability {
            max_texture: 128,
            ..RendererCapability::sprite_only()
        };
        let package = compile(&model, &caps).unwrap();
        assert_eq!(package.mode, PackageMode::SpriteAtlas);
        assert_eq!(package.textures[0].width, 128);
        assert_eq!(package.textures[0].height, 128);
        assert_eq!(
            package.textures[0].bytes, 1024,
            "declared size is unchanged"
        );
    }

    #[test]
    fn packages_round_trip_as_json() {
        let model = GltfImporter
            .import(&sample_gltf(4, 1024, 256), &ImportLimits::default())
            .unwrap();
        let package = compile(&model, &RendererCapability::sprite_only()).unwrap();
        let text = serde_json::to_string(&package).unwrap();
        assert!(text.contains("\"mode\":\"sprite-atlas\""));
        let back: DevicePackage = serde_json::from_str(&text).unwrap();
        assert_eq!(back, package);
    }
}
