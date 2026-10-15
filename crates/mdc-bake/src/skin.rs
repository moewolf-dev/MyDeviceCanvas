//! N111: skeletal skinning bake.
//!
//! The IR carries a node hierarchy, per-vertex joints/weights and per-joint
//! inverse bind matrices once a glTF skin is present. This module poses the
//! skeleton at a time sampled from an animation, computes each joint's skinning
//! matrix (`world * inverseBind`) and deforms the preview mesh with linear blend
//! skinning, so consecutive baked frames are the *same model posed differently*
//! rather than a cycled source image.
//!
//! It is a host-side approximation: only the preview vertices (capped by the
//! importer) are deformed, there is no texture lookup, and CUBICSPLINE is
//! sampled as LINEAR using the key value.
use crate::{raster_points, BakeError, BakedTexture};
use mdc_model::{Animation, Mesh, Model, Node, Skeleton};

/// A 4×4 matrix in glTF column-major order (`m[column * 4 + row]`).
pub type Mat4 = [f32; 16];

pub fn mat4_identity() -> Mat4 {
    [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

/// `a * b` for two column-major matrices.
pub fn mat4_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [0.0f32; 16];
    for column in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0f32;
            for k in 0..4 {
                sum += a[k * 4 + row] * b[column * 4 + k];
            }
            out[column * 4 + row] = sum;
        }
    }
    out
}

/// Compose `T * R * S` from a translation, a quaternion `[x, y, z, w]` and a scale.
pub fn mat4_from_trs(translation: [f32; 3], rotation: [f32; 4], scale: [f32; 3]) -> Mat4 {
    let [x, y, z, w] = rotation;
    let (xx, yy, zz) = (x * x, y * y, z * z);
    let (xy, xz, yz) = (x * y, x * z, y * z);
    let (wx, wy, wz) = (w * x, w * y, w * z);
    let r = [
        [1.0 - 2.0 * (yy + zz), 2.0 * (xy - wz), 2.0 * (xz + wy)],
        [2.0 * (xy + wz), 1.0 - 2.0 * (xx + zz), 2.0 * (yz - wx)],
        [2.0 * (xz - wy), 2.0 * (yz + wx), 1.0 - 2.0 * (xx + yy)],
    ];
    let [sx, sy, sz] = scale;
    [
        r[0][0] * sx,
        r[1][0] * sx,
        r[2][0] * sx,
        0.0,
        r[0][1] * sy,
        r[1][1] * sy,
        r[2][1] * sy,
        0.0,
        r[0][2] * sz,
        r[1][2] * sz,
        r[2][2] * sz,
        0.0,
        translation[0],
        translation[1],
        translation[2],
        1.0,
    ]
}

/// Transform a point by a column-major matrix (implicit `w = 1`, no projection).
pub fn transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

/// World matrices from each node's own local transform.
pub fn node_world_matrices(nodes: &[Node]) -> Vec<Mat4> {
    let locals: Vec<Mat4> = nodes
        .iter()
        .map(|n| mat4_from_trs(n.translation, n.rotation, n.scale))
        .collect();
    worlds_from_locals(nodes, &locals)
}

/// World matrices for an explicit set of local matrices. Roots are nodes no
/// other node lists as a child; a cycle cannot loop forever.
fn worlds_from_locals(nodes: &[Node], locals: &[Mat4]) -> Vec<Mat4> {
    let count = nodes.len();
    let mut parent: Vec<Option<usize>> = vec![None; count];
    for (i, node) in nodes.iter().enumerate() {
        for &child in &node.children {
            if child < count {
                parent[child] = Some(i);
            }
        }
    }
    let mut out = vec![mat4_identity(); count];
    let mut done = vec![false; count];
    for start in 0..count {
        let mut chain = Vec::new();
        let mut current = Some(start);
        while let Some(index) = current {
            if done[index] || chain.len() > count {
                break;
            }
            chain.push(index);
            current = parent[index];
        }
        for &index in chain.iter().rev() {
            let world = match parent[index] {
                Some(p) if done[p] => mat4_mul(&out[p], &locals[index]),
                _ => locals[index],
            };
            out[index] = world;
            done[index] = true;
        }
    }
    out
}

/// Local matrices after sampling `animation` at `time` (seconds). Nodes without
/// a track keep their file transform.
pub fn posed_local_matrices(nodes: &[Node], animation: &Animation, time: f32) -> Vec<Mat4> {
    let mut translation: Vec<[f32; 3]> = nodes.iter().map(|n| n.translation).collect();
    let mut rotation: Vec<[f32; 4]> = nodes.iter().map(|n| n.rotation).collect();
    let mut scale: Vec<[f32; 3]> = nodes.iter().map(|n| n.scale).collect();
    for track in &animation.tracks {
        if track.node >= nodes.len() {
            continue;
        }
        match track.path.as_str() {
            "translation" => {
                let v = sample_track(track, 3, time);
                translation[track.node] = [v[0], v[1], v[2]];
            }
            "rotation" => {
                let v = sample_track(track, 4, time);
                rotation[track.node] = [v[0], v[1], v[2], v[3]];
            }
            "scale" => {
                let v = sample_track(track, 3, time);
                scale[track.node] = [v[0], v[1], v[2]];
            }
            _ => {}
        }
    }
    (0..nodes.len())
        .map(|i| mat4_from_trs(translation[i], rotation[i], scale[i]))
        .collect()
}

/// World matrices with `animation` sampled at `time`.
pub fn pose_world_matrices(nodes: &[Node], animation: &Animation, time: f32) -> Vec<Mat4> {
    let locals = posed_local_matrices(nodes, animation, time);
    worlds_from_locals(nodes, &locals)
}

/// The animation's time span `(start, end)` from every track's first/last key.
pub fn animation_span(animation: &Animation) -> (f32, f32) {
    let mut start = f32::INFINITY;
    let mut end = f32::NEG_INFINITY;
    for track in &animation.tracks {
        if let (Some(first), Some(last)) = (track.times.first(), track.times.last()) {
            start = start.min(*first);
            end = end.max(*last);
        }
    }
    if start.is_finite() && end.is_finite() && end > start {
        (start, end)
    } else {
        (0.0, 0.0)
    }
}

/// Sample one transform channel at `time`. Returns up to four components.
fn sample_track(track: &mdc_model::AnimationTrack, components: usize, time: f32) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    if components == 4 {
        out[3] = 1.0;
    }
    let cubic = track.interpolation.eq_ignore_ascii_case("CUBICSPLINE");
    let stride = if cubic { components * 3 } else { components };
    if stride == 0 {
        return out;
    }
    let value_at = |key: usize| -> Vec<f32> {
        let base = key * stride + if cubic { components } else { 0 };
        let mut v = Vec::with_capacity(components);
        for k in 0..components {
            v.push(track.values.get(base + k).copied().unwrap_or(0.0));
        }
        v
    };
    let count = track.times.len();
    if count == 0 || track.values.len() < components {
        return out;
    }
    let last = count - 1;
    let pick_first = |out: &mut [f32; 4]| {
        let v = value_at(0);
        for (slot, value) in out.iter_mut().zip(v.iter()) {
            *slot = *value;
        }
    };
    if time <= track.times[0] {
        pick_first(&mut out);
        return out;
    }
    if time >= track.times[last] {
        let v = value_at(last);
        for (slot, value) in out.iter_mut().zip(v.iter()) {
            *slot = *value;
        }
        return out;
    }
    let mut lower = 0usize;
    for i in 0..last {
        if time >= track.times[i] && time < track.times[i + 1] {
            lower = i;
            break;
        }
    }
    let a = value_at(lower);
    if track.interpolation.eq_ignore_ascii_case("STEP") {
        for (slot, value) in out.iter_mut().zip(a.iter()) {
            *slot = *value;
        }
        return out;
    }
    let b = value_at(lower + 1);
    let t0 = track.times[lower];
    let t1 = track.times[lower + 1];
    let f = if t1 > t0 {
        ((time - t0) / (t1 - t0)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if components == 4 {
        let q = slerp([a[0], a[1], a[2], a[3]], [b[0], b[1], b[2], b[3]], f);
        out = q;
    } else {
        for k in 0..components {
            out[k] = a[k] + (b[k] - a[k]) * f;
        }
    }
    out
}

fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut b = b;
    let mut dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    if dot < 0.0 {
        dot = -dot;
        b = [-b[0], -b[1], -b[2], -b[3]];
    }
    if dot > 0.9995 {
        let mut out = [0.0f32; 4];
        for k in 0..4 {
            out[k] = a[k] + (b[k] - a[k]) * t;
        }
        return normalize4(out);
    }
    let theta = dot.clamp(-1.0, 1.0).acos();
    let sin_theta = theta.sin();
    let wa = ((1.0 - t) * theta).sin() / sin_theta;
    let wb = (t * theta).sin() / sin_theta;
    [
        a[0] * wa + b[0] * wb,
        a[1] * wa + b[1] * wb,
        a[2] * wa + b[2] * wb,
        a[3] * wa + b[3] * wb,
    ]
}

fn normalize4(q: [f32; 4]) -> [f32; 4] {
    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if len <= f32::EPSILON {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        [q[0] / len, q[1] / len, q[2] / len, q[3] / len]
    }
}

/// Linear blend skinning of `mesh.preview_positions` by `worlds`. Vertices
/// without weights (or with out-of-range joints) keep their bind position.
pub fn skin_preview(mesh: &Mesh, skeleton: &Skeleton, worlds: &[Mat4]) -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(mesh.preview_positions.len());
    for (i, position) in mesh.preview_positions.iter().enumerate() {
        let joints = mesh.skin_joints.get(i);
        let weights = mesh.skin_weights.get(i);
        let mut acc = [0.0f32; 3];
        let mut total = 0.0f32;
        if let (Some(joints), Some(weights)) = (joints, weights) {
            for k in 0..4 {
                let weight = weights[k];
                if weight == 0.0 {
                    continue;
                }
                let joint = joints[k] as usize;
                let Some(&node) = skeleton.joint_nodes.get(joint) else {
                    continue;
                };
                let Some(world) = worlds.get(node) else {
                    continue;
                };
                let bind = skeleton
                    .inverse_bind
                    .get(joint)
                    .copied()
                    .unwrap_or_else(mat4_identity);
                let skinned = transform_point(&mat4_mul(world, &bind), *position);
                acc[0] += skinned[0] * weight;
                acc[1] += skinned[1] * weight;
                acc[2] += skinned[2] * weight;
                total += weight;
            }
        }
        if total > 0.0 {
            out.push([acc[0] / total, acc[1] / total, acc[2] / total]);
        } else {
            out.push(*position);
        }
    }
    out
}

/// Project a local point into `[0, width-1] × [0, height-1]`.
fn project(point: [f32; 3], width: u32, height: u32) -> (i16, i16) {
    let px = ((point[0] + 1.0) * 0.5 * (width.saturating_sub(1)) as f32)
        .round()
        .clamp(0.0, width.saturating_sub(1) as f32) as i16;
    let py = ((1.0 - point[1]) * 0.5 * (height.saturating_sub(1)) as f32)
        .round()
        .clamp(0.0, height.saturating_sub(1) as f32) as i16;
    (px, py)
}

/// N111: bake `frames` poses of the first skinned mesh in `model`, sampled from
/// `animation_index` across its time span. At most 64 frames.
pub fn bake_skinned_mesh(
    model: &Model,
    animation_index: usize,
    frames: u32,
    sprite: (u32, u32),
    color: u16,
) -> Result<Vec<BakedTexture>, BakeError> {
    if frames == 0 || frames > 64 {
        return Err(BakeError::FrameCount(frames as usize, 64));
    }
    if sprite.0 == 0 || sprite.1 == 0 || sprite.0 > 1024 || sprite.1 > 1024 {
        return Err(BakeError::Budget(sprite.0, sprite.1));
    }
    let mesh = model
        .meshes
        .iter()
        .find(|m| !m.skin_joints.is_empty() && !m.preview_positions.is_empty())
        .ok_or_else(|| BakeError::Decode("model has no skinned preview mesh".into()))?;
    let skeleton = model
        .skeletons
        .iter()
        .find(|s| !s.joint_nodes.is_empty())
        .ok_or_else(|| BakeError::Decode("model has no skin with joints".into()))?;
    let animation = model
        .animations
        .get(animation_index)
        .ok_or_else(|| BakeError::Decode(format!("animation {animation_index} missing")))?;
    let (start, end) = animation_span(animation);
    let mut baked = Vec::with_capacity(frames as usize);
    for frame in 0..frames {
        let time = if frames > 1 {
            start + (end - start) * (frame as f32) / ((frames - 1) as f32)
        } else {
            start
        };
        let worlds = pose_world_matrices(&model.nodes, animation, time);
        let skinned = skin_preview(mesh, skeleton, &worlds);
        let points: Vec<(i16, i16)> = skinned
            .iter()
            .map(|p| project(*p, sprite.0, sprite.1))
            .collect();
        let rgb565 = raster_points(&points, &mesh.preview_indices, sprite.0, sprite.1, color)?;
        baked.push(BakedTexture {
            width: sprite.0,
            height: sprite.1,
            rgb565,
        });
    }
    Ok(baked)
}

/// N112: bake skinned frames straight into a runtime `BakedSprites` package so a
/// device can receive the poses over the `PACKAGE` path.
pub fn bake_skinned_to_package(
    model: &Model,
    animation_index: usize,
    frames: u32,
    sprite: (u32, u32),
    color: u16,
) -> Result<mdc_model::MdcPackage, BakeError> {
    let baked = bake_skinned_mesh(model, animation_index, frames, sprite, color)?;
    let columns = (frames as f64).sqrt().ceil().max(1.0) as u32;
    let rows = frames.div_ceil(columns);
    let plan = mdc_model::AtlasPlan {
        sprite_width: sprite.0,
        sprite_height: sprite.1,
        frames,
        columns,
        rows,
        total_bytes: u64::from(frames)
            .saturating_mul(u64::from(sprite.0))
            .saturating_mul(u64::from(sprite.1))
            .saturating_mul(2),
        motions: Vec::new(),
    };
    let atlas = crate::pack(&plan, &baked)?;
    let atlas = mdc_model::AtlasBlob {
        sprite_width: atlas.sprite_width,
        sprite_height: atlas.sprite_height,
        frames: atlas.frames,
        columns: atlas.columns,
        rows: atlas.rows,
        rgb565: atlas.rgb565,
        clips: vec![mdc_model::AtlasClip {
            name: "bake-mesh".into(),
            start: 0,
            frames,
            fps: 30,
            looping: true,
            fade_in_ms: 0,
            fade_out_ms: 0,
        }],
    };
    let package = mdc_model::DevicePackage {
        target: "esp32-plane".into(),
        mode: mdc_model::PackageMode::BakedSprites,
        nodes: Vec::new(),
        meshes: Vec::new(),
        textures: Vec::new(),
        materials: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: atlas.rgb565.len() as u64,
        atlas: Some(atlas),
    };
    mdc_model::MdcPackage::seal(package).map_err(|e| BakeError::Package(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdc_model::{AnimationTrack, Node};

    fn node(name: &str, translation: [f32; 3], children: Vec<usize>) -> Node {
        Node {
            name: name.into(),
            mesh: None,
            children,
            translation,
            ..Node::default()
        }
    }

    fn skinned_model() -> Model {
        let mesh = Mesh {
            name: "tri".into(),
            vertices: 3,
            indices: 0,
            material: None,
            preview_positions: vec![[0.0, 0.0, 0.0], [0.5, 0.0, 0.0], [0.0, 0.5, 0.0]],
            skin_joints: vec![[1, 0, 0, 0], [1, 0, 0, 0], [1, 0, 0, 0]],
            skin_weights: vec![
                [1.0, 0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0, 0.0],
            ],
            ..Mesh::default()
        };
        let skeleton = Skeleton {
            name: "rig".into(),
            bones: 2,
            joint_nodes: vec![0, 1],
            inverse_bind: vec![mat4_identity(), mat4_identity()],
        };
        // Quarter turn about Z: [0, 0, sin45, cos45].
        let quarter = std::f32::consts::FRAC_1_SQRT_2;
        let animation = Animation {
            name: "spin".into(),
            channels: 1,
            tracks: vec![AnimationTrack {
                node: 1,
                path: "rotation".into(),
                interpolation: "LINEAR".into(),
                times: vec![0.0, 1.0],
                values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, quarter, quarter],
            }],
        };
        Model {
            source: "test".into(),
            nodes: vec![
                node("root", [0.0, 0.0, 0.0], vec![1]),
                node("j1", [0.0, 0.0, 0.0], vec![]),
            ],
            meshes: vec![mesh],
            skeletons: vec![skeleton],
            animations: vec![animation],
            ..Model::default()
        }
    }

    #[test]
    fn trs_and_multiplication_compose_translations() {
        let parent = mat4_from_trs([1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0]);
        let child = mat4_from_trs([2.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0]);
        let world = mat4_mul(&parent, &child);
        let origin = transform_point(&world, [0.0, 0.0, 0.0]);
        assert!((origin[0] - 3.0).abs() < 1e-5);

        let nodes = vec![
            node("root", [1.0, 0.0, 0.0], vec![1]),
            node("c", [2.0, 0.0, 0.0], vec![]),
        ];
        let worlds = node_world_matrices(&nodes);
        assert!((worlds[1][12] - 3.0).abs() < 1e-5);
    }

    #[test]
    fn skinning_rotates_a_weighted_vertex() {
        let model = skinned_model();
        let worlds = pose_world_matrices(&model.nodes, &model.animations[0], 1.0);
        let skinned = skin_preview(&model.meshes[0], &model.skeletons[0], &worlds);
        // Vertex 1 was [0.5, 0, 0]; a 90° Z rotation sends it to [0, 0.5, 0].
        assert!(skinned[1][0].abs() < 1e-4, "{:?}", skinned[1]);
        assert!((skinned[1][1] - 0.5).abs() < 1e-4, "{:?}", skinned[1]);
    }

    #[test]
    fn animation_span_and_pose_differ_over_time() {
        let model = skinned_model();
        let (start, end) = animation_span(&model.animations[0]);
        assert_eq!((start, end), (0.0, 1.0));
        let bind = pose_world_matrices(&model.nodes, &model.animations[0], 0.0);
        let posed = pose_world_matrices(&model.nodes, &model.animations[0], 1.0);
        assert_ne!(bind[1], posed[1]);
    }

    #[test]
    fn skinned_bake_frames_are_not_copies() {
        let model = skinned_model();
        let frames = bake_skinned_mesh(&model, 0, 3, (16, 16), 0xF800).unwrap();
        assert_eq!(frames.len(), 3);
        assert_ne!(frames[0].rgb565, frames[1].rgb565);
        assert_ne!(frames[1].rgb565, frames[2].rgb565);
        assert!(frames[0].rgb565.iter().any(|b| *b != 0));
    }

    #[test]
    fn skinned_bake_to_package_round_trips_and_gates_on_sprite_capability() {
        let model = skinned_model();
        let package = bake_skinned_to_package(&model, 0, 4, (16, 16), 0xF800).unwrap();
        assert_eq!(package.manifest.mode, mdc_model::PackageMode::BakedSprites);
        let atlas = package.package.atlas.as_ref().expect("atlas present");
        assert_eq!(atlas.frames, 4);
        assert_eq!(atlas.clips[0].name, "bake-mesh");
        // Frame 0 and frame 3 are different poses.
        assert_ne!(
            &atlas.rgb565[..16 * 16 * 2],
            &atlas.rgb565[atlas.frame_offset(3).unwrap()..][..16 * 16 * 2]
        );
        let bytes = package.to_bytes().unwrap();
        let back = mdc_model::MdcPackage::from_bytes(&bytes).unwrap();
        assert_eq!(back, package);
        assert!(back
            .accepted_by(&mdc_model::RendererCapability::sprite_only())
            .is_ok());
    }

    #[test]
    fn skinned_bake_refuses_a_model_without_skin() {
        let model = Model {
            meshes: vec![Mesh {
                name: "plain".into(),
                vertices: 3,
                preview_positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                ..Mesh::default()
            }],
            ..Model::default()
        };
        assert!(matches!(
            bake_skinned_mesh(&model, 0, 3, (16, 16), 0xF800),
            Err(BakeError::Decode(_))
        ));
    }
}
