//! Capability-driven model optimizer (N45). It produces a *plan* — texture
//! downscale targets and a vertex decimation ratio — for a device capability.
//! No pixels or vertices are touched; the plan is bounded before any work.
use crate::backend::RendererCapability;
use crate::ir::{Model, ModelError};
use serde::{Deserialize, Serialize};

/// Largest optimized texture total accepted (32 MiB); a compile budget.
pub const MAX_OPTIMIZED_TEXTURE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TexturePlan {
    pub name: String,
    /// Original declared size.
    pub source: [u32; 2],
    /// Target size after clamping to the device limit.
    pub target: [u32; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptimizerPlan {
    pub textures: Vec<TexturePlan>,
    /// Vertex budget the device allows (0 for a sprite-only target).
    pub vertex_limit: u32,
    /// target/original vertex ratio (1.0 = keep everything, 0.0 = bake to sprites).
    pub decimation_ratio: f32,
    /// RGB565 bytes of the target textures.
    pub estimated_texture_bytes: u64,
}

/// Plan texture downscale and vertex decimation for `caps`.
pub fn plan(model: &Model, caps: &RendererCapability) -> Result<OptimizerPlan, ModelError> {
    let mut textures = Vec::with_capacity(model.textures.len());
    let mut bytes: u64 = 0;
    for texture in &model.textures {
        let target_w = texture.width.min(caps.max_texture);
        let target_h = texture.height.min(caps.max_texture);
        bytes = bytes.saturating_add(
            u64::from(target_w)
                .saturating_mul(u64::from(target_h))
                .saturating_mul(2),
        );
        textures.push(TexturePlan {
            name: texture.name.clone(),
            source: [texture.width, texture.height],
            target: [target_w, target_h],
        });
    }
    if bytes > MAX_OPTIMIZED_TEXTURE_BYTES {
        return Err(ModelError::Budget(format!(
            "optimized textures {bytes} over {MAX_OPTIMIZED_TEXTURE_BYTES} limit"
        )));
    }

    let total = model.total_vertices();
    let decimation_ratio = if caps.max_vertices == 0 {
        0.0
    } else if total == 0 || total <= u64::from(caps.max_vertices) {
        1.0
    } else {
        (f64::from(caps.max_vertices) / total as f64) as f32
    };

    Ok(OptimizerPlan {
        textures,
        vertex_limit: caps.max_vertices,
        decimation_ratio,
        estimated_texture_bytes: bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Mesh, Texture};

    fn model_with(vertices: u32, texture: u32) -> Model {
        Model {
            source: "test".into(),
            meshes: vec![Mesh {
                name: "body".into(),
                vertices,
                indices: 0,
                material: None,
                preview_xy: Vec::new(),
            }],
            textures: vec![Texture {
                name: "skin".into(),
                width: texture,
                height: texture,
                format: "image/png".into(),
                bytes: 0,
            }],
            ..Model::default()
        }
    }

    #[test]
    fn textures_are_clamped_to_the_device_limit() {
        let plan = plan(&model_with(10, 4096), &RendererCapability::sprite_only()).unwrap();
        assert_eq!(plan.textures[0].source, [4096, 4096]);
        assert_eq!(plan.textures[0].target, [512, 512]);
        assert_eq!(plan.estimated_texture_bytes, 512 * 512 * 2);
    }

    #[test]
    fn decimation_ratio_reflects_the_vertex_budget() {
        // Fits: keep everything.
        let caps = RendererCapability {
            max_vertices: 1000,
            ..RendererCapability::full_3d()
        };
        assert_eq!(
            plan(&model_with(500, 256), &caps).unwrap().decimation_ratio,
            1.0
        );
        // Over budget: ratio scales down.
        let ratio = plan(&model_with(4000, 256), &caps)
            .unwrap()
            .decimation_ratio;
        assert!((ratio - 0.25).abs() < 1e-6, "{ratio}");
        // Sprite-only: bake everything.
        assert_eq!(
            plan(&model_with(4000, 256), &RendererCapability::sprite_only())
                .unwrap()
                .decimation_ratio,
            0.0
        );
    }
}
