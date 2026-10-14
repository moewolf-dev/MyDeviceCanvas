//! `mdc-model` — Model Compiler / MDC IR (N05 skeleton, deepened in N08).
//!
//! Pipeline: source model -> Importer -> MDC IR -> Target backend -> device
//! package. Two importers (glTF 2.0 JSON/GLB and Live2D `model3.json`) and two
//! target backends (MCU sprite/mesh and Linux mesh) share one IR, so more
//! formats and backends can be added without touching Core.
//!
//! This crate depends only on serde; it never depends on `mdc-core`, and model
//! formats never enter the device runtime (see
//! `docs/render-topology-decision.md` §4 and `docs/model-compiler.md`).
mod atlas;
mod backend;
mod gltf;
mod ir;
mod live2d;
mod package;

pub use atlas::{AtlasBaker, AtlasPlan, MotionFrames};
pub use backend::{
    compile, compile_auto, compile_with, DevicePackage, Esp32PlaneBackend, LinuxMeshBackend,
    PackageMode, RendererCapability, TargetBackend,
};
pub use gltf::GltfImporter;
pub use ir::{
    Animation, ImportLimits, Layer, Material, Mesh, Model, ModelError, Node, Skeleton, Sprite,
    Texture,
};
pub use live2d::{ImportedBundle, Live2DBundle, Live2DImporter, MotionInfo};
pub use package::{
    compile_to_package, fnv1a, MdcPackage, PackageManifest, MAX_PACKAGE_BYTES, PACKAGE_VERSION,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A glTF JSON model written through the whole pipeline into two packages.
    #[test]
    fn source_model_flows_to_both_backends() {
        let doc = json!({
            "asset": {"version": "2.0"},
            "nodes": [{"name": "root", "mesh": 0}],
            "meshes": [{"name": "body", "primitives": [{"attributes": {"POSITION": 0}, "material": 0}]}],
            "accessors": [{"count": 120, "type": "VEC3"}],
            "materials": [{"name": "skin", "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}],
            "textures": [{"source": 0}],
            "images": [{"name": "skin", "mimeType": "image/png", "width": 1024, "height": 1024}],
            "animations": [{"name": "idle", "channels": [{"target": {"node": 0, "path": "scale"}}]}]
        });
        let bytes = serde_json::to_vec(&doc).unwrap();
        let model = GltfImporter
            .import(&bytes, &ImportLimits::default())
            .unwrap();
        assert_eq!(model.source, "gltf");

        let mcu = compile(&model, &RendererCapability::sprite_only()).unwrap();
        assert_eq!(mcu.mode, PackageMode::SpriteAtlas);

        let gpu = compile_auto(&model, &RendererCapability::full_3d()).unwrap();
        assert_eq!(gpu.target, "linux-mesh");
        assert_eq!(gpu.mode, PackageMode::Mesh);
        assert_eq!(gpu.meshes.len(), 1);
    }
}
