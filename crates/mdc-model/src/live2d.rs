//! Live2D `model3.json` importer (Cubism 3/4/5 metadata level).
//!
//! Extracts textures, motion groups, expressions and the moc reference from the
//! manifest into MDC IR. The binary `.moc3` is never decoded — mesh, deformer and
//! parameter data stay out of the IR for now — so a weak target degrades to a
//! sprite atlas exactly like a glTF model without geometry.
use crate::gltf::json_name;
use crate::ir::{
    Animation, ImportLimits, Layer, Material, Model, ModelError, Node, Skeleton, Sprite, Texture,
};
use serde_json::Value;

pub struct Live2DImporter;

impl Live2DImporter {
    pub fn import(&self, bytes: &[u8], limits: &ImportLimits) -> Result<Model, ModelError> {
        if bytes.len() > limits.max_input_bytes {
            return Err(ModelError::InputTooLarge(
                bytes.len(),
                limits.max_input_bytes,
            ));
        }
        let doc: Value =
            serde_json::from_slice(bytes).map_err(|e| ModelError::Json(e.to_string()))?;

        let version = doc.get("Version").and_then(Value::as_u64).unwrap_or(0);
        if !(2..=5).contains(&version) {
            return Err(ModelError::Unsupported(format!(
                "Live2D model3 Version {version}"
            )));
        }

        let references = doc
            .get("FileReferences")
            .ok_or_else(|| ModelError::Unsupported("Live2D FileReferences missing".into()))?;
        if references.get("Moc").and_then(Value::as_str).is_none() {
            return Err(ModelError::Unsupported(
                "Live2D Moc reference missing".into(),
            ));
        }

        let empty = Vec::new();
        let textures_json = references
            .get("Textures")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let motion_groups = references
            .get("Motions")
            .and_then(Value::as_object)
            .map(|m| m.len())
            .unwrap_or(0);
        let expression_list = references
            .get("Expressions")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let layer_count = textures_json.len() + motion_groups + expression_list.len();
        if layer_count > limits.max_nodes {
            return Err(ModelError::Budget(format!(
                "{layer_count} Live2D parts over {} limit",
                limits.max_nodes
            )));
        }

        // One texture, material, node and sprite per declared texture. Without a
        // decoded moc3 the real atlas geometry is unknown, so sizes stay 0.
        let mut textures = Vec::with_capacity(textures_json.len());
        let mut materials = Vec::with_capacity(textures_json.len());
        let mut nodes = Vec::with_capacity(textures_json.len());
        let mut sprites = Vec::with_capacity(textures_json.len());
        for (i, texture) in textures_json.iter().enumerate() {
            let path = texture.as_str().unwrap_or("");
            let name = file_stem(path, &format!("texture{i}"));
            textures.push(Texture {
                name: name.clone(),
                width: 0,
                height: 0,
                format: texture_format(path),
                bytes: 0,
            });
            materials.push(Material {
                name: name.clone(),
                texture: Some(i),
            });
            nodes.push(Node {
                name: name.clone(),
                mesh: None,
                children: Vec::new(),
            });
            sprites.push(Sprite {
                name,
                texture: Some(i),
                width: 0,
                height: 0,
            });
        }

        // Motion groups become animations; expressions become one-channel
        // animations prefixed so they stay distinguishable.
        let mut animations = Vec::new();
        if let Some(motions) = references.get("Motions").and_then(Value::as_object) {
            for (group, files) in motions {
                animations.push(Animation {
                    name: group.clone(),
                    channels: files.as_array().map(|f| f.len() as u32).unwrap_or(0),
                });
            }
        }
        for (i, expression) in expression_list.iter().enumerate() {
            let label = expression
                .get("Name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| json_name(expression, &format!("expression{i}")));
            animations.push(Animation {
                name: format!("expression:{label}"),
                channels: 1,
            });
        }

        let layer = Layer {
            name: "root".into(),
            nodes: (0..nodes.len()).collect(),
        };

        Ok(Model {
            source: "live2d".into(),
            nodes,
            meshes: Vec::new(),
            textures,
            materials,
            skeletons: Vec::<Skeleton>::new(),
            animations,
            sprites,
            layers: vec![layer],
        })
    }
}

fn file_stem(path: &str, fallback: &str) -> String {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = file.split('.').next().unwrap_or(file);
    if stem.is_empty() {
        fallback.to_string()
    } else {
        stem.to_string()
    }
}

fn texture_format(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png".into()
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg".into()
    } else {
        "application/octet-stream".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{compile, PackageMode, RendererCapability};
    use serde_json::json;

    fn sample_model3() -> Vec<u8> {
        let doc = json!({
            "Version": 3,
            "FileReferences": {
                "Moc": "haru.moc3",
                "Textures": ["haru.4096/texture_00.png", "haru.4096/texture_01.png"],
                "Physics": "haru.physics3.json",
                "Pose": "haru.pose3.json",
                "Expressions": [
                    {"Name": "F01", "File": "expressions/F01.exp3.json"},
                    {"Name": "F02", "File": "expressions/F02.exp3.json"}
                ],
                "Motions": {
                    "Idle": [{"File": "motions/idle_00.motion3.json"}],
                    "TapBody": [
                        {"File": "motions/tap_00.motion3.json"},
                        {"File": "motions/tap_01.motion3.json"}
                    ]
                }
            }
        });
        serde_json::to_vec(&doc).unwrap()
    }

    #[test]
    fn imports_model3_into_ir() {
        let model = Live2DImporter
            .import(&sample_model3(), &ImportLimits::default())
            .unwrap();
        assert_eq!(model.source, "live2d");
        assert_eq!(model.textures.len(), 2);
        assert_eq!(model.textures[0].name, "texture_00");
        assert_eq!(model.textures[0].format, "image/png");
        assert_eq!(model.nodes.len(), 2);
        assert_eq!(model.sprites.len(), 2);
        assert_eq!(model.sprites[1].texture, Some(1));
        assert_eq!(model.materials[1].texture, Some(1));
        assert_eq!(
            model.animations.len(),
            4,
            "two motion groups + two expressions"
        );
        assert!(model
            .animations
            .iter()
            .any(|a| a.name == "TapBody" && a.channels == 2));
        assert!(model.animations.iter().any(|a| a.name == "expression:F01"));
        assert_eq!(model.layers[0].nodes, vec![0, 1]);
    }

    #[test]
    fn live2d_weak_target_degrades_to_sprites() {
        let model = Live2DImporter
            .import(&sample_model3(), &ImportLimits::default())
            .unwrap();
        let weak = compile(&model, &RendererCapability::sprite_only()).unwrap();
        assert_eq!(weak.mode, PackageMode::SpriteAtlas);
        assert_eq!(weak.sprites.len(), 2);
    }

    #[test]
    fn unsupported_version_and_missing_refs_are_rejected() {
        let bad_version = serde_json::to_vec(&json!({"Version": 9, "FileReferences": {}})).unwrap();
        assert!(matches!(
            Live2DImporter.import(&bad_version, &ImportLimits::default()),
            Err(ModelError::Unsupported(_))
        ));

        let no_moc = serde_json::to_vec(&json!({"Version": 3, "FileReferences": {}})).unwrap();
        assert!(matches!(
            Live2DImporter.import(&no_moc, &ImportLimits::default()),
            Err(ModelError::Unsupported(_))
        ));

        let no_refs = serde_json::to_vec(&json!({"Version": 3})).unwrap();
        assert!(matches!(
            Live2DImporter.import(&no_refs, &ImportLimits::default()),
            Err(ModelError::Unsupported(_))
        ));
    }

    #[test]
    fn part_budget_is_enforced() {
        let limits = ImportLimits {
            max_nodes: 1,
            ..ImportLimits::default()
        };
        assert!(matches!(
            Live2DImporter.import(&sample_model3(), &limits),
            Err(ModelError::Budget(_))
        ));
    }
}
