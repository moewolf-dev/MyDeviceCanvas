//! Real glTF 2.0 importer: accepts the JSON (`.gltf`) form and the binary (`.glb`)
//! container, validates buffer/bufferView/accessor bounds, and probes embedded
//! PNG/JPEG headers for real pixel dimensions. Image bytes are never decoded —
//! only the header is read — so a decode bomb is rejected before any decode
//! buffer is allocated.
use crate::ir::{
    Animation, ImportLimits, Layer, Material, Mesh, Model, ModelError, Node, Skeleton, Texture,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub struct GltfImporter;

/// JSON chunk magic in a GLB container ("JSON").
const GLB_CHUNK_JSON: u32 = 0x4E4F_534A;
/// BIN chunk magic in a GLB container ("BIN\0").
const GLB_CHUNK_BIN: u32 = 0x004E_4942;

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
        for (i, image) in images.iter().enumerate() {
            let declared_w = image.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
            let declared_h = image.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
            let view_index = image.get("bufferView").and_then(Value::as_u64);
            let bytes_len = view_index
                .and_then(|bv| buffer_views.get(bv as usize))
                .map(|v| v.byte_length)
                .unwrap_or(0);
            let probed = view_index.and_then(view_bytes).and_then(probe_image_size);
            let (width, height) = probed.unwrap_or((declared_w, declared_h));
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
            let mut preview_xy = Vec::new();
            let mut preview_indices = Vec::new();
            if let Some(primitives) = mesh.get("primitives").and_then(Value::as_array) {
                for primitive in primitives {
                    if let Some(position) = primitive
                        .get("attributes")
                        .and_then(|a| a.get("POSITION"))
                        .and_then(Value::as_u64)
                    {
                        vertices = vertices.saturating_add(accessor_count(position)?);
                        if preview_xy.is_empty() {
                            if let Some(accessor) = accessors.get(position as usize) {
                                let bytes = accessor
                                    .get("bufferView")
                                    .and_then(Value::as_u64)
                                    .and_then(&view_bytes);
                                preview_xy = decode_preview_xy(accessor, bytes);
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
                }
            }
            meshes.push(Mesh {
                name: json_name(mesh, &format!("mesh{i}")),
                vertices: vertices.min(u64::from(u32::MAX)) as u32,
                indices,
                material,
                preview_xy,
                preview_indices,
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
            nodes.push(Node {
                name: json_name(node, &format!("node{i}")),
                mesh: node.get("mesh").and_then(Value::as_u64).map(|v| v as usize),
                children,
            });
        }

        let animations: Vec<Animation> = doc
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

        let (meta, expressions) = read_vrm(&doc);
        let mut animations = animations;
        animations.extend(expressions);

        Ok(Model {
            source: "gltf".into(),
            nodes,
            meshes,
            textures,
            materials,
            skeletons: read_skeletons(&doc, nodes_json.len())?,
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
                let value = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]));
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

fn decode_preview_xy(accessor: &Value, bytes: Option<&[u8]>) -> Vec<(i16, i16)> {
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
    while offset + 12 <= bytes.len() && out.len() < 64 {
        let x = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]));
        let y = f32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap_or([0; 4]));
        let px = ((x + 1.0) * 16.0).round().clamp(0.0, 31.0) as i16;
        let py = ((1.0 - y) * 16.0).round().clamp(0.0, 31.0) as i16;
        out.push((px, py));
        offset += 12;
    }
    out
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
/// rejected rather than silently imported.
fn read_skeletons(doc: &Value, node_count: usize) -> Result<Vec<Skeleton>, ModelError> {
    let mut out = Vec::new();
    let empty = Vec::new();
    if let Some(skins) = doc.get("skins").and_then(Value::as_array) {
        for (i, skin) in skins.iter().enumerate() {
            let joints = skin
                .get("joints")
                .and_then(Value::as_array)
                .unwrap_or(&empty);
            for joint in joints {
                let index = joint.as_u64().unwrap_or(u64::MAX);
                if index as usize >= node_count {
                    return Err(ModelError::Unsupported(format!(
                        "skin {i} joint {index} out of range ({node_count} nodes)"
                    )));
                }
            }
            out.push(Skeleton {
                name: json_name(skin, &format!("skin{i}")),
                bones: joints.len().min(u32::MAX as usize) as u32,
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
            .import(&glb(&serde_json::to_vec(&doc).unwrap(), &bin), &ImportLimits::default())
            .unwrap();
        assert_eq!(model.meshes[0].preview_xy.len(), 3);
        assert_eq!(model.meshes[0].preview_xy[0], (0, 31));
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
