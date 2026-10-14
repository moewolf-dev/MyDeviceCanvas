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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A Live2D bundle: the `model3.json` manifest plus any referenced motion files
/// keyed by their manifest path. Only the files provided are read.
#[derive(Debug, Default, Clone)]
pub struct Live2DBundle {
    pub manifest: Vec<u8>,
    pub motions: BTreeMap<String, Vec<u8>>,
    /// N46: optional `physics3.json` bytes referenced by the manifest.
    pub physics: Option<Vec<u8>>,
    /// N46: optional `pose3.json` bytes referenced by the manifest.
    pub pose: Option<Vec<u8>>,
}

/// Metadata of one `motion3.json`, read from its `Meta` block and curves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MotionInfo {
    pub name: String,
    /// Number of curves, i.e. animated parameters/parts.
    pub channels: u32,
    pub duration_sec: f64,
    pub fps: f64,
    pub fade_in_sec: f64,
    pub fade_out_sec: f64,
    pub looping: bool,
}

impl MotionInfo {
    /// Discrete frame count a sampler needs, capped by the duration budget.
    pub fn frame_count(&self) -> u32 {
        if self.fps <= 0.0 || self.duration_sec <= 0.0 {
            return 0;
        }
        let frames = (self.duration_sec * self.fps).ceil();
        if frames >= f64::from(u32::MAX) {
            u32::MAX
        } else {
            frames as u32 + 1
        }
    }
}

/// A manifest compiled together with its motions.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedBundle {
    pub model: Model,
    pub motions: Vec<MotionInfo>,
}

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
            meta: BTreeMap::new(),
        })
    }

    /// Read one `motion3.json`: curve count plus the `Meta` block. Segments are
    /// not expanded into keyframes.
    pub fn import_motion(
        &self,
        name: &str,
        bytes: &[u8],
        limits: &ImportLimits,
    ) -> Result<MotionInfo, ModelError> {
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
                "Live2D motion3 Version {version}"
            )));
        }
        let curves = doc
            .get("Curves")
            .and_then(Value::as_array)
            .ok_or_else(|| ModelError::Unsupported("motion3 Curves missing".into()))?;
        if curves.len() > limits.max_nodes {
            return Err(ModelError::Budget(format!(
                "{} motion curves over {} limit",
                curves.len(),
                limits.max_nodes
            )));
        }
        let meta = doc.get("Meta");
        let number = |key: &str| meta.and_then(|m| m.get(key)).and_then(Value::as_f64);
        let duration_sec = number("Duration").unwrap_or(0.0);
        if duration_sec > MAX_MOTION_SECS {
            return Err(ModelError::Budget(format!(
                "motion duration {duration_sec}s over {MAX_MOTION_SECS}s limit"
            )));
        }
        let fps = match number("Fps") {
            Some(fps) if fps > 0.0 => fps,
            _ => 30.0,
        };
        Ok(MotionInfo {
            name: name.to_string(),
            channels: curves.len().min(u32::MAX as usize) as u32,
            duration_sec,
            fps,
            fade_in_sec: number("FadeInTime").unwrap_or(0.0),
            fade_out_sec: number("FadeOutTime").unwrap_or(0.0),
            looping: meta
                .and_then(|m| m.get("Loop"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// Compile the manifest together with the supplied motion files. The
    /// manifest's per-group stubs are replaced by one animation per motion with
    /// its real curve count; expression animations are kept.
    pub fn import_bundle(
        &self,
        bundle: &Live2DBundle,
        limits: &ImportLimits,
    ) -> Result<ImportedBundle, ModelError> {
        let mut model = self.import(&bundle.manifest, limits)?;
        model
            .animations
            .retain(|a| a.name.starts_with("expression:"));
        let mut motions = Vec::with_capacity(bundle.motions.len());
        for (path, bytes) in &bundle.motions {
            let info = self.import_motion(path, bytes, limits)?;
            model.animations.push(Animation {
                name: info.name.clone(),
                channels: info.channels,
            });
            motions.push(info);
        }
        // N46: record physics/pose metadata counts when the files are supplied.
        if let Some(physics) = &bundle.physics {
            if let Ok(doc) = serde_json::from_slice::<Value>(physics) {
                let count = doc
                    .get("PhysicsSettings")
                    .and_then(Value::as_array)
                    .map(|list| list.len())
                    .unwrap_or(0);
                model
                    .meta
                    .insert("physics.settings".into(), count.to_string());
            }
        }
        if let Some(pose) = &bundle.pose {
            if let Ok(doc) = serde_json::from_slice::<Value>(pose) {
                let count = doc
                    .get("Groups")
                    .and_then(Value::as_array)
                    .map(|list| list.len())
                    .unwrap_or(0);
                model.meta.insert("pose.groups".into(), count.to_string());
            }
        }
        Ok(ImportedBundle { model, motions })
    }
}

/// Longest motion accepted before the sampler budget is exceeded.
const MAX_MOTION_SECS: f64 = 600.0;

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

    fn sample_motion() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "Version": 3,
            "Meta": {"Duration": 2.0, "Fps": 30.0, "Loop": true, "FadeInTime": 0.5, "FadeOutTime": 0.25},
            "Curves": [
                {"Target": "Parameter", "Id": "ParamAngleX", "Segments": [0.0, 0.0]},
                {"Target": "Parameter", "Id": "ParamAngleY", "Segments": [0.0, 0.0]},
                {"Target": "Parameter", "Id": "ParamEyeOpen", "Segments": [0.0, 0.0]}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn imports_a_motion_with_meta_and_curves() {
        let info = Live2DImporter
            .import_motion("idle_00", &sample_motion(), &ImportLimits::default())
            .unwrap();
        assert_eq!(info.name, "idle_00");
        assert_eq!(info.channels, 3);
        assert_eq!(info.duration_sec, 2.0);
        assert_eq!(info.fps, 30.0);
        assert_eq!(info.fade_in_sec, 0.5);
        assert_eq!(info.fade_out_sec, 0.25);
        assert!(info.looping);
        assert_eq!(info.frame_count(), 61, "2s × 30fps + 1");
    }

    #[test]
    fn bad_motions_are_rejected() {
        let bad_version = serde_json::to_vec(&json!({"Version": 9, "Curves": []})).unwrap();
        assert!(matches!(
            Live2DImporter.import_motion("m", &bad_version, &ImportLimits::default()),
            Err(ModelError::Unsupported(_))
        ));
        let no_curves = serde_json::to_vec(&json!({"Version": 3, "Meta": {}})).unwrap();
        assert!(matches!(
            Live2DImporter.import_motion("m", &no_curves, &ImportLimits::default()),
            Err(ModelError::Unsupported(_))
        ));
        let too_long = serde_json::to_vec(&json!({
            "Version": 3,
            "Meta": {"Duration": 100000.0, "Fps": 30.0},
            "Curves": []
        }))
        .unwrap();
        assert!(matches!(
            Live2DImporter.import_motion("m", &too_long, &ImportLimits::default()),
            Err(ModelError::Budget(_))
        ));
    }

    #[test]
    fn bundle_replaces_group_stubs_with_real_channels() {
        let mut motions = BTreeMap::new();
        motions.insert("motions/idle_00.motion3.json".to_string(), sample_motion());
        let bundle = Live2DBundle {
            manifest: sample_model3(),
            motions,
            ..Live2DBundle::default()
        };
        let imported = Live2DImporter
            .import_bundle(&bundle, &ImportLimits::default())
            .unwrap();
        assert_eq!(imported.motions.len(), 1);
        assert_eq!(imported.motions[0].channels, 3);
        // Two expression animations remain; the two group stubs are gone and
        // replaced by the one real motion.
        assert_eq!(imported.model.animations.len(), 3);
        assert!(imported
            .model
            .animations
            .iter()
            .any(|a| a.name == "motions/idle_00.motion3.json" && a.channels == 3));
        assert!(imported
            .model
            .animations
            .iter()
            .any(|a| a.name == "expression:F01"));
    }

    #[test]
    fn bundle_records_physics_and_pose_metadata() {
        let physics = serde_json::to_vec(&json!({
            "Version": 3,
            "PhysicsSettings": [{"Id": "hair"}, {"Id": "body"}]
        }))
        .unwrap();
        let pose = serde_json::to_vec(&json!({
            "Version": 3,
            "Groups": [{"Name": "LeftArm", "Ids": []}]
        }))
        .unwrap();
        let bundle = Live2DBundle {
            manifest: sample_model3(),
            physics: Some(physics),
            pose: Some(pose),
            ..Live2DBundle::default()
        };
        let imported = Live2DImporter
            .import_bundle(&bundle, &ImportLimits::default())
            .unwrap();
        assert_eq!(imported.model.meta.get("physics.settings").unwrap(), "2");
        assert_eq!(imported.model.meta.get("pose.groups").unwrap(), "1");
    }
}
