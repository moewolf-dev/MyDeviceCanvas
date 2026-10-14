//! Target backends: compile MDC IR into a device package for a renderer
//! capability. Backend selection is Capability-driven — never `board == x`.
//!
//! - `Esp32PlaneBackend` ("esp32-plane"): the MCU backend. Keeps the mesh when
//!   the device can draw one, otherwise bakes the model down to a sprite atlas.
//! - `LinuxMeshBackend` ("linux-mesh"): the GPU/Linux backend. Requires mesh2d;
//!   keeps meshes, skeletons and animations at the device texture limit.
use crate::ir::{
    Animation, Layer, Material, Mesh, Model, ModelError, Node, Skeleton, Sprite, Texture,
};
use serde::{Deserialize, Serialize};

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

/// A compiler that turns MDC IR into a device package for one target family.
pub trait TargetBackend {
    fn name(&self) -> &'static str;
    fn compile(
        &self,
        model: &Model,
        caps: &RendererCapability,
    ) -> Result<DevicePackage, ModelError>;
}

/// Bake every mesh node into one sprite, clamped to the texture limit.
fn baked_sprites(model: &Model, max_texture: u32) -> Vec<Sprite> {
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
            .map(|t| (t.width.min(max_texture), t.height.min(max_texture)))
            .unwrap_or((0, 0));
        sprites.push(Sprite {
            name: node.name.clone(),
            texture,
            width,
            height,
        });
    }
    sprites
}

fn clamped_textures(model: &Model, max_texture: u32) -> Vec<Texture> {
    model
        .textures
        .iter()
        .map(|t| Texture {
            width: t.width.min(max_texture),
            height: t.height.min(max_texture),
            ..t.clone()
        })
        .collect()
}

/// The MCU backend: mesh when possible, sprite atlas otherwise.
pub struct Esp32PlaneBackend;

impl TargetBackend for Esp32PlaneBackend {
    fn name(&self) -> &'static str {
        "esp32-plane"
    }

    fn compile(
        &self,
        model: &Model,
        caps: &RendererCapability,
    ) -> Result<DevicePackage, ModelError> {
        let total_vertices = model.total_vertices();
        let textures_fit = model
            .textures
            .iter()
            .all(|t| (t.width <= caps.max_texture) && (t.height <= caps.max_texture));
        let mesh_ok = caps.mesh2d
            && !model.meshes.is_empty()
            && total_vertices <= u64::from(caps.max_vertices)
            && textures_fit;

        if mesh_ok {
            return Ok(DevicePackage {
                target: self.name().into(),
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
                sprites: model.sprites.clone(),
                layers: model.layers.clone(),
                estimated_bytes: model.texture_bytes(),
            });
        }

        // Degrade: keep author sprites, bake every mesh node into a sprite, and
        // clamp textures to the backend limit. No mesh/skeleton data survives.
        let mut sprites = model.sprites.clone();
        sprites.extend(baked_sprites(model, caps.max_texture));
        let textures = clamped_textures(model, caps.max_texture);
        let estimated_bytes = textures.iter().map(|t| u64::from(t.bytes)).sum();
        Ok(DevicePackage {
            target: self.name().into(),
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
}

/// The GPU/Linux backend: requires mesh2d and keeps geometry + skeleton.
pub struct LinuxMeshBackend;

impl TargetBackend for LinuxMeshBackend {
    fn name(&self) -> &'static str {
        "linux-mesh"
    }

    fn compile(
        &self,
        model: &Model,
        caps: &RendererCapability,
    ) -> Result<DevicePackage, ModelError> {
        if !caps.mesh2d {
            return Err(ModelError::Unsupported(
                "linux-mesh backend needs mesh2d capability".into(),
            ));
        }
        let total_vertices = model.total_vertices();
        if total_vertices > u64::from(caps.max_vertices) {
            return Err(ModelError::Budget(format!(
                "{total_vertices} vertices over {} backend limit",
                caps.max_vertices
            )));
        }
        // A model without mesh geometry (e.g. a Live2D manifest) still compiles:
        // it degrades to its author sprites instead of pretending to have a mesh.
        if model.meshes.is_empty() {
            let mut sprites = model.sprites.clone();
            sprites.extend(baked_sprites(model, caps.max_texture));
            let estimated_bytes = model.texture_bytes();
            return Ok(DevicePackage {
                target: self.name().into(),
                mode: PackageMode::SpriteAtlas,
                nodes: model.nodes.clone(),
                meshes: Vec::new(),
                textures: clamped_textures(model, caps.max_texture),
                materials: model.materials.clone(),
                skeletons: Vec::new(),
                animations: model.animations.clone(),
                sprites,
                layers: model.layers.clone(),
                estimated_bytes,
            });
        }
        // Estimated package size: textures plus 12 bytes per vertex (3×f32).
        let estimated_bytes = model.texture_bytes() + total_vertices * 12;
        Ok(DevicePackage {
            target: self.name().into(),
            mode: PackageMode::Mesh,
            nodes: model.nodes.clone(),
            meshes: model.meshes.clone(),
            textures: clamped_textures(model, caps.max_texture),
            materials: model.materials.clone(),
            skeletons: if caps.skeleton {
                model.skeletons.clone()
            } else {
                Vec::new()
            },
            animations: model.animations.clone(),
            sprites: model.sprites.clone(),
            layers: model.layers.clone(),
            estimated_bytes,
        })
    }
}

/// Compile with the default MCU backend (kept for the N05 entry point).
pub fn compile(model: &Model, caps: &RendererCapability) -> Result<DevicePackage, ModelError> {
    Esp32PlaneBackend.compile(model, caps)
}

/// Compile with an explicit backend.
pub fn compile_with(
    backend: &dyn TargetBackend,
    model: &Model,
    caps: &RendererCapability,
) -> Result<DevicePackage, ModelError> {
    backend.compile(model, caps)
}

/// Pick a backend from renderer capability only: a device that can draw meshes
/// and skeletons gets the Linux/GPU backend, everything else the MCU backend.
pub fn compile_auto(model: &Model, caps: &RendererCapability) -> Result<DevicePackage, ModelError> {
    if caps.mesh2d && caps.skeleton {
        LinuxMeshBackend.compile(model, caps)
    } else {
        Esp32PlaneBackend.compile(model, caps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gltf::GltfImporter;
    use crate::ir::ImportLimits;
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

    fn sample_model() -> Model {
        GltfImporter
            .import(&sample_gltf(4, 1024, 256), &ImportLimits::default())
            .unwrap()
    }

    #[test]
    fn weak_device_bakes_sprites_strong_device_keeps_mesh() {
        let model = sample_model();

        let weak = compile(&model, &RendererCapability::sprite_only()).unwrap();
        assert_eq!(weak.target, "esp32-plane");
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
    fn linux_backend_keeps_mesh_and_skeleton() {
        let model = sample_model();
        let package =
            compile_with(&LinuxMeshBackend, &model, &RendererCapability::full_3d()).unwrap();
        assert_eq!(package.target, "linux-mesh");
        assert_eq!(package.mode, PackageMode::Mesh);
        assert_eq!(package.meshes.len(), 1);
        assert!(package.sprites.is_empty());
        // 1024 texture bytes + 4 vertices × 12 bytes.
        assert_eq!(package.estimated_bytes, 1024 + 48);
    }

    #[test]
    fn linux_backend_refuses_a_device_without_mesh2d() {
        let model = sample_model();
        let err = compile_with(
            &LinuxMeshBackend,
            &model,
            &RendererCapability::sprite_only(),
        )
        .unwrap_err();
        assert!(matches!(err, ModelError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn auto_selection_follows_capability_not_board_id() {
        let model = sample_model();
        assert_eq!(
            compile_auto(&model, &RendererCapability::full_3d())
                .unwrap()
                .target,
            "linux-mesh"
        );
        assert_eq!(
            compile_auto(&model, &RendererCapability::sprite_only())
                .unwrap()
                .target,
            "esp32-plane"
        );
    }

    #[test]
    fn packages_round_trip_as_json() {
        let model = sample_model();
        for package in [
            compile(&model, &RendererCapability::sprite_only()).unwrap(),
            compile_with(&LinuxMeshBackend, &model, &RendererCapability::full_3d()).unwrap(),
        ] {
            let text = serde_json::to_string(&package).unwrap();
            assert!(text.contains(&format!("\"target\":\"{}\"", package.target)));
            let back: DevicePackage = serde_json::from_str(&text).unwrap();
            assert_eq!(back, package);
        }
    }
}
