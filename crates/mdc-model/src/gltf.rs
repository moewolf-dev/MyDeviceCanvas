//! Real glTF 2.0 importer: accepts the JSON (`.gltf`) form and the binary (`.glb`)
//! container, validates buffer/bufferView/accessor bounds, and probes embedded
//! PNG/JPEG headers for real pixel dimensions. Image bytes are never decoded —
//! only the header is read — so a decode bomb is rejected before any decode
//! buffer is allocated.
use crate::ir::{
    Animation, ImportLimits, Layer, Material, Mesh, Model, ModelError, Node, Skeleton, Texture,
    UvTransform, MAX_GEOMETRY_INDICES, MAX_GEOMETRY_VERTICES,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub struct GltfImporter;

/// JSON chunk magic in a GLB container ("JSON").
const GLB_CHUNK_JSON: u32 = 0x4E4F_534A;
/// BIN chunk magic in a GLB container ("BIN\0").
const GLB_CHUNK_BIN: u32 = 0x004E_4942;
/// N123: most morph targets imported per mesh (a preview budget, not a promise).
pub const MAX_MORPH_TARGETS: usize = 16;

#[derive(Debug, Clone, Copy)]
struct BufferView {
    buffer: u64,
    byte_offset: u64,
    byte_length: u64,
}

struct Buffer {
    declared: u64,
    data: Option<Vec<u8>>,
}

impl GltfImporter {
    pub fn import(&self, bytes: &[u8], limits: &ImportLimits) -> Result<Model, ModelError> {
        if bytes.len() > limits.max_input_bytes {
            return Err(ModelError::InputTooLarge(
                bytes.len(),
                limits.max_input_bytes,
            ));
        }
        let (json, bin) = split_container(bytes)?;
        let doc: Value =
            serde_json::from_slice(json).map_err(|e| ModelError::Json(e.to_string()))?;
        if let Some(version) = doc
            .get("asset")
            .and_then(|a| a.get("version"))
            .and_then(Value::as_str)
        {
            if !version.starts_with('2') {
                return Err(ModelError::Unsupported(format!("glTF version {version}")));
            }
        }

        let empty = Vec::new();
        let buffers = resolve_buffers(&doc, bin, limits.max_input_bytes)?;
        let buffer_views = read_buffer_views(&doc, &buffers)?;
        let accessors = doc
            .get("accessors")
            .and_then(Value::as_array)
            .unwrap_or(&empty);

        let view_bytes = |index: u64| -> Option<&[u8]> {
            let view = buffer_views.get(index as usize)?;
            if !view_has_valid_data(view, &buffers) {
                return None;
            }
            let buffer = buffers.get(view.buffer as usize)?;
            let data = buffer.data.as_ref()?;
            let start = usize::try_from(view.byte_offset).ok()?;
            let len = usize::try_from(view.byte_length).ok()?;
            data.get(start..start.checked_add(len)?)
        };

        // Accessor count is validated against its bufferView before use.
        let accessor_count = |index: u64| -> Result<u64, ModelError> {
            let accessor = accessors
                .get(index as usize)
                .ok_or_else(|| ModelError::Unsupported(format!("accessor {index} missing")))?;
            validate_accessor(accessor, &buffer_views)
        };

        // Images -> textures. Probe the embedded header for real dimensions;
        // fall back to the declared size when the bytes are not on disk.
        let images = doc
            .get("images")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let mut textures = Vec::with_capacity(images.len());
        let mut retained_bytes: u64 = 0;
        for (i, image) in images.iter().enumerate() {
            let declared_w = image.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
            let declared_h = image.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
            let view_index = image.get("bufferView").and_then(Value::as_u64);
            // N323: keep the actual image bytes (bufferView or `data:` URI) so the
            // compiler can use the model's own texture. Bounded by max_texture_bytes.
            let raw: Option<Vec<u8>> = if let Some(bytes) = view_index.and_then(view_bytes) {
                Some(bytes.to_vec())
            } else {
                image
                    .get("uri")
                    .and_then(Value::as_str)
                    .filter(|uri| uri.starts_with("data:"))
                    .and_then(|uri| decode_data_uri(uri).ok())
            };
            let bytes_len = raw
                .as_ref()
                .map(|raw| raw.len() as u64)
                .or_else(|| {
                    view_index
                        .and_then(|bv| buffer_views.get(bv as usize))
                        .map(|v| v.byte_length)
                })
                .unwrap_or(0);
            let probed = raw.as_deref().and_then(probe_image_size);
            let (width, height) = probed.unwrap_or((declared_w, declared_h));
            let data = match raw {
                Some(raw)
                    if retained_bytes.saturating_add(raw.len() as u64)
                        <= limits.max_texture_bytes =>
                {
                    retained_bytes += raw.len() as u64;
                    Some(raw)
                }
                _ => None,
            };
            textures.push(Texture {
                name: json_name(image, &format!("image{i}")),
                width,
                height,
                format: image
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("image/png")
                    .to_string(),
                bytes: bytes_len.min(u64::from(u32::MAX)) as u32,
                data,
                wrap: None,
            });
        }
        let declared_bytes: u64 = textures.iter().map(|t| u64::from(t.bytes)).sum();
        if declared_bytes > limits.max_texture_bytes {
            return Err(ModelError::Budget(format!(
                "declared texture bytes {declared_bytes} over {} limit",
                limits.max_texture_bytes
            )));
        }

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

        // N334: record the sampler wrap on the image-indexed texture.
        let samplers_json = doc.get("samplers").and_then(Value::as_array);
        for texture in textures_json {
            let Some(source) = texture.get("source").and_then(Value::as_u64) else {
                continue;
            };
            let wrap = texture
                .get("sampler")
                .and_then(Value::as_u64)
                .and_then(|s| samplers_json.and_then(|list| list.get(s as usize)))
                .map(|sampler| {
                    let clamp = |key: &str| sampler.get(key).and_then(Value::as_u64) == Some(33071);
                    if clamp("wrapS") || clamp("wrapT") {
                        "clamp"
                    } else {
                        "repeat"
                    }
                });
            if let Some(wrap) = wrap {
                if let Some(texture) = textures.get_mut(source as usize) {
                    texture.wrap = Some(wrap.to_string());
                }
            }
        }

        let materials_json = doc
            .get("materials")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let mut materials = Vec::with_capacity(materials_json.len());
        for (i, material) in materials_json.iter().enumerate() {
            let pbr = material.get("pbrMetallicRoughness");
            let texture = pbr
                .and_then(|p| p.get("baseColorTexture"))
                .and_then(|b| b.get("index"))
                .and_then(Value::as_u64)
                .and_then(resolve_texture);
            // N174: baseColorFactor -> 8-bit RGB. N333: index 3 is alpha.
            let color = pbr
                .and_then(|p| p.get("baseColorFactor"))
                .and_then(Value::as_array)
                .and_then(|list| {
                    let channel = |k: usize| {
                        list.get(k)
                            .and_then(Value::as_f64)
                            .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                    };
                    Some([channel(0)?, channel(1)?, channel(2)?])
                });
            let alpha = pbr
                .and_then(|p| p.get("baseColorFactor"))
                .and_then(Value::as_array)
                .and_then(|list| list.get(3))
                .and_then(Value::as_f64)
                .map(|v| v.clamp(0.0, 1.0) as f32);
            // N343: metallic/roughness factors.
            let metallic = pbr
                .and_then(|p| p.get("metallicFactor"))
                .and_then(Value::as_f64)
                .map(|v| v.clamp(0.0, 1.0) as f32);
            let roughness = pbr
                .and_then(|p| p.get("roughnessFactor"))
                .and_then(Value::as_f64)
                .map(|v| v.clamp(0.0, 1.0) as f32);
            // N363: emissiveFactor (top-level) and KHR_materials_unlit.
            let emissive = material
                .get("emissiveFactor")
                .and_then(Value::as_array)
                .and_then(|list| {
                    let channel = |k: usize| {
                        list.get(k)
                            .and_then(Value::as_f64)
                            .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                    };
                    Some([channel(0)?, channel(1)?, channel(2)?])
                });
            let unlit = material
                .get("extensions")
                .and_then(|e| e.get("KHR_materials_unlit"))
                .map(|_| true);
            // N373: KHR_texture_transform on the base-colour texture.
            let uv_transform = pbr
                .and_then(|p| p.get("baseColorTexture"))
                .and_then(|b| b.get("extensions"))
                .and_then(|e| e.get("KHR_texture_transform"))
                .map(|t| {
                    let pair = |key: &str, default: [f32; 2]| -> [f32; 2] {
                        t.get(key)
                            .and_then(Value::as_array)
                            .and_then(|list| {
                                Some([
                                    list.first()?.as_f64()? as f32,
                                    list.get(1)?.as_f64()? as f32,
                                ])
                            })
                            .unwrap_or(default)
                    };
                    UvTransform {
                        offset: pair("offset", [0.0, 0.0]),
                        rotation: t
                            .get("rotation")
                            .and_then(Value::as_f64)
                            .map(|v| v as f32)
                            .unwrap_or(0.0),
                        scale: pair("scale", [1.0, 1.0]),
                    }
                })
                .filter(|t| !t.is_identity());
            // N374: KHR_materials_emissive_strength (default 1.0).
            let emissive_strength = material
                .get("extensions")
                .and_then(|e| e.get("KHR_materials_emissive_strength"))
                .and_then(|s| s.get("emissiveStrength"))
                .and_then(Value::as_f64)
                .map(|v| v.max(0.0) as f32);
            materials.push(Material {
                name: json_name(material, &format!("material{i}")),
                texture,
                color,
                alpha_mode: material
                    .get("alphaMode")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                alpha_cutoff: material
                    .get("alphaCutoff")
                    .and_then(Value::as_f64)
                    .map(|v| v as f32),
                alpha,
                metallic,
                roughness,
                emissive,
                unlit,
                uv_transform,
                emissive_strength,
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
            let mut preview_xy = Vec::new();
            let mut preview_indices = Vec::new();
            let mut preview_positions: Vec<[f32; 3]> = Vec::new();
            let mut skin_joints: Vec<[u16; 4]> = Vec::new();
            let mut skin_weights: Vec<[f32; 4]> = Vec::new();
            let mut preview_uv: Vec<[f32; 2]> = Vec::new();
            let mut morph_targets: Vec<Vec<[f32; 3]>> = Vec::new();
            let mut geometry: Option<crate::ir::MeshGeometry> = None;
            if let Some(primitives) = mesh.get("primitives").and_then(Value::as_array) {
                for primitive in primitives {
                    let attributes = primitive.get("attributes");
                    if let Some(position) = attributes
                        .and_then(|a| a.get("POSITION"))
                        .and_then(Value::as_u64)
                    {
                        vertices = vertices.saturating_add(accessor_count(position)?);
                        if preview_positions.is_empty() {
                            if let Some(accessor) = accessors.get(position as usize) {
                                let bytes = accessor
                                    .get("bufferView")
                                    .and_then(Value::as_u64)
                                    .and_then(&view_bytes);
                                preview_positions = decode_preview_positions(accessor, bytes);
                                preview_xy = preview_positions.iter().map(project_xy).collect();
                            }
                        }
                        // N164/N274: bounded full geometry; every primitive with
                        // POSITION is appended into one merged geometry.
                        let remaining = MAX_GEOMETRY_VERTICES
                            .saturating_sub(geometry.as_ref().map_or(0, |g| g.positions.len()));
                        if remaining > 0 {
                            if let Some(accessor) = accessors.get(position as usize) {
                                let bytes = accessor
                                    .get("bufferView")
                                    .and_then(Value::as_u64)
                                    .and_then(&view_bytes);
                                let positions = decode_positions(accessor, bytes, remaining);
                                if !positions.is_empty() {
                                    let pair = |source: &Value, key: &str| -> Option<(&Value, Option<&[u8]>)> {
                                        let index = source.get(key).and_then(Value::as_u64)?;
                                        let accessor = accessors.get(index as usize)?;
                                        let bytes = accessor
                                            .get("bufferView")
                                            .and_then(Value::as_u64)
                                            .and_then(&view_bytes);
                                        Some((accessor, bytes))
                                    };
                                    let attributes_value =
                                        attributes.cloned().unwrap_or(Value::Null);
                                    let indices = pair(primitive, "indices")
                                        .map(|(a, b)| {
                                            decode_geometry_indices(a, b, MAX_GEOMETRY_INDICES)
                                        })
                                        .unwrap_or_default();
                                    let uv = pair(&attributes_value, "TEXCOORD_0")
                                        .map(|(a, b)| decode_uv_mesh(a, b, MAX_GEOMETRY_VERTICES))
                                        .unwrap_or_default();
                                    // N326: NORMAL, parallel to positions.
                                    let normals = pair(&attributes_value, "NORMAL")
                                        .map(|(a, b)| decode_normals(a, b, MAX_GEOMETRY_VERTICES))
                                        .unwrap_or_default();
                                    // N364: COLOR_0.
                                    let colors = pair(&attributes_value, "COLOR_0")
                                        .map(|(a, b)| decode_colors(a, b, MAX_GEOMETRY_VERTICES))
                                        .unwrap_or_default();
                                    let skin_joints = pair(&attributes_value, "JOINTS_0")
                                        .map(|(a, b)| {
                                            decode_joints_mesh(a, b, MAX_GEOMETRY_VERTICES)
                                        })
                                        .unwrap_or_default();
                                    let skin_weights = pair(&attributes_value, "WEIGHTS_0")
                                        .map(|(a, b)| {
                                            decode_weights_mesh(a, b, MAX_GEOMETRY_VERTICES)
                                        })
                                        .unwrap_or_default();
                                    // N224: geometry morph targets (POSITION deltas).
                                    let mut morph_targets = Vec::new();
                                    if let Some(targets) =
                                        primitive.get("targets").and_then(Value::as_array)
                                    {
                                        for target in targets.iter().take(MAX_MORPH_TARGETS) {
                                            let Some(index) =
                                                target.get("POSITION").and_then(Value::as_u64)
                                            else {
                                                continue;
                                            };
                                            let Some(accessor) = accessors.get(index as usize)
                                            else {
                                                continue;
                                            };
                                            let bytes = accessor
                                                .get("bufferView")
                                                .and_then(Value::as_u64)
                                                .and_then(&view_bytes);
                                            morph_targets
                                                .push(decode_positions(accessor, bytes, remaining));
                                        }
                                    }
                                    let primitive_geometry = crate::ir::MeshGeometry {
                                        positions,
                                        indices,
                                        uv,
                                        normals,
                                        colors,
                                        skin_joints,
                                        skin_weights,
                                        morph_targets,
                                    };
                                    match geometry.as_mut() {
                                        Some(merged) => merged.merge(primitive_geometry),
                                        None => geometry = Some(primitive_geometry),
                                    }
                                }
                            }
                        }
                    }
                    if let (Some(joint_accessor), Some(weight_accessor)) = (
                        attributes
                            .and_then(|a| a.get("JOINTS_0"))
                            .and_then(Value::as_u64),
                        attributes
                            .and_then(|a| a.get("WEIGHTS_0"))
                            .and_then(Value::as_u64),
                    ) {
                        if skin_joints.is_empty() {
                            let joints = accessors.get(joint_accessor as usize).map(|a| {
                                let bytes = a
                                    .get("bufferView")
                                    .and_then(Value::as_u64)
                                    .and_then(&view_bytes);
                                decode_joints(a, bytes)
                            });
                            let weights = accessors.get(weight_accessor as usize).map(|a| {
                                let bytes = a
                                    .get("bufferView")
                                    .and_then(Value::as_u64)
                                    .and_then(&view_bytes);
                                decode_weights(a, bytes)
                            });
                            if let (Some(joints), Some(weights)) = (joints, weights) {
                                if !joints.is_empty() && joints.len() == weights.len() {
                                    skin_joints = joints;
                                    skin_weights = weights;
                                }
                            }
                        }
                    }
                    if let Some(uv_accessor) = attributes
                        .and_then(|a| a.get("TEXCOORD_0"))
                        .and_then(Value::as_u64)
                    {
                        if preview_uv.is_empty() {
                            if let Some(accessor) = accessors.get(uv_accessor as usize) {
                                let bytes = accessor
                                    .get("bufferView")
                                    .and_then(Value::as_u64)
                                    .and_then(&view_bytes);
                                preview_uv = decode_uv(accessor, bytes);
                            }
                        }
                    }
                    if let Some(index) = primitive.get("indices").and_then(Value::as_u64) {
                        indices = indices
                            .saturating_add(accessor_count(index)?.min(u64::from(u32::MAX)) as u32);
                        if preview_indices.is_empty() {
                            if let Some(accessor) = accessors.get(index as usize) {
                                let bytes = accessor
                                    .get("bufferView")
                                    .and_then(Value::as_u64)
                                    .and_then(&view_bytes);
                                preview_indices = decode_preview_indices(accessor, bytes);
                            }
                        }
                    }
                    if let Some(mat) = primitive.get("material").and_then(Value::as_u64) {
                        material = Some(mat as usize);
                    }
                    // N123: morph targets (POSITION deltas), first primitive only.
                    if morph_targets.is_empty() {
                        if let Some(targets) = primitive.get("targets").and_then(Value::as_array) {
                            for target in targets.iter().take(MAX_MORPH_TARGETS) {
                                let Some(accessor_index) =
                                    target.get("POSITION").and_then(Value::as_u64)
                                else {
                                    continue;
                                };
                                let Some(accessor) = accessors.get(accessor_index as usize) else {
                                    continue;
                                };
                                let bytes = accessor
                                    .get("bufferView")
                                    .and_then(Value::as_u64)
                                    .and_then(&view_bytes);
                                morph_targets.push(decode_preview_positions(accessor, bytes));
                            }
                        }
                    }
                }
            }
            // N123: default morph weights (`mesh.weights`), capped to the targets.
            let morph_weights: Vec<f32> = mesh
                .get("weights")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .take(morph_targets.len().max(MAX_MORPH_TARGETS))
                        .map(|w| w.as_f64().unwrap_or(0.0) as f32)
                        .collect()
                })
                .unwrap_or_default();
            meshes.push(Mesh {
                name: json_name(mesh, &format!("mesh{i}")),
                vertices: vertices.min(u64::from(u32::MAX)) as u32,
                indices,
                primitives: mesh
                    .get("primitives")
                    .and_then(Value::as_array)
                    .map(|p| p.len().min(u32::MAX as usize) as u32)
                    .unwrap_or(0),
                material,
                preview_xy,
                preview_indices,
                preview_positions,
                skin_joints,
                skin_weights,
                preview_uv,
                morph_targets,
                morph_weights,
                geometry,
            });
        }
        if meshes.iter().map(|m| u64::from(m.vertices)).sum::<u64>() > limits.max_vertices {
            return Err(ModelError::Budget("vertex count over limit".into()));
        }

        let nodes_json = doc.get("nodes").and_then(Value::as_array).unwrap_or(&empty);
        if nodes_json.len() > limits.max_nodes {
            return Err(ModelError::Budget(format!(
                "{} nodes over {} limit",
                nodes_json.len(),
                limits.max_nodes
            )));
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
            // N109: read the node's local transform (TRS or matrix).
            let (translation, rotation, scale) = match node.get("matrix").and_then(Value::as_array)
            {
                Some(m) if m.len() == 16 => decompose_matrix(m),
                _ => (
                    read_f32_3(node.get("translation"), [0.0, 0.0, 0.0]),
                    read_f32_4(node.get("rotation"), [0.0, 0.0, 0.0, 1.0]),
                    read_f32_3(node.get("scale"), [1.0, 1.0, 1.0]),
                ),
            };
            nodes.push(Node {
                name: json_name(node, &format!("node{i}")),
                mesh: node.get("mesh").and_then(Value::as_u64).map(|v| v as usize),
                children,
                translation,
                rotation,
                scale,
            });
        }

        let animations: Vec<Animation> = doc
            .get("animations")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .enumerate()
                    .map(|(i, animation)| {
                        let channels = animation
                            .get("channels")
                            .and_then(Value::as_array)
                            .map(|c| c.len() as u32)
                            .unwrap_or(0);
                        let mut tracks = Vec::new();
                        if let Some(channel_list) =
                            animation.get("channels").and_then(Value::as_array)
                        {
                            let samplers = animation.get("samplers").and_then(Value::as_array);
                            for channel in channel_list {
                                let target = channel.get("target");
                                let node =
                                    target.and_then(|t| t.get("node")).and_then(Value::as_u64);
                                let Some(node) = node else { continue };
                                if node as usize >= nodes_json.len() {
                                    continue;
                                }
                                let path = target
                                    .and_then(|t| t.get("path"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                let components = match path {
                                    "rotation" => 4usize,
                                    "translation" | "scale" => 3usize,
                                    // N124: morph-target weights; component count
                                    // is derived from the flat output below.
                                    "weights" => 1usize,
                                    _ => continue,
                                };
                                let sampler_index = channel
                                    .get("sampler")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(u64::MAX);
                                let sampler = samplers
                                    .and_then(|s| s.get(sampler_index as usize))
                                    .cloned()
                                    .unwrap_or(Value::Null);
                                let read = |key: &str, components: usize| -> Vec<f32> {
                                    let Some(index) = sampler.get(key).and_then(Value::as_u64)
                                    else {
                                        return Vec::new();
                                    };
                                    let Some(accessor) = accessors.get(index as usize) else {
                                        return Vec::new();
                                    };
                                    let bytes = accessor
                                        .get("bufferView")
                                        .and_then(Value::as_u64)
                                        .and_then(&view_bytes);
                                    let cap = limits.max_nodes.saturating_mul(MAX_MORPH_TARGETS);
                                    decode_float_accessor(accessor, bytes, components, cap)
                                };
                                let times = read("input", 1);
                                if times.is_empty() {
                                    continue;
                                }
                                // For "weights" the output is a flat scalar stream:
                                // one value per (key, target).
                                let values = if path == "weights" {
                                    read("output", 1)
                                } else {
                                    read("output", components)
                                };
                                if values.len() < components {
                                    continue;
                                }
                                if path == "weights" && values.len() % times.len() != 0 {
                                    continue;
                                }
                                let interpolation = sampler
                                    .get("interpolation")
                                    .and_then(Value::as_str)
                                    .unwrap_or("LINEAR")
                                    .to_string();
                                tracks.push(crate::ir::AnimationTrack {
                                    node: node as usize,
                                    path: path.to_string(),
                                    interpolation,
                                    times,
                                    values,
                                });
                            }
                        }
                        Animation {
                            name: json_name(animation, &format!("animation{i}")),
                            channels,
                            tracks,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // MDC IR has no glTF layer concept yet: one implicit layer holds every node.
        let layer = Layer {
            name: "root".into(),
            nodes: (0..nodes.len()).collect(),
        };

        let (meta, expressions) = read_vrm(&doc);
        let mut animations = animations;
        animations.extend(expressions);

        Ok(Model {
            source: "gltf".into(),
            nodes,
            meshes,
            textures,
            materials,
            skeletons: read_skeletons(&doc, nodes_json.len(), accessors, &buffer_views, &buffers)?,
            animations,
            sprites: Vec::new(),
            layers: vec![layer],
            meta,
        })
    }
}

/// Split a `.glb` container into its JSON and BIN chunks, or pass a `.gltf` JSON
/// buffer through unchanged. Truncated headers and over-long chunks are rejected
/// without reading past the declared length.
fn split_container(bytes: &[u8]) -> Result<(&[u8], Option<&[u8]>), ModelError> {
    if bytes.starts_with(b"glTF") {
        if bytes.len() < 12 {
            return Err(ModelError::Unsupported("glb header truncated".into()));
        }
        let version =
            read_u32(bytes, 4).ok_or_else(|| ModelError::Unsupported("glb version".into()))?;
        if version != 2 {
            return Err(ModelError::Unsupported(format!("glb version {version}")));
        }
        let total = read_u32(bytes, 8)
            .ok_or_else(|| ModelError::Unsupported("glb length".into()))?
            as usize;
        if total > bytes.len() || total < 12 {
            return Err(ModelError::Unsupported(
                "glb declared length out of range".into(),
            ));
        }
        let mut offset = 12usize;
        let mut json = None;
        let mut bin = None;
        while offset + 8 <= total {
            let len = read_u32(bytes, offset).unwrap() as usize;
            let kind = read_u32(bytes, offset + 4).unwrap();
            let start = offset + 8;
            let stop = start
                .checked_add(len)
                .ok_or_else(|| ModelError::Unsupported("glb chunk overflow".into()))?;
            if stop > total {
                return Err(ModelError::Unsupported("glb chunk over length".into()));
            }
            match kind {
                GLB_CHUNK_JSON => json = Some(&bytes[start..stop]),
                GLB_CHUNK_BIN => bin = Some(&bytes[start..stop]),
                _ => {}
            }
            offset = stop;
        }
        let json = json.ok_or_else(|| ModelError::Unsupported("glb missing JSON chunk".into()))?;
        Ok((json, bin))
    } else {
        Ok((bytes, None))
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Resolve every declared buffer. A `data:` URI is base64-decoded; the GLB BIN
/// chunk satisfies buffer 0; external URIs stay unresolved (we never read the
/// workspace during compile). The decoded total is capped by `max_bytes`.
fn resolve_buffers(
    doc: &Value,
    bin: Option<&[u8]>,
    max_bytes: usize,
) -> Result<Vec<Buffer>, ModelError> {
    let mut out = Vec::new();
    let mut total = 0u64;
    if let Some(buffers) = doc.get("buffers").and_then(Value::as_array) {
        for (i, buffer) in buffers.iter().enumerate() {
            let declared = buffer
                .get("byteLength")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            total = total.saturating_add(declared);
            let data = match buffer.get("uri").and_then(Value::as_str) {
                None => {
                    if i == 0 {
                        bin.map(|data| data.to_vec())
                    } else {
                        None
                    }
                }
                Some(uri) if uri.starts_with("data:") => Some(decode_data_uri(uri)?),
                Some(_) => None,
            };
            out.push(Buffer { declared, data });
        }
    } else if let Some(bin) = bin {
        total = bin.len() as u64;
        out.push(Buffer {
            declared: bin.len() as u64,
            data: Some(bin.to_vec()),
        });
    }
    if total > max_bytes as u64 {
        return Err(ModelError::Budget(format!(
            "resolved buffers {total} over {max_bytes} limit"
        )));
    }
    Ok(out)
}

fn read_buffer_views(doc: &Value, buffers: &[Buffer]) -> Result<Vec<BufferView>, ModelError> {
    let mut out = Vec::new();
    let Some(views) = doc.get("bufferViews").and_then(Value::as_array) else {
        return Ok(out);
    };
    for view in views {
        let buffer = view.get("buffer").and_then(Value::as_u64).unwrap_or(0);
        let byte_offset = view.get("byteOffset").and_then(Value::as_u64).unwrap_or(0);
        let byte_length = view.get("byteLength").and_then(Value::as_u64).unwrap_or(0);
        let parsed = BufferView {
            buffer,
            byte_offset,
            byte_length,
        };
        if !buffers.is_empty() {
            let target = buffers.get(buffer as usize).ok_or_else(|| {
                ModelError::Unsupported(format!("bufferView references buffer {buffer}"))
            })?;
            let available = target
                .data
                .as_ref()
                .map(|d| d.len() as u64)
                .unwrap_or(target.declared);
            let end = byte_offset.saturating_add(byte_length);
            if end > available {
                return Err(ModelError::Unsupported(format!(
                    "bufferView {byte_offset}+{byte_length} over buffer size {available}"
                )));
            }
        }
        out.push(parsed);
    }
    Ok(out)
}

/// Read up to 64 VEC3 float positions and map XY from roughly [-1, 1] into pixels.
fn decode_preview_indices(accessor: &Value, bytes: Option<&[u8]>) -> Vec<u16> {
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let component = accessor.get("componentType").and_then(Value::as_u64);
    let mut out = Vec::new();
    match component {
        Some(5123) => {
            let mut offset = 0;
            while offset + 2 <= bytes.len() && out.len() < 192 {
                out.push(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]));
                offset += 2;
            }
        }
        Some(5125) => {
            let mut offset = 0;
            while offset + 4 <= bytes.len() && out.len() < 192 {
                let value =
                    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]));
                if value > u32::from(u16::MAX) {
                    break;
                }
                out.push(value as u16);
                offset += 4;
            }
        }
        _ => {}
    }
    out
}

/// N89/N109: read up to 64 VEC3 float positions in local space. `project_xy`
/// turns them into the pixel preview; a skinned bake deforms them first.
fn decode_preview_positions(accessor: &Value, bytes: Option<&[u8]>) -> Vec<[f32; 3]> {
    decode_positions(accessor, bytes, 64)
}

/// N163: read up to `max` VEC3 float positions (the full-mesh variant).
fn decode_positions(accessor: &Value, bytes: Option<&[u8]>, max: usize) -> Vec<[f32; 3]> {
    if accessor.get("componentType").and_then(Value::as_u64) != Some(5126) {
        return Vec::new();
    }
    if accessor.get("type").and_then(Value::as_str) != Some("VEC3") {
        return Vec::new();
    }
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + 12 <= bytes.len() && out.len() < max {
        let x = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]));
        let y = f32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap_or([0; 4]));
        let z = f32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap_or([0; 4]));
        out.push([x, y, z]);
        offset += 12;
    }
    out
}

/// N364: COLOR_0 (VEC3/VEC4 f32, or normalized u8/u16) per geometry vertex.
fn decode_colors(accessor: &Value, bytes: Option<&[u8]>, max: usize) -> Vec<[f32; 4]> {
    let ty = accessor.get("type").and_then(Value::as_str);
    let components = match ty {
        Some("VEC3") => 3,
        Some("VEC4") => 4,
        _ => return Vec::new(),
    };
    let component = accessor.get("componentType").and_then(Value::as_u64);
    let normalized = accessor
        .get("normalized")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match component {
        Some(5126) => {
            let stride = components * 4;
            let mut offset = 0usize;
            while offset + stride <= bytes.len() && out.len() < max {
                let read = |k: usize| {
                    f32::from_le_bytes(
                        bytes[offset + k * 4..offset + k * 4 + 4]
                            .try_into()
                            .unwrap_or([0; 4]),
                    )
                };
                out.push([
                    read(0),
                    read(1),
                    read(2),
                    if components == 4 { read(3) } else { 1.0 },
                ]);
                offset += stride;
            }
        }
        Some(5121) if normalized => {
            let mut offset = 0usize;
            while offset + components <= bytes.len() && out.len() < max {
                let read = |k: usize| f32::from(bytes[offset + k]) / 255.0;
                out.push([
                    read(0),
                    read(1),
                    read(2),
                    if components == 4 { read(3) } else { 1.0 },
                ]);
                offset += components;
            }
        }
        Some(5123) if normalized => {
            let stride = components * 2;
            let mut offset = 0usize;
            while offset + stride <= bytes.len() && out.len() < max {
                let read = |k: usize| {
                    f32::from(u16::from_le_bytes([
                        bytes[offset + k * 2],
                        bytes[offset + k * 2 + 1],
                    ])) / 65535.0
                };
                out.push([
                    read(0),
                    read(1),
                    read(2),
                    if components == 4 { read(3) } else { 1.0 },
                ]);
                offset += stride;
            }
        }
        _ => {}
    }
    out
}

/// N326: NORMAL (VEC3 f32, or normalized i8/i16) per full-geometry vertex.
fn decode_normals(accessor: &Value, bytes: Option<&[u8]>, max: usize) -> Vec<[f32; 3]> {
    if accessor.get("type").and_then(Value::as_str) != Some("VEC3") {
        return Vec::new();
    }
    let component = accessor.get("componentType").and_then(Value::as_u64);
    let normalized = accessor
        .get("normalized")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match component {
        Some(5126) => {
            let mut offset = 0usize;
            while offset + 12 <= bytes.len() && out.len() < max {
                let x = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]));
                let y =
                    f32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap_or([0; 4]));
                let z =
                    f32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap_or([0; 4]));
                out.push([x, y, z]);
                offset += 12;
            }
        }
        Some(5120) if normalized => {
            let mut offset = 0usize;
            while offset + 3 <= bytes.len() && out.len() < max {
                let v = |i: usize| (bytes[offset + i] as i8) as f32 / 127.0;
                out.push([v(0).max(-1.0), v(1).max(-1.0), v(2).max(-1.0)]);
                offset += 3;
            }
        }
        Some(5122) if normalized => {
            let mut offset = 0usize;
            while offset + 6 <= bytes.len() && out.len() < max {
                let v = |i: usize| {
                    i16::from_le_bytes([bytes[offset + i], bytes[offset + i + 1]]) as f32 / 32767.0
                };
                out.push([v(0).max(-1.0), v(2).max(-1.0), v(4).max(-1.0)]);
                offset += 6;
            }
        }
        _ => {}
    }
    out
}

/// N163: triangle-list indices into the full geometry (u16 or u32).
fn decode_geometry_indices(accessor: &Value, bytes: Option<&[u8]>, max: usize) -> Vec<u32> {
    let component = accessor.get("componentType").and_then(Value::as_u64);
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match component {
        Some(5123) => {
            let mut offset = 0usize;
            while offset + 2 <= bytes.len() && out.len() < max {
                out.push(u32::from(u16::from_le_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                ])));
                offset += 2;
            }
        }
        Some(5125) => {
            let mut offset = 0usize;
            while offset + 4 <= bytes.len() && out.len() < max {
                out.push(u32::from_le_bytes(
                    bytes[offset..offset + 4].try_into().unwrap_or([0; 4]),
                ));
                offset += 4;
            }
        }
        _ => {}
    }
    out
}

/// N163: a full-mesh UV list up to `max` elements.
fn decode_uv_mesh(accessor: &Value, bytes: Option<&[u8]>, max: usize) -> Vec<[f32; 2]> {
    if accessor.get("type").and_then(Value::as_str) != Some("VEC2") {
        return Vec::new();
    }
    let component = accessor.get("componentType").and_then(Value::as_u64);
    let normalized = accessor
        .get("normalized")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match component {
        Some(5126) => {
            let mut offset = 0usize;
            while offset + 8 <= bytes.len() && out.len() < max {
                let u = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]));
                let v =
                    f32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap_or([0; 4]));
                out.push([u, v]);
                offset += 8;
            }
        }
        Some(5121) if normalized => {
            let mut offset = 0usize;
            while offset + 2 <= bytes.len() && out.len() < max {
                out.push([
                    f32::from(bytes[offset]) / 255.0,
                    f32::from(bytes[offset + 1]) / 255.0,
                ]);
                offset += 2;
            }
        }
        Some(5123) if normalized => {
            let mut offset = 0usize;
            while offset + 4 <= bytes.len() && out.len() < max {
                let u = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
                let v = u16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]]);
                out.push([f32::from(u) / 65535.0, f32::from(v) / 65535.0]);
                offset += 4;
            }
        }
        _ => {}
    }
    out
}

/// N163: a full-mesh JOINTS_0 list up to `max` elements.
fn decode_joints_mesh(accessor: &Value, bytes: Option<&[u8]>, max: usize) -> Vec<[u16; 4]> {
    if accessor.get("type").and_then(Value::as_str) != Some("VEC4") {
        return Vec::new();
    }
    let component = accessor.get("componentType").and_then(Value::as_u64);
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match component {
        Some(5121) => {
            let mut offset = 0usize;
            while offset + 4 <= bytes.len() && out.len() < max {
                out.push([
                    u16::from(bytes[offset]),
                    u16::from(bytes[offset + 1]),
                    u16::from(bytes[offset + 2]),
                    u16::from(bytes[offset + 3]),
                ]);
                offset += 4;
            }
        }
        Some(5123) => {
            let mut offset = 0usize;
            while offset + 8 <= bytes.len() && out.len() < max {
                let mut joints = [0u16; 4];
                for (k, joint) in joints.iter_mut().enumerate() {
                    *joint = u16::from_le_bytes([bytes[offset + k * 2], bytes[offset + k * 2 + 1]]);
                }
                out.push(joints);
                offset += 8;
            }
        }
        _ => {}
    }
    out
}

/// N163: a full-mesh WEIGHTS_0 list up to `max` elements.
fn decode_weights_mesh(accessor: &Value, bytes: Option<&[u8]>, max: usize) -> Vec<[f32; 4]> {
    if accessor.get("type").and_then(Value::as_str) != Some("VEC4") {
        return Vec::new();
    }
    let component = accessor.get("componentType").and_then(Value::as_u64);
    let normalized = accessor
        .get("normalized")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match component {
        Some(5126) => {
            let mut offset = 0usize;
            while offset + 16 <= bytes.len() && out.len() < max {
                let mut weights = [0f32; 4];
                for (k, weight) in weights.iter_mut().enumerate() {
                    *weight = f32::from_le_bytes(
                        bytes[offset + k * 4..offset + k * 4 + 4]
                            .try_into()
                            .unwrap_or([0; 4]),
                    );
                }
                out.push(weights);
                offset += 16;
            }
        }
        Some(5121) if normalized => {
            let mut offset = 0usize;
            while offset + 4 <= bytes.len() && out.len() < max {
                out.push([
                    f32::from(bytes[offset]) / 255.0,
                    f32::from(bytes[offset + 1]) / 255.0,
                    f32::from(bytes[offset + 2]) / 255.0,
                    f32::from(bytes[offset + 3]) / 255.0,
                ]);
                offset += 4;
            }
        }
        Some(5123) if normalized => {
            let mut offset = 0usize;
            while offset + 8 <= bytes.len() && out.len() < max {
                let mut weights = [0f32; 4];
                for (k, weight) in weights.iter_mut().enumerate() {
                    *weight = f32::from(u16::from_le_bytes([
                        bytes[offset + k * 2],
                        bytes[offset + k * 2 + 1],
                    ])) / 65535.0;
                }
                out.push(weights);
                offset += 8;
            }
        }
        _ => {}
    }
    out
}

/// Map a local-space point from roughly [-1, 1] into a 32×32 pixel cell.
fn project_xy(point: &[f32; 3]) -> (i16, i16) {
    let px = ((point[0] + 1.0) * 16.0).round().clamp(0.0, 31.0) as i16;
    let py = ((1.0 - point[1]) * 16.0).round().clamp(0.0, 31.0) as i16;
    (px, py)
}

/// N109: JOINTS_0 (VEC4 u8/u16) per preview vertex.
fn decode_joints(accessor: &Value, bytes: Option<&[u8]>) -> Vec<[u16; 4]> {
    if accessor.get("type").and_then(Value::as_str) != Some("VEC4") {
        return Vec::new();
    }
    let component = accessor.get("componentType").and_then(Value::as_u64);
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match component {
        Some(5121) => {
            let mut offset = 0usize;
            while offset + 4 <= bytes.len() && out.len() < 64 {
                out.push([
                    u16::from(bytes[offset]),
                    u16::from(bytes[offset + 1]),
                    u16::from(bytes[offset + 2]),
                    u16::from(bytes[offset + 3]),
                ]);
                offset += 4;
            }
        }
        Some(5123) => {
            let mut offset = 0usize;
            while offset + 8 <= bytes.len() && out.len() < 64 {
                let mut joints = [0u16; 4];
                for (k, joint) in joints.iter_mut().enumerate() {
                    *joint = u16::from_le_bytes([bytes[offset + k * 2], bytes[offset + k * 2 + 1]]);
                }
                out.push(joints);
                offset += 8;
            }
        }
        _ => {}
    }
    out
}

/// N109: WEIGHTS_0 (VEC4 f32, or normalized u8/u16) per preview vertex.
fn decode_weights(accessor: &Value, bytes: Option<&[u8]>) -> Vec<[f32; 4]> {
    if accessor.get("type").and_then(Value::as_str) != Some("VEC4") {
        return Vec::new();
    }
    let component = accessor.get("componentType").and_then(Value::as_u64);
    let normalized = accessor
        .get("normalized")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match component {
        Some(5126) => {
            let mut offset = 0usize;
            while offset + 16 <= bytes.len() && out.len() < 64 {
                let mut weights = [0f32; 4];
                for (k, weight) in weights.iter_mut().enumerate() {
                    *weight = f32::from_le_bytes(
                        bytes[offset + k * 4..offset + k * 4 + 4]
                            .try_into()
                            .unwrap_or([0; 4]),
                    );
                }
                out.push(weights);
                offset += 16;
            }
        }
        Some(5121) if normalized => {
            let mut offset = 0usize;
            while offset + 4 <= bytes.len() && out.len() < 64 {
                out.push([
                    f32::from(bytes[offset]) / 255.0,
                    f32::from(bytes[offset + 1]) / 255.0,
                    f32::from(bytes[offset + 2]) / 255.0,
                    f32::from(bytes[offset + 3]) / 255.0,
                ]);
                offset += 4;
            }
        }
        Some(5123) if normalized => {
            let mut offset = 0usize;
            while offset + 8 <= bytes.len() && out.len() < 64 {
                let mut weights = [0f32; 4];
                for (k, weight) in weights.iter_mut().enumerate() {
                    *weight = f32::from(u16::from_le_bytes([
                        bytes[offset + k * 2],
                        bytes[offset + k * 2 + 1],
                    ])) / 65535.0;
                }
                out.push(weights);
                offset += 8;
            }
        }
        _ => {}
    }
    out
}

/// N113: TEXCOORD_0 (VEC2 f32, or normalized u8/u16) per preview vertex.
/// glTF UVs use the top-left origin; the baker maps `v` to image rows directly.
fn decode_uv(accessor: &Value, bytes: Option<&[u8]>) -> Vec<[f32; 2]> {
    if accessor.get("type").and_then(Value::as_str) != Some("VEC2") {
        return Vec::new();
    }
    let component = accessor.get("componentType").and_then(Value::as_u64);
    let normalized = accessor
        .get("normalized")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match component {
        Some(5126) => {
            let mut offset = 0usize;
            while offset + 8 <= bytes.len() && out.len() < 64 {
                let u = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]));
                let v =
                    f32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap_or([0; 4]));
                out.push([u, v]);
                offset += 8;
            }
        }
        Some(5121) if normalized => {
            let mut offset = 0usize;
            while offset + 2 <= bytes.len() && out.len() < 64 {
                out.push([
                    f32::from(bytes[offset]) / 255.0,
                    f32::from(bytes[offset + 1]) / 255.0,
                ]);
                offset += 2;
            }
        }
        Some(5123) if normalized => {
            let mut offset = 0usize;
            while offset + 4 <= bytes.len() && out.len() < 64 {
                let u = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
                let v = u16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]]);
                out.push([f32::from(u) / 65535.0, f32::from(v) / 65535.0]);
                offset += 4;
            }
        }
        _ => {}
    }
    out
}

/// N109/N110: decode an f32 accessor into a flat list, `components` per element,
/// capped at `max_count` elements. Non-f32 accessors return empty.
fn decode_float_accessor(
    accessor: &Value,
    bytes: Option<&[u8]>,
    components: usize,
    max_count: usize,
) -> Vec<f32> {
    if accessor.get("componentType").and_then(Value::as_u64) != Some(5126) {
        return Vec::new();
    }
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let stride = components * 4;
    if stride == 0 {
        return Vec::new();
    }
    let cap = max_count.max(1);
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + stride <= bytes.len() && out.len() / components < cap {
        for k in 0..components {
            out.push(f32::from_le_bytes(
                bytes[offset + k * 4..offset + k * 4 + 4]
                    .try_into()
                    .unwrap_or([0; 4]),
            ));
        }
        offset += stride;
    }
    out
}

fn read_f32_3(value: Option<&Value>, default: [f32; 3]) -> [f32; 3] {
    let Some(list) = value.and_then(Value::as_array) else {
        return default;
    };
    let mut out = default;
    for (slot, item) in out.iter_mut().zip(list.iter()) {
        if let Some(v) = item.as_f64() {
            *slot = v as f32;
        }
    }
    out
}

fn read_f32_4(value: Option<&Value>, default: [f32; 4]) -> [f32; 4] {
    let Some(list) = value.and_then(Value::as_array) else {
        return default;
    };
    let mut out = default;
    for (slot, item) in out.iter_mut().zip(list.iter()) {
        if let Some(v) = item.as_f64() {
            *slot = v as f32;
        }
    }
    out
}

/// N109: decompose a column-major glTF `matrix` into translation, quaternion and
/// scale. Negative scale is not handled (it would need a mirror decision).
fn decompose_matrix(m: &[Value]) -> ([f32; 3], [f32; 4], [f32; 3]) {
    let f = |i: usize| m.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
    let translation = [f(12), f(13), f(14)];
    let col0 = [f(0), f(1), f(2)];
    let col1 = [f(4), f(5), f(6)];
    let col2 = [f(8), f(9), f(10)];
    let sx = norm3(col0).max(f32::EPSILON);
    let sy = norm3(col1).max(f32::EPSILON);
    let sz = norm3(col2).max(f32::EPSILON);
    let scale = [sx, sy, sz];
    let r00 = col0[0] / sx;
    let r10 = col0[1] / sx;
    let r20 = col0[2] / sx;
    let r01 = col1[0] / sy;
    let r11 = col1[1] / sy;
    let r21 = col1[2] / sy;
    let r02 = col2[0] / sz;
    let r12 = col2[1] / sz;
    let r22 = col2[2] / sz;
    let trace = r00 + r11 + r22;
    let quat = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [(r21 - r12) / s, (r02 - r20) / s, (r10 - r01) / s, 0.25 * s]
    } else if r00 > r11 && r00 > r22 {
        let s = (1.0 + r00 - r11 - r22).sqrt() * 2.0;
        [0.25 * s, (r01 + r10) / s, (r02 + r20) / s, (r21 - r12) / s]
    } else if r11 > r22 {
        let s = (1.0 + r11 - r00 - r22).sqrt() * 2.0;
        [(r01 + r10) / s, 0.25 * s, (r12 + r21) / s, (r02 - r20) / s]
    } else {
        let s = (1.0 + r22 - r00 - r11).sqrt() * 2.0;
        [(r02 + r20) / s, (r12 + r21) / s, 0.25 * s, (r10 - r01) / s]
    };
    (translation, quat, scale)
}

fn norm3(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn view_has_valid_data(view: &BufferView, buffers: &[Buffer]) -> bool {
    buffers
        .get(view.buffer as usize)
        .and_then(|b| b.data.as_ref())
        .is_some()
}

/// Validate an accessor against its bufferView and return its element count.
/// Accessors without a bufferView (sparse or zero-filled) keep their declared
/// count. Unknown component/type pairs and overruns are rejected.
fn validate_accessor(accessor: &Value, views: &[BufferView]) -> Result<u64, ModelError> {
    let count = accessor.get("count").and_then(Value::as_u64).unwrap_or(0);
    let Some(view_index) = accessor.get("bufferView").and_then(Value::as_u64) else {
        return Ok(count);
    };
    if views.is_empty() {
        return Ok(count);
    }
    let view = views.get(view_index as usize).ok_or_else(|| {
        ModelError::Unsupported(format!("accessor bufferView {view_index} missing"))
    })?;
    let element = accessor_element_size(
        accessor
            .get("componentType")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        accessor.get("type").and_then(Value::as_str).unwrap_or(""),
    )
    .ok_or_else(|| ModelError::Unsupported("accessor component/type".into()))?;
    let accessor_offset = accessor
        .get("byteOffset")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let needed = accessor_offset.saturating_add(count.saturating_mul(element));
    if needed > view.byte_length {
        return Err(ModelError::Unsupported(format!(
            "accessor {needed} bytes over bufferView {} bytes",
            view.byte_length
        )));
    }
    Ok(count)
}

fn accessor_element_size(component_type: u64, kind: &str) -> Option<u64> {
    let component = match component_type {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        _ => return None,
    };
    let components = match kind {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        "MAT2" => 4,
        "MAT3" => 9,
        "MAT4" => 16,
        _ => return None,
    };
    Some(component * components)
}

/// Decode a `data:[<mime>][;base64],<payload>` URI. Only base64 is supported;
/// a plain-text data URI is rejected (glTF buffers are always base64).
fn decode_data_uri(uri: &str) -> Result<Vec<u8>, ModelError> {
    let (_, payload) = uri
        .split_once(',')
        .ok_or_else(|| ModelError::Unsupported("data uri without payload".into()))?;
    if !uri[..uri.len() - payload.len()].contains("base64") {
        return Err(ModelError::Unsupported("data uri is not base64".into()));
    }
    base64_decode(payload.as_bytes())
}

fn base64_decode(input: &[u8]) -> Result<Vec<u8>, ModelError> {
    fn value(byte: u8) -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some(u32::from(byte - b'A')),
            b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for &byte in input {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            break;
        }
        let digit =
            value(byte).ok_or_else(|| ModelError::Unsupported("bad base64 digit".into()))?;
        accumulator = (accumulator << 6) | digit;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    Ok(out)
}

/// Probe PNG or JPEG header bytes for real dimensions without decoding pixels.
fn probe_image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        // PNG: signature(8) IHDR length(4) "IHDR"(4) width(4) height(4)
        if bytes.get(12..16)? != b"IHDR" {
            return None;
        }
        return Some((read_be_u32(bytes, 16)?, read_be_u32(bytes, 20)?));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return probe_jpeg_size(bytes);
    }
    None
}

fn probe_jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut offset = 2usize;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xFF {
            return None;
        }
        let marker = bytes[offset + 1];
        // Standalone markers carry no length payload.
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            offset += 2;
            continue;
        }
        let length = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
        if length < 2 {
            return None;
        }
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            let height = read_be_u16(bytes, offset + 5)?;
            let width = read_be_u16(bytes, offset + 7)?;
            return Some((u32::from(width), u32::from(height)));
        }
        offset = offset.checked_add(2)?.checked_add(length)?;
    }
    None
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

/// Read glTF `skins` and any VRM humanoid extension into IR skeletons. Joint
/// indices are validated against the node array; an out-of-range joint is
/// rejected rather than silently imported. N109 also carries the joint node
/// indices and the inverse bind matrices so a bake can deform the mesh.
fn read_skeletons(
    doc: &Value,
    node_count: usize,
    accessors: &[Value],
    views: &[BufferView],
    buffers: &[Buffer],
) -> Result<Vec<Skeleton>, ModelError> {
    let view_bytes = |index: u64| -> Option<&[u8]> {
        let view = views.get(index as usize)?;
        if !view_has_valid_data(view, buffers) {
            return None;
        }
        let buffer = buffers.get(view.buffer as usize)?;
        let data = buffer.data.as_ref()?;
        let start = usize::try_from(view.byte_offset).ok()?;
        let len = usize::try_from(view.byte_length).ok()?;
        data.get(start..start.checked_add(len)?)
    };
    let mut out = Vec::new();
    let empty = Vec::new();
    if let Some(skins) = doc.get("skins").and_then(Value::as_array) {
        for (i, skin) in skins.iter().enumerate() {
            let joints = skin
                .get("joints")
                .and_then(Value::as_array)
                .unwrap_or(&empty);
            let mut joint_nodes = Vec::with_capacity(joints.len());
            for joint in joints {
                let index = joint.as_u64().unwrap_or(u64::MAX);
                if index as usize >= node_count {
                    return Err(ModelError::Unsupported(format!(
                        "skin {i} joint {index} out of range ({node_count} nodes)"
                    )));
                }
                joint_nodes.push(index as usize);
            }
            // N109: inverse bind matrices (MAT4 f32, column-major).
            let mut inverse_bind = Vec::new();
            if let Some(index) = skin.get("inverseBindMatrices").and_then(Value::as_u64) {
                if let Some(accessor) = accessors.get(index as usize) {
                    let bytes = accessor
                        .get("bufferView")
                        .and_then(Value::as_u64)
                        .and_then(&view_bytes);
                    let floats =
                        decode_float_accessor(accessor, bytes, 16, joint_nodes.len().max(1));
                    for chunk in floats.chunks_exact(16) {
                        let mut matrix = [0.0f32; 16];
                        matrix.copy_from_slice(chunk);
                        inverse_bind.push(matrix);
                    }
                }
            }
            out.push(Skeleton {
                name: json_name(skin, &format!("skin{i}")),
                bones: joints.len().min(u32::MAX as usize) as u32,
                joint_nodes,
                inverse_bind,
            });
        }
    }
    // VRM 0.x stores humanBones as an array; VRM 1.0 as an object map.
    for key in ["VRM", "VRMC_vrm"] {
        let human_bones = doc
            .get("extensions")
            .and_then(|e| e.get(key))
            .and_then(|v| v.get("humanoid"))
            .and_then(|h| h.get("humanBones"));
        if let Some(bones) = human_bones {
            let count = bones
                .as_array()
                .map(|a| a.len())
                .or_else(|| bones.as_object().map(|o| o.len()))
                .unwrap_or(0);
            if count > 0 {
                out.push(Skeleton {
                    name: "vrm-humanoid".into(),
                    bones: count.min(u32::MAX as usize) as u32,
                    joint_nodes: Vec::new(),
                    inverse_bind: Vec::new(),
                });
                break;
            }
        }
    }
    Ok(out)
}

/// N46: read VRM metadata and expressions (VRM 0.x and VRMC_vrm 1.0) into a
/// small meta map plus one animation per expression.
fn read_vrm(doc: &Value) -> (BTreeMap<String, String>, Vec<Animation>) {
    let mut meta = BTreeMap::new();
    let mut animations = Vec::new();
    let extensions = doc.get("extensions");
    if let Some(vrm) = extensions.and_then(|e| e.get("VRM")) {
        if let Some(meta_json) = vrm.get("meta") {
            for (key, field) in [("title", "vrm.title"), ("author", "vrm.author")] {
                if let Some(value) = meta_json.get(key).and_then(Value::as_str) {
                    meta.insert(field.to_string(), value.to_string());
                }
            }
        }
        if let Some(groups) = vrm
            .get("blendShapeMaster")
            .and_then(|b| b.get("blendShapeGroups"))
            .and_then(Value::as_array)
        {
            meta.insert("vrm.expressions".into(), groups.len().to_string());
            for group in groups {
                animations.push(Animation {
                    name: format!("vrm-expression:{}", json_name(group, "expression")),
                    channels: 1,
                    ..Animation::default()
                });
            }
        }
    }
    if let Some(vrm) = extensions.and_then(|e| e.get("VRMC_vrm")) {
        if let Some(meta_json) = vrm.get("meta") {
            if let Some(name) = meta_json.get("name").and_then(Value::as_str) {
                meta.insert("vrm.title".into(), name.to_string());
            }
            if let Some(author) = meta_json
                .get("authors")
                .and_then(Value::as_array)
                .and_then(|list| list.first())
                .and_then(Value::as_str)
            {
                meta.insert("vrm.author".into(), author.to_string());
            }
        }
        if let Some(expressions) = vrm.get("expressions").and_then(Value::as_object) {
            meta.insert("vrm.expressions".into(), expressions.len().to_string());
            for name in expressions.keys() {
                animations.push(Animation {
                    name: format!("vrm-expression:{name}"),
                    channels: 1,
                    ..Animation::default()
                });
            }
        }
    }
    (meta, animations)
}

pub(crate) fn json_name(value: &Value, fallback: &str) -> String {
    value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
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

    /// A minimal valid PNG header carrying the given dimensions.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out
    }

    fn jpeg_header(width: u16, height: u16) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        out.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        out.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out
    }

    /// Standard base64 for test `data:` URIs.
    fn base64(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0];
            let b1 = *chunk.get(1).unwrap_or(&0);
            let b2 = *chunk.get(2).unwrap_or(&0);
            out.push(TABLE[(b0 >> 2) as usize] as char);
            out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            out.push(if chunk.len() > 1 {
                TABLE[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                TABLE[(b2 & 0x3F) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    /// Build a GLB with a JSON chunk and a BIN chunk.
    fn glb(json: &[u8], bin: &[u8]) -> Vec<u8> {
        fn pad4(data: &mut Vec<u8>, byte: u8) {
            while (data.len() & 3) != 0 {
                data.push(byte);
            }
        }
        let mut json_chunk = json.to_vec();
        pad4(&mut json_chunk, 0x20);
        let mut bin_chunk = bin.to_vec();
        pad4(&mut bin_chunk, 0x00);
        let total = 12 + 8 + json_chunk.len() + 8 + bin_chunk.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json_chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&GLB_CHUNK_JSON.to_le_bytes());
        out.extend_from_slice(&json_chunk);
        out.extend_from_slice(&(bin_chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&GLB_CHUNK_BIN.to_le_bytes());
        out.extend_from_slice(&bin_chunk);
        out
    }

    #[test]
    fn embedded_positions_become_preview_pixels() {
        let mut bin = Vec::new();
        for (x, y) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0)] {
            bin.extend_from_slice(&x.to_le_bytes());
            bin.extend_from_slice(&y.to_le_bytes());
            bin.extend_from_slice(&0.0f32.to_le_bytes());
        }
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": bin.len()}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "nodes": [{"mesh": 0}]
        });
        let model = GltfImporter
            .import(
                &glb(&serde_json::to_vec(&doc).unwrap(), &bin),
                &ImportLimits::default(),
            )
            .unwrap();
        assert_eq!(model.meshes[0].preview_xy.len(), 3);
        assert_eq!(model.meshes[0].preview_xy[0], (0, 31));
    }

    #[test]
    fn texture_coordinates_import_as_preview_uv() {
        let mut bin = Vec::new();
        for (x, y) in [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0)] {
            for v in [x, y, 0.0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        for (u, v) in [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0)] {
            bin.extend_from_slice(&u.to_le_bytes());
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 36},
                {"buffer": 0, "byteOffset": 36, "byteLength": 24}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC2"}
            ],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "TEXCOORD_0": 1}}]}],
            "nodes": [{"mesh": 0}]
        });
        let model = GltfImporter
            .import(
                &glb(&serde_json::to_vec(&doc).unwrap(), &bin),
                &ImportLimits::default(),
            )
            .unwrap();
        assert_eq!(model.meshes[0].preview_uv.len(), 3);
        assert_eq!(model.meshes[0].preview_uv[1], [1.0, 0.0]);
        assert_eq!(model.meshes[0].preview_uv[2], [0.0, 1.0]);
    }

    #[test]
    fn morph_targets_and_weights_are_imported() {
        let mut bin = Vec::new();
        for (x, y) in [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0)] {
            for v in [x, y, 0.0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        // Morph delta POSITION: a +1 shift on X for each vertex.
        for _ in 0..3 {
            for v in [1.0f32, 0.0, 0.0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        for t in [0.0f32, 1.0] {
            bin.extend_from_slice(&t.to_le_bytes());
        }
        for w in [0.0f32, 1.0] {
            bin.extend_from_slice(&w.to_le_bytes());
        }
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 36},
                {"buffer": 0, "byteOffset": 36, "byteLength": 36},
                {"buffer": 0, "byteOffset": 72, "byteLength": 8},
                {"buffer": 0, "byteOffset": 80, "byteLength": 8}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 2, "componentType": 5126, "count": 2, "type": "SCALAR"},
                {"bufferView": 3, "componentType": 5126, "count": 2, "type": "SCALAR"}
            ],
            "meshes": [{
                "name": "face",
                "weights": [0.5],
                "primitives": [{
                    "attributes": {"POSITION": 0},
                    "targets": [{"POSITION": 1}]
                }]
            }],
            "nodes": [{"mesh": 0}],
            "animations": [{
                "name": "smile",
                "channels": [{"sampler": 0, "target": {"node": 0, "path": "weights"}}],
                "samplers": [{"input": 2, "output": 3, "interpolation": "LINEAR"}]
            }]
        });
        let model = GltfImporter
            .import(
                &glb(&serde_json::to_vec(&doc).unwrap(), &bin),
                &ImportLimits::default(),
            )
            .unwrap();
        let mesh = &model.meshes[0];
        assert_eq!(mesh.morph_targets.len(), 1);
        assert_eq!(mesh.morph_targets[0][0], [1.0, 0.0, 0.0]);
        assert_eq!(mesh.morph_weights, vec![0.5]);
        // N224: the same deltas are imported into the full geometry.
        let geometry = mesh.geometry.as_ref().expect("geometry");
        assert_eq!(geometry.morph_targets.len(), 1);
        assert_eq!(geometry.morph_targets[0][0], [1.0, 0.0, 0.0]);

        let track = &model.animations[0].tracks[0];
        assert_eq!(track.path, "weights");
        assert_eq!(track.node, 0);
        assert_eq!(track.times, vec![0.0, 1.0]);
        assert_eq!(track.values, vec![0.0, 1.0]);
    }

    #[test]
    fn full_mesh_geometry_is_imported() {
        let mut bin = Vec::new();
        // 4 POSITION (VEC3 f32).
        for (x, y) in [(0.0f32, 0.0f32), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
            for v in [x, y, 0.0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        // 6 u16 indices (two triangles).
        for i in [0u16, 1, 2, 0, 2, 3] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        // 4 UV (VEC2 f32).
        for (u, v) in [(0.0f32, 0.0f32), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
            bin.extend_from_slice(&u.to_le_bytes());
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 48},
                {"buffer": 0, "byteOffset": 48, "byteLength": 12},
                {"buffer": 0, "byteOffset": 60, "byteLength": 32}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5123, "count": 6, "type": "SCALAR"},
                {"bufferView": 2, "componentType": 5126, "count": 4, "type": "VEC2"}
            ],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "TEXCOORD_0": 2}, "indices": 1}]}],
            "nodes": [{"mesh": 0}]
        });
        let model = GltfImporter
            .import(
                &glb(&serde_json::to_vec(&doc).unwrap(), &bin),
                &ImportLimits::default(),
            )
            .unwrap();
        let geometry = model.meshes[0].geometry.as_ref().expect("geometry");
        assert_eq!(geometry.positions.len(), 4);
        assert_eq!(geometry.indices, vec![0, 1, 2, 0, 2, 3]);
        assert_eq!(geometry.uv.len(), 4);
        assert_eq!(geometry.uv[2], [1.0, 1.0]);
    }

    #[test]
    fn normals_import_in_f32_and_normalized_forms() {
        let mut bin = Vec::new();
        for (x, y) in [(-0.5f32, -0.5f32), (0.5, -0.5), (0.5, 0.5), (-0.5, 0.5)] {
            for v in [x, y, 0.0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        for i in [0u16, 1, 2, 0, 2, 3] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        for _ in 0..4 {
            for v in [0.0f32, 0.0, 1.0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 48},
                {"buffer": 0, "byteOffset": 48, "byteLength": 12},
                {"buffer": 0, "byteOffset": 60, "byteLength": 48}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5123, "count": 6, "type": "SCALAR"},
                {"bufferView": 2, "componentType": 5126, "count": 4, "type": "VEC3"}
            ],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 2}, "indices": 1}]}],
            "nodes": [{"mesh": 0}]
        });
        let model = GltfImporter
            .import(
                &glb(&serde_json::to_vec(&doc).unwrap(), &bin),
                &ImportLimits::default(),
            )
            .unwrap();
        let geometry = model.meshes[0].geometry.as_ref().expect("geometry");
        assert_eq!(geometry.normals.len(), 4);
        assert_eq!(geometry.normals[0], [0.0, 0.0, 1.0]);

        // Normalized i8 normals: (0, 0, 127) -> unit +z.
        let mut bin = Vec::new();
        for (x, y) in [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0)] {
            for v in [x, y, 0.0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        for _ in 0..3 {
            bin.extend_from_slice(&[0u8, 0, 127]);
        }
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 36},
                {"buffer": 0, "byteOffset": 36, "byteLength": 9}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5120, "normalized": true, "count": 3, "type": "VEC3"}
            ],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1}}]}],
            "nodes": [{"mesh": 0}]
        });
        let model = GltfImporter
            .import(
                &glb(&serde_json::to_vec(&doc).unwrap(), &bin),
                &ImportLimits::default(),
            )
            .unwrap();
        let normals = &model.meshes[0].geometry.as_ref().unwrap().normals;
        assert_eq!(normals.len(), 3);
        assert!(
            (normals[2][2] - 1.0).abs() < 1e-6,
            "i8 normal normalized to +z"
        );
    }

    #[test]
    fn sampler_wrap_is_recorded_on_the_texture() {
        let doc = json!({
            "asset": {"version": "2.0"},
            "samplers": [{"wrapS": 33071, "wrapT": 33071}],
            "images": [{"mimeType": "image/png"}],
            "textures": [{"source": 0, "sampler": 0}],
            "materials": [{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}],
            "nodes": [{"mesh": 0}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "material": 0}]}],
            "accessors": [{"count": 3, "type": "VEC3"}]
        });
        let model = GltfImporter
            .import(&serde_json::to_vec(&doc).unwrap(), &ImportLimits::default())
            .unwrap();
        assert_eq!(model.textures[0].wrap.as_deref(), Some("clamp"));
        assert_eq!(model.material_texture_wrap(0), Some("clamp"));
    }

    #[test]
    fn material_base_colour_factor_imports() {
        let doc = json!({
            "asset": {"version": "2.0"},
            "nodes": [{"mesh": 0}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "material": 0}]}],
            "accessors": [{"count": 3, "type": "VEC3"}],
            "materials": [
                {"name": "red", "emissiveFactor": [1.0, 0.5, 0.0], "extensions": {"KHR_materials_unlit": {}, "KHR_materials_emissive_strength": {"emissiveStrength": 1.5}}, "pbrMetallicRoughness": {"baseColorFactor": [1.0, 0.0, 0.0, 1.0], "metallicFactor": 0.25, "roughnessFactor": 0.75, "baseColorTexture": {"index": 0, "extensions": {"KHR_texture_transform": {"offset": [0.25, 0.0], "rotation": 0.0, "scale": [2.0, 1.0]}}}}},
                {"name": "plain"}
            ]
        });
        let model = GltfImporter
            .import(&serde_json::to_vec(&doc).unwrap(), &ImportLimits::default())
            .unwrap();
        assert_eq!(model.materials[0].color, Some([255, 0, 0]));
        // N343: metallic/roughness factors are imported with spec defaults.
        assert_eq!(model.materials[0].metallic, Some(0.25));
        assert_eq!(model.materials[0].roughness, Some(0.75));
        assert_eq!(model.materials[0].roughness(), 0.75);
        assert_eq!(model.materials[1].roughness(), 1.0);
        // N363: emissive + unlit.
        assert_eq!(model.materials[0].emissive, Some([255, 128, 0]));
        assert!(model.materials[0].is_unlit());
        assert!(model.materials[0].emissive_luma() > 0.0);
        assert!(!model.materials[1].is_unlit());
        // N373: KHR_texture_transform on the base-colour texture.
        let uv = model.materials[0].uv_transform();
        assert_eq!(uv.offset, [0.25, 0.0]);
        assert_eq!(uv.scale, [2.0, 1.0]);
        assert_eq!(uv.rotation, 0.0);
        assert!(!uv.is_identity());
        assert!(model.materials[1].uv_transform().is_identity());
        // N374: KHR_materials_emissive_strength scales the emissive luminance.
        assert_eq!(model.materials[0].emissive_strength(), 1.5);
        assert!(model.materials[0].emissive_luma() > 0.8);
        assert_eq!(model.materials[1].emissive_strength(), 1.0);
        assert_eq!(model.materials[0].color_rgb565(), Some(0xF800));
        assert_eq!(model.materials[1].color, None);
        assert_eq!(model.materials[1].color_rgb565(), None);
        // N333: baseColorFactor alpha is imported.
        assert_eq!(model.materials[0].alpha, Some(1.0));
        assert_eq!(model.materials[1].alpha, None);
    }

    #[test]
    fn multiple_primitives_merge_into_one_geometry() {
        let mut bin = Vec::new();
        for (x, y) in [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0)] {
            for v in [x, y, 0.0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        for (x, y) in [(2.0f32, 0.0f32), (3.0, 0.0), (2.0, 1.0)] {
            for v in [x, y, 0.0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        for i in [0u16, 1, 2, 0, 1, 2] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 36},
                {"buffer": 0, "byteOffset": 36, "byteLength": 36},
                {"buffer": 0, "byteOffset": 72, "byteLength": 6},
                {"buffer": 0, "byteOffset": 78, "byteLength": 6}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 2, "componentType": 5123, "count": 3, "type": "SCALAR"},
                {"bufferView": 3, "componentType": 5123, "count": 3, "type": "SCALAR"}
            ],
            "meshes": [{"primitives": [
                {"attributes": {"POSITION": 0}, "indices": 2},
                {"attributes": {"POSITION": 1}, "indices": 3}
            ]}],
            "nodes": [{"mesh": 0}]
        });
        let model = GltfImporter
            .import(
                &glb(&serde_json::to_vec(&doc).unwrap(), &bin),
                &ImportLimits::default(),
            )
            .unwrap();
        let geometry = model.meshes[0].geometry.as_ref().expect("geometry");
        assert_eq!(geometry.positions.len(), 6);
        assert_eq!(geometry.positions[3], [2.0, 0.0, 0.0]);
        // The second primitive's indices are rebased by +3.
        assert_eq!(geometry.indices, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn material_alpha_mode_and_cutoff_import() {
        let doc = json!({
            "asset": {"version": "2.0"},
            "nodes": [{"mesh": 0}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "material": 0}]}],
            "accessors": [{"count": 3, "type": "VEC3"}],
            "materials": [
                {"name": "cut", "alphaMode": "MASK", "alphaCutoff": 0.25,
                 "pbrMetallicRoughness": {"baseColorFactor": [1.0, 1.0, 1.0, 1.0]}},
                {"name": "opaque"}
            ]
        });
        let model = GltfImporter
            .import(&serde_json::to_vec(&doc).unwrap(), &ImportLimits::default())
            .unwrap();
        assert!(model.materials[0].is_masked());
        assert_eq!(model.materials[0].alpha_mode.as_deref(), Some("MASK"));
        assert_eq!(model.materials[0].cutoff(), 0.25);
        assert!(!model.materials[1].is_masked());
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
    fn glb_container_imports_and_probes_png_dimensions() {
        let png = png_header(320, 200);
        let doc = json!({
            "asset": {"version": "2.0"},
            "nodes": [{"name": "root", "mesh": 0}],
            "meshes": [{"name": "m", "primitives": [{"attributes": {"POSITION": 0}}]}],
            "accessors": [{"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"}],
            "buffers": [{"byteLength": png.len() + 36}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": png.len()},
                {"buffer": 0, "byteOffset": png.len(), "byteLength": 36}
            ],
            "images": [{"name": "tex", "mimeType": "image/png", "bufferView": 0}],
            "textures": [{"source": 0}]
        });
        let json = serde_json::to_vec(&doc).unwrap();
        let mut bin = png.clone();
        bin.extend_from_slice(&[0u8; 36]);
        let bytes = glb(&json, &bin);

        let model = GltfImporter
            .import(&bytes, &ImportLimits::default())
            .unwrap();
        assert_eq!(
            (model.textures[0].width, model.textures[0].height),
            (320, 200),
            "dimensions come from the PNG header, not the (absent) declared size"
        );
        assert_eq!(model.meshes[0].vertices, 3);
    }

    #[test]
    fn base64_data_uri_buffer_probes_jpeg_dimensions() {
        let jpeg = jpeg_header(640, 480);
        let encoded = {
            // Small standard-encoder for the test data.
            const TABLE: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for chunk in jpeg.chunks(3) {
                let b = [
                    chunk[0],
                    *chunk.get(1).unwrap_or(&0),
                    *chunk.get(2).unwrap_or(&0),
                ];
                out.push(TABLE[(b[0] >> 2) as usize] as char);
                out.push(TABLE[(((b[0] & 0x03) << 4) | (b[1] >> 4)) as usize] as char);
                out.push(if chunk.len() > 1 {
                    TABLE[(((b[1] & 0x0F) << 2) | (b[2] >> 6)) as usize] as char
                } else {
                    '='
                });
                out.push(if chunk.len() > 2 {
                    TABLE[(b[2] & 0x3F) as usize] as char
                } else {
                    '='
                });
            }
            out
        };
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": jpeg.len(), "uri": format!("data:image/jpeg;base64,{encoded}")}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": jpeg.len()}],
            "images": [{"name": "tex", "mimeType": "image/jpeg", "bufferView": 0}],
            "textures": [{"source": 0}]
        });
        let bytes = serde_json::to_vec(&doc).unwrap();
        let model = GltfImporter
            .import(&bytes, &ImportLimits::default())
            .unwrap();
        assert_eq!(
            (model.textures[0].width, model.textures[0].height),
            (640, 480)
        );
        // N323: the embedded bytes are retained for the compiler.
        assert_eq!(model.embedded_texture(0), Some(jpeg.as_slice()));
        assert_eq!(model.embedded_texture(1), None);
    }

    #[test]
    fn image_bytes_over_budget_are_not_retained() {
        // A tiny max_texture_bytes keeps the declared metadata but not the bytes.
        let png = png_header(16, 16); // ~45 bytes
        let limits = ImportLimits {
            max_texture_bytes: 8,
            ..ImportLimits::default()
        };
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": png.len(), "uri": format!("data:image/png;base64,{}", base64(&png))}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": png.len()}],
            "images": [{"name": "tex", "mimeType": "image/png", "bufferView": 0}],
            "textures": [{"source": 0}]
        });
        // The declared size is over the budget, so the import is refused before
        // any retention (the metadata check fires first).
        let err = GltfImporter
            .import(&serde_json::to_vec(&doc).unwrap(), &limits)
            .unwrap_err();
        assert!(matches!(err, ModelError::Budget(_)));

        // With the bytes inside the budget, they are retained.
        let limits = ImportLimits {
            max_texture_bytes: 1024,
            ..ImportLimits::default()
        };
        let model = GltfImporter
            .import(&serde_json::to_vec(&doc).unwrap(), &limits)
            .unwrap();
        assert_eq!(model.embedded_texture(0), Some(png.as_slice()));
    }

    #[test]
    fn decode_bomb_is_rejected_before_allocation() {
        let limits = ImportLimits {
            max_input_bytes: 32,
            ..ImportLimits::default()
        };
        assert!(matches!(
            GltfImporter.import(&sample_gltf(4, 1024, 256), &limits),
            Err(ModelError::InputTooLarge(_, 32))
        ));
        let limits = ImportLimits {
            max_texture_bytes: 512,
            ..ImportLimits::default()
        };
        let err = GltfImporter
            .import(&sample_gltf(4, 4096, 256), &limits)
            .unwrap_err();
        assert!(matches!(err, ModelError::Budget(_)), "{err:?}");
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
    fn accessor_overrunning_its_bufferview_is_rejected() {
        // 3 VEC3 floats = 36 bytes, but the view only holds 8.
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": 8}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 8}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "nodes": [{"mesh": 0}]
        });
        let bytes = serde_json::to_vec(&doc).unwrap();
        let err = GltfImporter
            .import(&bytes, &ImportLimits::default())
            .unwrap_err();
        assert!(matches!(err, ModelError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn bufferview_overrunning_its_buffer_is_rejected() {
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": 16, "uri": "data:application/octet-stream;base64,AAAAAA=="}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 64}]
        });
        let bytes = serde_json::to_vec(&doc).unwrap();
        let err = GltfImporter
            .import(&bytes, &ImportLimits::default())
            .unwrap_err();
        assert!(matches!(err, ModelError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn skins_import_as_skeletons_and_bad_joints_are_rejected() {
        let doc = json!({
            "asset": {"version": "2.0"},
            "nodes": [{"name": "root"}, {"name": "hips"}, {"name": "spine"}],
            "skins": [{"name": "rig", "joints": [0, 1, 2]}]
        });
        let model = GltfImporter
            .import(&serde_json::to_vec(&doc).unwrap(), &ImportLimits::default())
            .unwrap();
        assert_eq!(model.skeletons.len(), 1);
        assert_eq!(model.skeletons[0].name, "rig");
        assert_eq!(model.skeletons[0].bones, 3);

        let bad = json!({
            "asset": {"version": "2.0"},
            "nodes": [{"name": "root"}],
            "skins": [{"joints": [0, 5]}]
        });
        assert!(matches!(
            GltfImporter.import(&serde_json::to_vec(&bad).unwrap(), &ImportLimits::default()),
            Err(ModelError::Unsupported(_))
        ));
    }

    #[test]
    fn vrm_humanoid_extension_adds_a_skeleton() {
        // VRM 0.x: humanBones is an array.
        let vrm0 = json!({
            "asset": {"version": "2.0"},
            "nodes": [{"name": "root"}],
            "extensions": {"VRM": {"humanoid": {"humanBones": [{"bone": "hips"}, {"bone": "head"}]}}}
        });
        let model = GltfImporter
            .import(
                &serde_json::to_vec(&vrm0).unwrap(),
                &ImportLimits::default(),
            )
            .unwrap();
        assert_eq!(model.skeletons[0].name, "vrm-humanoid");
        assert_eq!(model.skeletons[0].bones, 2);

        // VRM 1.0: humanBones is an object map.
        let vrm1 = json!({
            "asset": {"version": "2.0"},
            "nodes": [{"name": "root"}],
            "extensions": {"VRMC_vrm": {"humanoid": {"humanBones": {"hips": {}, "spine": {}, "head": {}}}}}
        });
        let model = GltfImporter
            .import(
                &serde_json::to_vec(&vrm1).unwrap(),
                &ImportLimits::default(),
            )
            .unwrap();
        assert_eq!(model.skeletons[0].bones, 3);
    }

    #[test]
    fn vrm_meta_and_expressions_are_imported() {
        let doc = json!({
            "asset": {"version": "2.0"},
            "nodes": [{"name": "root"}],
            "extensions": {
                "VRM": {
                    "meta": {"title": "Haru", "author": "Me"},
                    "blendShapeMaster": {"blendShapeGroups": [{"name": "Joy"}, {"name": "Angry"}]}
                }
            }
        });
        let model = GltfImporter
            .import(&serde_json::to_vec(&doc).unwrap(), &ImportLimits::default())
            .unwrap();
        assert_eq!(model.meta.get("vrm.title").unwrap(), "Haru");
        assert_eq!(model.meta.get("vrm.author").unwrap(), "Me");
        assert_eq!(model.meta.get("vrm.expressions").unwrap(), "2");
        assert!(model
            .animations
            .iter()
            .any(|a| a.name == "vrm-expression:Joy"));

        // VRM 1.0 object form.
        let doc = json!({
            "asset": {"version": "2.0"},
            "extensions": {"VRMC_vrm": {"meta": {"name": "Aria", "authors": ["Dev"]}, "expressions": {"happy": {}, "sad": {}}}}
        });
        let model = GltfImporter
            .import(&serde_json::to_vec(&doc).unwrap(), &ImportLimits::default())
            .unwrap();
        assert_eq!(model.meta.get("vrm.title").unwrap(), "Aria");
        assert_eq!(model.meta.get("vrm.author").unwrap(), "Dev");
        assert_eq!(model.meta.get("vrm.expressions").unwrap(), "2");
    }

    /// Build a GLB with a node transform, a skin, JOINTS_0/WEIGHTS_0 and an
    /// animation sampler.
    fn skinned_glb() -> Vec<u8> {
        let mut bin = Vec::new();
        // POSITION (3 × VEC3 f32) at 0.
        for (x, y, z) in [(0.0f32, 0.0f32, 0.0f32), (1.0, 0.0, 0.0), (0.0, 1.0, 0.0)] {
            for v in [x, y, z] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        let position_len = bin.len();
        // JOINTS_0 (3 × VEC4 u16).
        for joints in [[0u16, 0, 0, 0], [1, 0, 0, 0], [0, 0, 0, 0]] {
            for j in joints {
                bin.extend_from_slice(&j.to_le_bytes());
            }
        }
        let joints_len = bin.len() - position_len;
        // WEIGHTS_0 (3 × VEC4 f32).
        for weights in [
            [1.0f32, 0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
        ] {
            for w in weights {
                bin.extend_from_slice(&w.to_le_bytes());
            }
        }
        let weights_len = bin.len() - position_len - joints_len;
        // inverseBindMatrices (2 × MAT4 f32, identity).
        let identity = [
            1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        for _ in 0..2 {
            for v in identity {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        let bind_len = 128;
        // Animation input (2 × SCALAR f32) and output (2 × VEC4 f32).
        for t in [0.0f32, 1.0] {
            bin.extend_from_slice(&t.to_le_bytes());
        }
        for _ in 0..2 {
            for v in [0.0f32, 0.0, 0.0, 1.0] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        let bind_offset = position_len + joints_len + weights_len;
        let anim_input_offset = bind_offset + bind_len;
        let anim_output_offset = anim_input_offset + 8;
        let doc = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": position_len},
                {"buffer": 0, "byteOffset": position_len, "byteLength": joints_len},
                {"buffer": 0, "byteOffset": position_len + joints_len, "byteLength": weights_len},
                {"buffer": 0, "byteOffset": bind_offset, "byteLength": bind_len},
                {"buffer": 0, "byteOffset": anim_input_offset, "byteLength": 8},
                {"buffer": 0, "byteOffset": anim_output_offset, "byteLength": 32}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5123, "count": 3, "type": "VEC4"},
                {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC4"},
                {"bufferView": 3, "componentType": 5126, "count": 2, "type": "MAT4"},
                {"bufferView": 4, "componentType": 5126, "count": 2, "type": "SCALAR"},
                {"bufferView": 5, "componentType": 5126, "count": 2, "type": "VEC4"}
            ],
            "meshes": [{"name": "body", "primitives": [{"attributes": {"POSITION": 0, "JOINTS_0": 1, "WEIGHTS_0": 2}}]}],
            "nodes": [
                {"name": "root", "mesh": 0, "translation": [1.0, 2.0, 3.0], "children": [1]},
                {"name": "joint"}
            ],
            "skins": [{"name": "rig", "joints": [0, 1], "inverseBindMatrices": 3}],
            "animations": [{
                "name": "wave",
                "channels": [{"sampler": 0, "target": {"node": 1, "path": "rotation"}}],
                "samplers": [{"input": 4, "output": 5, "interpolation": "LINEAR"}]
            }]
        });
        glb(&serde_json::to_vec(&doc).unwrap(), &bin)
    }

    #[test]
    fn node_transforms_and_skin_weights_are_imported() {
        let model = GltfImporter
            .import(&skinned_glb(), &ImportLimits::default())
            .unwrap();
        assert_eq!(model.nodes[0].translation, [1.0, 2.0, 3.0]);
        assert_eq!(model.nodes[0].rotation, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(model.nodes[1].scale, [1.0, 1.0, 1.0]);

        let mesh = &model.meshes[0];
        assert_eq!(mesh.preview_positions.len(), 3);
        assert_eq!(mesh.preview_positions[1], [1.0, 0.0, 0.0]);
        assert_eq!(mesh.skin_joints.len(), 3);
        assert_eq!(mesh.skin_joints[1], [1, 0, 0, 0]);
        assert_eq!(mesh.skin_weights[0], [1.0, 0.0, 0.0, 0.0]);

        let skeleton = &model.skeletons[0];
        assert_eq!(skeleton.bones, 2);
        assert_eq!(skeleton.joint_nodes, vec![0, 1]);
        assert_eq!(skeleton.inverse_bind.len(), 2);
        assert_eq!(skeleton.inverse_bind[0][0], 1.0);
    }

    #[test]
    fn animation_channels_import_timed_tracks() {
        let model = GltfImporter
            .import(&skinned_glb(), &ImportLimits::default())
            .unwrap();
        let track = &model.animations[0].tracks[0];
        assert_eq!(model.animations[0].name, "wave");
        assert_eq!(track.node, 1);
        assert_eq!(track.path, "rotation");
        assert_eq!(track.interpolation, "LINEAR");
        assert_eq!(track.times, vec![0.0, 1.0]);
        assert_eq!(track.values, vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn malformed_glb_containers_are_rejected() {
        // Bad version.
        let mut bad = glb(b"{}", &[]);
        bad[4] = 1;
        assert!(matches!(
            GltfImporter.import(&bad, &ImportLimits::default()),
            Err(ModelError::Unsupported(_))
        ));
        // Declared length larger than the input.
        let mut truncated = glb(b"{}", &[]);
        truncated[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            GltfImporter.import(&truncated, &ImportLimits::default()),
            Err(ModelError::Unsupported(_))
        ));
    }
}
