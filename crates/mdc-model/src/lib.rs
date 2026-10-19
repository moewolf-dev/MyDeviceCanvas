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
mod ktx2;
mod live2d;
mod optimizer;
mod package;

pub use atlas::{AtlasBaker, AtlasPlan, MotionFrames};
pub use backend::{
    compile, compile_auto, compile_with, AtlasBlob, AtlasClip, AtlasMip, DevicePackage,
    Esp32PlaneBackend, LinuxMeshBackend, PackageMode, RendererCapability, TargetBackend,
    TextureBlob, TextureMip,
};
mod meshopt;
pub use gltf::{
    reserved_extensions, reserved_extensions_in_bytes, GltfImporter, RESERVED_EXTENSIONS,
    UNSUPPORTED_EXTENSIONS,
};
pub use ir::{
    Animation, AnimationTrack, Camera, ImportLimits, Layer, Light, Material, Mesh, MeshGeometry,
    Model, ModelError, ModelSummary, Node, Skeleton, Sprite, Texture, UvTransform,
    ValidationReport, VertexCache, MAX_GEOMETRY_INDICES, MAX_GEOMETRY_VERTICES,
};
pub use ktx2::{is_ktx2, Ktx2Header, KTX2_HEADER_BYTES, KTX2_IDENTIFIER};
pub use live2d::{
    sample_segments, ImportedBundle, Live2DBundle, Live2DImporter, MotionCurve, MotionInfo,
    MAX_MOTION_SEGMENTS,
};
pub use meshopt::{
    decode_gltf_buffer, decode_index_buffer, decode_index_sequence, decode_vertex_buffer,
    MeshoptFilter, MeshoptMode,
};
pub use optimizer::{plan as plan_optimization, OptimizerPlan, TexturePlan};
pub use package::{
    compile_to_package, fnv1a, package_cache_misses, MdcPackage, PackageAssembler, PackageManifest,
    PackageVerification, CACHE_REPORT_SIZE, MAX_PACKAGE_BYTES, PACKAGE_VERSION,
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
