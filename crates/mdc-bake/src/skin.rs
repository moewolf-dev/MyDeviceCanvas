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

/// N365: transform a direction (normal) by the linear part of `m` (no translation).
pub fn transform_direction(m: &Mat4, v: [f32; 3]) -> [f32; 3] {
    let out = [
        m[0] * v[0] + m[4] * v[1] + m[8] * v[2],
        m[1] * v[0] + m[5] * v[1] + m[9] * v[2],
        m[2] * v[0] + m[6] * v[1] + m[10] * v[2],
    ];
    let len = (out[0] * out[0] + out[1] * out[1] + out[2] * out[2]).sqrt();
    if len > 1e-6 {
        [out[0] / len, out[1] / len, out[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}

/// N365: skin normals with the same joints as the positions (the linear part of
/// `world · inverseBind`; no inverse-transpose, so non-uniform scale is approximate).
pub fn skin_normals(
    joints: &[[u16; 4]],
    weights: &[[f32; 4]],
    normals: &[[f32; 3]],
    skeleton: &Skeleton,
    worlds: &[Mat4],
) -> Vec<[f32; 3]> {
    skin_normals_multi(joints, weights, &[], &[], normals, skeleton, worlds)
}

/// N384: skin normals with up to eight influences (see [`skin_arrays_multi`]).
#[allow(clippy::too_many_arguments)]
pub fn skin_normals_multi(
    joints: &[[u16; 4]],
    weights: &[[f32; 4]],
    joints2: &[[u16; 4]],
    weights2: &[[f32; 4]],
    normals: &[[f32; 3]],
    skeleton: &Skeleton,
    worlds: &[Mat4],
) -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(normals.len());
    for (i, normal) in normals.iter().enumerate() {
        let mut acc = [0.0f32; 3];
        let mut total = 0.0f32;
        for (set_joints, set_weights) in [(joints, weights), (joints2, weights2)] {
            let (Some(vertex_joints), Some(vertex_weights)) =
                (set_joints.get(i), set_weights.get(i))
            else {
                continue;
            };
            for k in 0..4 {
                let weight = vertex_weights[k];
                if weight == 0.0 {
                    continue;
                }
                let joint = vertex_joints[k] as usize;
                let (Some(&node), Some(bind)) = (
                    skeleton.joint_nodes.get(joint),
                    skeleton.inverse_bind.get(joint),
                ) else {
                    continue;
                };
                let Some(world) = worlds.get(node) else {
                    continue;
                };
                let skinned = transform_direction(&mat4_mul(world, bind), *normal);
                acc[0] += skinned[0] * weight;
                acc[1] += skinned[1] * weight;
                acc[2] += skinned[2] * weight;
                total += weight;
            }
        }
        if total > 0.0 {
            let len = (acc[0] * acc[0] + acc[1] * acc[1] + acc[2] * acc[2]).sqrt();
            out.push(if len > 1e-6 {
                [acc[0] / len, acc[1] / len, acc[2] / len]
            } else {
                *normal
            });
        } else {
            out.push(*normal);
        }
    }
    out
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

/// Sample a channel at `time` into `components` values. N253: `CUBICSPLINE` uses
/// the per-key in/out tangents (cubic Hermite); rotation-sized (4) channels are
/// normalized. `LINEAR`/`STEP` are as before.
pub fn sample_channel(track: &mdc_model::AnimationTrack, components: usize, time: f32) -> Vec<f32> {
    let mut out = vec![0.0f32; components];
    if components == 4 {
        out[3] = 1.0;
    }
    let cubic = track.interpolation.eq_ignore_ascii_case("CUBICSPLINE");
    let stride = if cubic { components * 3 } else { components };
    if stride == 0 {
        return out;
    }
    // CUBICSPLINE stores [in-tangent, value, out-tangent] per key.
    let values_at = |key: usize, offset: usize| -> Vec<f32> {
        let base = key * stride + offset;
        (0..components)
            .map(|k| track.values.get(base + k).copied().unwrap_or(0.0))
            .collect()
    };
    let value_offset = if cubic { components } else { 0 };
    let value_at = |key: usize| values_at(key, value_offset);
    let count = track.times.len();
    if count == 0 || track.values.len() < components {
        return out;
    }
    let last = count - 1;
    if time <= track.times[0] {
        return value_at(0);
    }
    if time >= track.times[last] {
        return value_at(last);
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
        return a;
    }
    let b = value_at(lower + 1);
    let t0 = track.times[lower];
    let t1 = track.times[lower + 1];
    let span = t1 - t0;
    let f = if span > 0.0 {
        ((time - t0) / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if cubic {
        // Hermite: out-tangent of key0 and in-tangent of key1, scaled by span.
        let out0 = values_at(lower, components * 2);
        let in1 = values_at(lower + 1, 0);
        let h00 = 2.0 * f * f * f - 3.0 * f * f + 1.0;
        let h10 = f * f * f - 2.0 * f * f + f;
        let h01 = -2.0 * f * f * f + 3.0 * f * f;
        let h11 = f * f * f - f * f;
        let result: Vec<f32> = (0..components)
            .map(|k| {
                h00 * a[k]
                    + h10 * span * out0.get(k).copied().unwrap_or(0.0)
                    + h01 * b[k]
                    + h11 * span * in1.get(k).copied().unwrap_or(0.0)
            })
            .collect();
        if components == 4 {
            return normalize4([result[0], result[1], result[2], result[3]]).to_vec();
        }
        return result;
    }
    if components == 4 {
        slerp([a[0], a[1], a[2], a[3]], [b[0], b[1], b[2], b[3]], f).to_vec()
    } else {
        (0..components).map(|k| a[k] + (b[k] - a[k]) * f).collect()
    }
}

/// N255: sample with looping — `time` is wrapped into `[start, end)` when
/// `looping`, otherwise clamped. `start`/`end` come from `animation_span`.
pub fn sample_channel_looped(
    track: &mdc_model::AnimationTrack,
    components: usize,
    time: f32,
    start: f32,
    end: f32,
    looping: bool,
) -> Vec<f32> {
    let time = if end > start && looping {
        let span = end - start;
        start + (time - start).rem_euclid(span)
    } else {
        time
    };
    sample_channel(track, components, time)
}

/// Sample one transform channel at `time`. Returns up to four components.
fn sample_track(track: &mdc_model::AnimationTrack, components: usize, time: f32) -> [f32; 4] {
    let v = sample_channel(track, components, time);
    let mut out = [0.0f32; 4];
    if components == 4 {
        out[3] = 1.0;
    }
    for (slot, value) in out.iter_mut().zip(v.iter()).take(4) {
        *slot = *value;
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

/// Add `Σ weight_i · delta_i` to every base position. `weights` beyond the
/// target count are ignored; missing weights default to 0.
pub fn apply_morphs(
    base: &[[f32; 3]],
    targets: &[Vec<[f32; 3]>],
    weights: &[f32],
) -> Vec<[f32; 3]> {
    let mut out = base.to_vec();
    for (target_index, target) in targets.iter().enumerate() {
        let weight = weights.get(target_index).copied().unwrap_or(0.0);
        if weight == 0.0 {
            continue;
        }
        for (position, delta) in out.iter_mut().zip(target.iter()) {
            position[0] += delta[0] * weight;
            position[1] += delta[1] * weight;
            position[2] += delta[2] * weight;
        }
    }
    out
}

/// N125: sample a morph-weight channel for `node` at `time`. The component count
/// is derived from `values.len() / times.len()`; a channel that is not a clean
/// multiple returns `None`.
pub fn morph_weights_at(animation: &Animation, node: usize, time: f32) -> Option<Vec<f32>> {
    let track = animation
        .tracks
        .iter()
        .find(|t| t.path == "weights" && t.node == node)?;
    if track.times.is_empty() {
        return None;
    }
    let components = track.values.len() / track.times.len();
    if components == 0 || track.values.len() % track.times.len() != 0 {
        return None;
    }
    Some(sample_channel(track, components, time))
}

/// Linear blend skinning of `positions` by `worlds`. Vertices without weights
/// (or with out-of-range joints) keep their input position.
pub fn skin_points(
    mesh: &Mesh,
    skeleton: &Skeleton,
    worlds: &[Mat4],
    positions: &[[f32; 3]],
) -> Vec<[f32; 3]> {
    skin_arrays(
        &mesh.skin_joints,
        &mesh.skin_weights,
        positions,
        skeleton,
        worlds,
    )
}

/// Linear blend skinning with explicit joint/weight arrays (used by the
/// full-geometry path).
pub fn skin_arrays(
    joints: &[[u16; 4]],
    weights: &[[f32; 4]],
    positions: &[[f32; 3]],
    skeleton: &Skeleton,
    worlds: &[Mat4],
) -> Vec<[f32; 3]> {
    skin_arrays_multi(joints, weights, &[], &[], positions, skeleton, worlds)
}

/// N384: skin positions with up to eight influences (JOINTS_0/WEIGHTS_0 plus
/// JOINTS_1/WEIGHTS_1). The weights of both sets are averaged together.
#[allow(clippy::too_many_arguments)]
pub fn skin_arrays_multi(
    joints: &[[u16; 4]],
    weights: &[[f32; 4]],
    joints2: &[[u16; 4]],
    weights2: &[[f32; 4]],
    positions: &[[f32; 3]],
    skeleton: &Skeleton,
    worlds: &[Mat4],
) -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(positions.len());
    for (i, position) in positions.iter().enumerate() {
        let mut acc = [0.0f32; 3];
        let mut total = 0.0f32;
        for (set_joints, set_weights) in [(joints, weights), (joints2, weights2)] {
            let (Some(vertex_joints), Some(vertex_weights)) =
                (set_joints.get(i), set_weights.get(i))
            else {
                continue;
            };
            for k in 0..4 {
                let weight = vertex_weights[k];
                if weight == 0.0 {
                    continue;
                }
                let joint = vertex_joints[k] as usize;
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

/// Linear blend skinning of `mesh.preview_positions` by `worlds`.
pub fn skin_preview(mesh: &Mesh, skeleton: &Skeleton, worlds: &[Mat4]) -> Vec<[f32; 3]> {
    skin_points(mesh, skeleton, worlds, &mesh.preview_positions)
}

/// Project a local point into `[0, width-1] × [0, height-1]`.
pub(crate) fn project(point: [f32; 3], width: u32, height: u32) -> (i16, i16) {
    let px = ((point[0] + 1.0) * 0.5 * (width.saturating_sub(1)) as f32)
        .round()
        .clamp(0.0, width.saturating_sub(1) as f32) as i16;
    let py = ((1.0 - point[1]) * 0.5 * (height.saturating_sub(1)) as f32)
        .round()
        .clamp(0.0, height.saturating_sub(1) as f32) as i16;
    (px, py)
}

/// Projected preview points, one list per frame.
type PointFrames = Vec<Vec<(i16, i16)>>;

/// The resolved inputs for a skinning render: the first skinned mesh, the node
/// that owns it, its skeleton and one animation.
struct Skinned<'a> {
    mesh: &'a Mesh,
    mesh_node: Option<usize>,
    skeleton: &'a Skeleton,
    animation: &'a Animation,
}

fn resolve_skinned(model: &Model, animation_index: usize) -> Result<Skinned<'_>, BakeError> {
    let (mesh_index, mesh) = model
        .meshes
        .iter()
        .enumerate()
        .find(|(_, m)| {
            !m.skin_joints.is_empty() && (!m.preview_positions.is_empty() || m.geometry.is_some())
        })
        .ok_or_else(|| BakeError::Decode("model has no skinned preview mesh".into()))?;
    let mesh_node = model.nodes.iter().position(|n| n.mesh == Some(mesh_index));
    let skeleton = model
        .skeletons
        .iter()
        .find(|s| !s.joint_nodes.is_empty())
        .ok_or_else(|| BakeError::Decode("model has no skin with joints".into()))?;
    let animation = model
        .animations
        .get(animation_index)
        .ok_or_else(|| BakeError::Decode(format!("animation {animation_index} missing")))?;
    Ok(Skinned {
        mesh,
        mesh_node,
        skeleton,
        animation,
    })
}

/// N225: morph a geometry's positions by the sampled weights (base + Σ w·delta).
/// Uses the node's `weights` channel when present, else the mesh's default
/// weights; a geometry with no targets returns its base positions.
fn morphed_positions(
    geometry: &mdc_model::MeshGeometry,
    mesh: &Mesh,
    animation: Option<&Animation>,
    node: Option<usize>,
    time: f32,
) -> Vec<[f32; 3]> {
    if geometry.morph_targets.is_empty() {
        return geometry.positions.clone();
    }
    let weights = node
        .and_then(|node| animation.and_then(|a| morph_weights_at(a, node, time)))
        .unwrap_or_else(|| mesh.morph_weights.clone());
    apply_morphs(&geometry.positions, &geometry.morph_targets, &weights)
}

/// Skinned + morphed preview points at `time`, projected into the sprite.
fn skinned_points_at(
    model: &Model,
    skinned: &Skinned<'_>,
    time: f32,
    sprite: (u32, u32),
) -> Vec<(i16, i16)> {
    // N126: morph targets deform before skinning.
    let morphed;
    let positions: &[[f32; 3]] = if skinned.mesh.morph_targets.is_empty() {
        &skinned.mesh.preview_positions
    } else {
        let weights = skinned
            .mesh_node
            .and_then(|node| morph_weights_at(skinned.animation, node, time))
            .unwrap_or_else(|| skinned.mesh.morph_weights.clone());
        morphed = apply_morphs(
            &skinned.mesh.preview_positions,
            &skinned.mesh.morph_targets,
            &weights,
        );
        &morphed
    };
    let worlds = pose_world_matrices(&model.nodes, skinned.animation, time);
    skin_points(skinned.mesh, skinned.skeleton, &worlds, positions)
        .iter()
        .map(|p| project(*p, sprite.0, sprite.1))
        .collect()
}

/// Resolve the skinned mesh, its skeleton and an animation, then project each
/// preview vertex once per frame. Shared by the solid and textured bakers.
fn skinned_point_frames(
    model: &Model,
    animation_index: usize,
    frames: u32,
    sprite: (u32, u32),
) -> Result<(PointFrames, &Mesh), BakeError> {
    if frames == 0 || frames > 64 {
        return Err(BakeError::FrameCount(frames as usize, 64));
    }
    if sprite.0 == 0 || sprite.1 == 0 || sprite.0 > 1024 || sprite.1 > 1024 {
        return Err(BakeError::Budget(sprite.0, sprite.1));
    }
    let skinned = resolve_skinned(model, animation_index)?;
    let (start, end) = animation_span(skinned.animation);
    let mut out = Vec::with_capacity(frames as usize);
    for frame in 0..frames {
        let time = if frames > 1 {
            start + (end - start) * (frame as f32) / ((frames - 1) as f32)
        } else {
            start
        };
        out.push(skinned_points_at(model, &skinned, time, sprite));
    }
    Ok((out, skinned.mesh))
}

/// N133: render one posed frame at `time` (seconds), sampling `texture` through
/// the mesh UVs when present. Used by the offline render CLIs.
pub fn render_frame_at(
    model: &Model,
    animation_index: usize,
    time: f32,
    sprite: (u32, u32),
    texture: Option<&crate::TextureMap>,
    color: u16,
) -> Result<BakedTexture, BakeError> {
    if sprite.0 == 0 || sprite.1 == 0 || sprite.0 > 1024 || sprite.1 > 1024 {
        return Err(BakeError::Budget(sprite.0, sprite.1));
    }
    let skinned = resolve_skinned(model, animation_index)?;
    let points = skinned_points_at(model, &skinned, time, sprite);
    let tint = material_color(model, skinned.mesh);
    let mask = material_masked(model, skinned.mesh);
    let fill = tint.unwrap_or(color);
    let uvs: Option<&[[f32; 2]]> = if skinned.mesh.preview_uv.len()
        >= skinned.mesh.preview_positions.len()
        && !skinned.mesh.preview_uv.is_empty()
    {
        Some(&skinned.mesh.preview_uv)
    } else {
        None
    };
    let rgb565 = crate::texture::raster_textured(
        &points,
        uvs,
        &skinned.mesh.preview_indices,
        sprite.0,
        sprite.1,
        texture,
        tint,
        mask,
        fill,
    )?;
    Ok(BakedTexture {
        width: sprite.0,
        height: sprite.1,
        rgb565,
    })
}

/// N166: the XY bounding box of a point cloud (for `fit_to_box`).
fn bbox_xy(points: &[[f32; 3]]) -> Option<([f32; 2], [f32; 2])> {
    let first = points.first()?;
    let mut min = [first[0], first[1]];
    let mut max = [first[0], first[1]];
    for p in points {
        min[0] = min[0].min(p[0]);
        min[1] = min[1].min(p[1]);
        max[0] = max[0].max(p[0]);
        max[1] = max[1].max(p[1]);
    }
    Some((min, max))
}

/// N166: aspect-fit `points` into a `width × height` box (Y flipped so model-up
/// is screen-up), centered. Shared by every frame so the animation does not jitter.
fn fit_to_box(
    points: &[[f32; 3]],
    min: [f32; 2],
    max: [f32; 2],
    width: u32,
    height: u32,
) -> Vec<(i16, i16)> {
    let w = width.saturating_sub(1) as f32;
    let h = height.saturating_sub(1) as f32;
    let extent_x = (max[0] - min[0]).max(1e-4);
    let extent_y = (max[1] - min[1]).max(1e-4);
    let scale = (w / extent_x).min(h / extent_y);
    let offset_x = (w - extent_x * scale) / 2.0;
    let offset_y = (h - extent_y * scale) / 2.0;
    points
        .iter()
        .map(|p| {
            let px = offset_x + (p[0] - min[0]) * scale;
            let py = offset_y + (max[1] - p[1]) * scale;
            (
                px.round().clamp(0.0, w) as i16,
                py.round().clamp(0.0, h) as i16,
            )
        })
        .collect()
}

/// N165/N166: the geometry used for a render, or `None` when the mesh only has
/// a preview. The geometry's own skin arrays are used when present.
fn resolve_geometry(mesh: &Mesh) -> Option<&mdc_model::MeshGeometry> {
    mesh.geometry.as_ref().filter(|g| !g.positions.is_empty())
}

/// N175: the mesh's material base colour as RGB565, when the model sets one.
fn material_of<'a>(model: &'a Model, mesh: &Mesh) -> Option<&'a mdc_model::Material> {
    mesh.material.and_then(|index| model.materials.get(index))
}

/// N175: the mesh's material base colour as RGB565, when the model sets one.
fn material_color(model: &Model, mesh: &Mesh) -> Option<u16> {
    material_of(model, mesh).and_then(|material| material.color_rgb565())
}

/// N287: whether the mesh's material cuts out transparent texels.
fn material_masked(model: &Model, mesh: &Mesh) -> bool {
    material_of(model, mesh).is_some_and(|material| material.is_masked())
}

/// N333: the source alpha for a mesh's material (255 when opaque).
fn material_blend(model: &Model, mesh: &Mesh) -> u8 {
    match material_of(model, mesh) {
        Some(material) if material.is_blended() => material.alpha_u8(),
        _ => 255,
    }
}

/// N343: the ambient floor for a mesh, raised by the material roughness (a
/// rough surface shades flatter).
fn material_ambient(model: &Model, mesh: &Mesh) -> f32 {
    match material_of(model, mesh) {
        Some(material) if material.is_unlit() => 1.0,
        Some(material) => {
            (SHADE_AMBIENT + 0.35 * material.roughness() + 0.3 * material.emissive_luma()).min(0.95)
        }
        None => 0.9,
    }
}

/// N363: the same ambient rule for a package material.
fn package_material_ambient(package: &mdc_model::DevicePackage, mesh: &Mesh) -> f32 {
    match mesh.material.and_then(|mi| package.materials.get(mi)) {
        Some(material) if material.is_unlit() => 1.0,
        Some(material) => {
            (SHADE_AMBIENT + 0.35 * material.roughness() + 0.3 * material.emissive_luma()).min(0.95)
        }
        None => 0.9,
    }
}

/// N166: render one posed frame of the mesh's full geometry into `size` pixels.
/// Falls back to the preview render when the mesh carries no geometry. N175: the
/// mesh's material colour is preferred over `color` when no texture is sampled.
pub fn render_geometry_at(
    model: &Model,
    animation_index: usize,
    time: f32,
    size: (u32, u32),
    texture: Option<&crate::TextureMap>,
    color: u16,
) -> Result<BakedTexture, BakeError> {
    if size.0 == 0 || size.1 == 0 || size.0 > 1024 || size.1 > 1024 {
        return Err(BakeError::Budget(size.0, size.1));
    }
    let skinned = resolve_skinned(model, animation_index)?;
    let Some(geometry) = resolve_geometry(skinned.mesh) else {
        return render_frame_at(model, animation_index, time, size, texture, color);
    };
    let tint = material_color(model, skinned.mesh);
    let mask = material_masked(model, skinned.mesh);
    let fill = tint.unwrap_or(color);
    let worlds = pose_world_matrices(&model.nodes, skinned.animation, time);
    // N226: morph the geometry before skinning.
    let morphed = morphed_positions(
        geometry,
        skinned.mesh,
        Some(skinned.animation),
        skinned.mesh_node,
        time,
    );
    let deformed = skin_arrays(
        &geometry.skin_joints,
        &geometry.skin_weights,
        &morphed,
        skinned.skeleton,
        &worlds,
    );
    // Fit against the bind-pose box so the whole animation shares one frame.
    let Some((min, max)) = bbox_xy(&geometry.positions) else {
        return Err(BakeError::Decode("geometry has no positions".into()));
    };
    let points = fit_to_box(&deformed, min, max, size.0, size.1);
    let uvs: Option<&[[f32; 2]]> =
        if geometry.uv.len() >= geometry.positions.len() && !geometry.uv.is_empty() {
            Some(&geometry.uv)
        } else {
            None
        };
    let rgb565 = crate::texture::raster_indexed(
        &points,
        uvs,
        &geometry.indices,
        size.0,
        size.1,
        texture,
        tint,
        mask,
        fill,
        mdc_model::MAX_GEOMETRY_VERTICES,
    )?;
    Ok(BakedTexture {
        width: size.0,
        height: size.1,
        rgb565,
    })
}

/// N167: bake `frames` full-geometry poses into an atlas + runtime package.
pub fn bake_geometry_frames(
    model: &Model,
    animation_index: usize,
    frames: u32,
    size: (u32, u32),
    texture: Option<&crate::TextureMap>,
    color: u16,
) -> Result<Vec<BakedTexture>, BakeError> {
    if frames == 0 || frames > 64 {
        return Err(BakeError::FrameCount(frames as usize, 64));
    }
    let skinned = resolve_skinned(model, animation_index)?;
    let (start, end) = animation_span(skinned.animation);
    let mut out = Vec::with_capacity(frames as usize);
    for frame in 0..frames {
        let time = if frames > 1 {
            start + (end - start) * (frame as f32) / ((frames - 1) as f32)
        } else {
            start
        };
        out.push(render_geometry_at(
            model,
            animation_index,
            time,
            size,
            texture,
            color,
        )?);
    }
    Ok(out)
}

/// N167: bake full-geometry poses straight into a `BakedSprites` package.
pub fn bake_geometry_to_package(
    model: &Model,
    animation_index: usize,
    frames: u32,
    size: (u32, u32),
    texture: Option<&crate::TextureMap>,
    color: u16,
) -> Result<mdc_model::MdcPackage, BakeError> {
    let baked = bake_geometry_frames(model, animation_index, frames, size, texture, color)?;
    frames_to_package(baked, frames, size)
}

/// N184: world matrices for a scene render. Falls back to the bind pose when the
/// model has no (or a missing) animation.
fn scene_worlds(model: &Model, animation_index: usize, time: f32) -> Vec<Mat4> {
    match model.animations.get(animation_index) {
        Some(animation) => pose_world_matrices(&model.nodes, animation, time),
        None => node_world_matrices(&model.nodes),
    }
}

/// N184: the bind-pose XY box of every mesh node, shared by all frames so a
/// scene animation does not jitter.
fn scene_bind_bbox(model: &Model) -> Option<([f32; 2], [f32; 2])> {
    let worlds = node_world_matrices(&model.nodes);
    let mut points = Vec::new();
    for (index, node) in model.nodes.iter().enumerate() {
        let Some(mesh) = node.mesh.and_then(|mi| model.meshes.get(mi)) else {
            continue;
        };
        let Some(geometry) = resolve_geometry(mesh) else {
            continue;
        };
        for position in &geometry.positions {
            points.push(transform_point(&worlds[index], *position));
        }
    }
    bbox_xy(&points)
}

/// N184: render every mesh node of `model` at `time` into one frame. A mesh's
/// geometry is skinned when it carries weights, otherwise placed by its node's
/// world transform; each mesh uses its material colour or `fallback`.
/// N325: one mesh ready to draw, with the mean world-space z used to order
/// occluding meshes (painter's algorithm: far first).
struct SceneDrawable<'a> {
    depth: f32,
    points: Vec<(i16, i16)>,
    uvs: Option<&'a [[f32; 2]]>,
    indices: &'a [u32],
    tint: Option<u16>,
    mask: bool,
    fill: u16,
    /// N327: per-triangle directional shading factor.
    shade: Vec<f32>,
    /// N333: source alpha (255 = opaque).
    blend: u8,
    /// N353: the mesh's texture (per material when the package carries several).
    texture: Option<&'a crate::TextureMap>,
    /// N364: per-vertex colours, parallel to the points.
    colors: Option<&'a [[f32; 4]]>,
    /// N403: the secondary maps' UV set (None = same as `uvs`).
    uvs_secondary: Option<&'a [[f32; 2]]>,
    /// N373: the material's base-colour UV transform.
    uv_transform: mdc_model::UvTransform,
    /// N379: per-vertex normals for per-pixel normal mapping.
    normals: Option<Vec<[f32; 3]>>,
    /// N379: per-vertex tangents (xyzw).
    tangents: Option<Vec<[f32; 4]>>,
    /// N379: the tangent-space normal map.
    normal_map: Option<&'a crate::TextureMap>,
    /// N379: `normalTexture.scale`.
    normal_scale: f32,
    /// N379: ambient floor for the per-pixel Lambert term.
    normal_ambient: f32,
    /// N385: the key light direction for this drawable.
    light: [f32; 3],
    /// N387: Blinn-Phong specular strength (0 = no highlight).
    specular: f32,
    /// N387: Blinn-Phong shininess exponent.
    shininess: f32,
    /// N395: the normal texture's own UV transform.
    normal_uv_transform: mdc_model::UvTransform,
    /// N393: an additive emissive texture.
    emissive_map: Option<&'a crate::TextureMap>,
    /// N393: the emissive strength.
    emissive_strength: f32,
    /// N395: the emissive texture's own UV transform.
    emissive_uv_transform: mdc_model::UvTransform,
    /// N394: metallicRoughness texture (G = roughness).
    roughness_map: Option<&'a crate::TextureMap>,
    /// N394: the material's `roughnessFactor`.
    roughness_factor: f32,
    /// N395: the metallicRoughness texture's own UV transform.
    roughness_uv_transform: mdc_model::UvTransform,
}

/// N327: default key light (straight down -z, i.e. toward +z-facing surfaces) and
/// the ambient floor. A surface facing the light is unshaded (factor 1.0), so a
/// flat XY quad renders exactly as before.
const SHADE_LIGHT: [f32; 3] = [0.0, 0.0, 1.0];
const SHADE_AMBIENT: f32 = 0.45;

/// N327: per-triangle Lambert factor from 3D positions and (optional) normals.
/// Without per-vertex normals the geometric face normal is used.
fn triangle_shades(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    indices: &[u32],
    light: [f32; 3],
    ambient: f32,
) -> Vec<f32> {
    if indices.len() < 3 {
        return Vec::new();
    }
    triangle_shades_ambients(positions, normals, indices, light, ambient, None)
}

/// N378: like [`triangle_shades`] but an optional per-triangle occlusion factor
/// (0..=1) multiplies the result, darkening shadowed triangles regardless of the
/// light direction.
fn triangle_shades_ambients(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    indices: &[u32],
    light: [f32; 3],
    ambient: f32,
    factors: Option<&[f32]>,
) -> Vec<f32> {
    if indices.len() < 3 {
        return Vec::new();
    }
    let use_normals = !normals.is_empty() && normals.len() >= positions.len();
    indices
        .chunks_exact(3)
        .enumerate()
        .map(|(ti, tri)| {
            let normal = if use_normals {
                let mut acc = [0.0f32; 3];
                for &i in tri {
                    let v = normals.get(i as usize).copied().unwrap_or([0.0, 0.0, 1.0]);
                    acc[0] += v[0];
                    acc[1] += v[1];
                    acc[2] += v[2];
                }
                acc
            } else {
                geometric_normal(positions, tri)
            };
            let len =
                (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
            let base = if len <= 1e-6 {
                1.0
            } else {
                let dot =
                    (normal[0] * light[0] + normal[1] * light[1] + normal[2] * light[2]) / len;
                dot.clamp(ambient, 1.0)
            };
            let occlusion = factors.and_then(|f| f.get(ti)).copied().unwrap_or(1.0);
            (base * occlusion).clamp(0.0, 1.0)
        })
        .collect()
}

/// N378: per-triangle occlusion factor from the occlusion texture's red channel
/// at each triangle's centroid UV (`None` when the material has no usable map).
fn occlusion_factors(
    package: &mdc_model::DevicePackage,
    mesh: &Mesh,
    geometry: &mdc_model::MeshGeometry,
    textures: &[Option<crate::TextureMap>],
) -> Option<Vec<f32>> {
    let (index, strength) = mesh
        .material
        .and_then(|mi| package.materials.get(mi))
        .map(|material| material.occlusion())?;
    let index = index?;
    let map = textures.get(index).and_then(|texture| texture.as_ref())?;
    if geometry.indices.len() < 3 || geometry.uv.len() < geometry.positions.len() {
        return None;
    }
    let mut out = Vec::with_capacity(geometry.indices.len() / 3);
    for tri in geometry.indices.chunks_exact(3) {
        let mut u = 0.0f32;
        let mut v = 0.0f32;
        for &i in tri {
            let uv = geometry.uv.get(i as usize).copied().unwrap_or([0.0, 0.0]);
            u += uv[0] / 3.0;
            v += uv[1] / 3.0;
        }
        let sample = map.sample(u, v);
        let red = f32::from((sample >> 11) & 0x1f) / 31.0;
        let ao = 1.0 - strength * (1.0 - red);
        out.push(ao.clamp(0.2, 1.0));
    }
    Some(out)
}

/// Geometric (unnormalized) normal of one triangle.
fn geometric_normal(positions: &[[f32; 3]], tri: &[u32]) -> [f32; 3] {
    let p = |k: usize| {
        tri.get(k)
            .and_then(|&i| positions.get(i as usize))
            .copied()
            .unwrap_or([0.0, 0.0, 0.0])
    };
    let (a, b, c) = (p(0), p(1), p(2));
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ]
}

/// N325: draw meshes far-to-near so a nearer mesh overlays a farther one. Ties
/// keep the caller's order (stable sort); a z-buffer is not used.
fn draw_scene_sorted(
    pixels: &mut [u8],
    size: (u32, u32),
    mut drawables: Vec<SceneDrawable<'_>>,
) -> Result<(), BakeError> {
    drawables.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    for d in drawables {
        crate::texture::draw_indexed(
            pixels,
            &d.points,
            d.uvs,
            d.uvs_secondary,
            d.indices,
            size.0,
            size.1,
            d.texture,
            d.tint,
            d.mask,
            d.blend,
            d.fill,
            Some(&d.shade),
            d.colors,
            d.uv_transform,
            match (d.normals.as_deref(), d.tangents.as_deref(), d.normal_map) {
                (Some(normals), Some(tangents), Some(map)) => Some(crate::texture::NormalMap {
                    normals,
                    tangents,
                    map,
                    scale: d.normal_scale,
                    light: d.light,
                    ambient: d.normal_ambient,
                    specular: d.specular,
                    shininess: d.shininess,
                    uv_transform: d.normal_uv_transform,
                    roughness_map: d.roughness_map,
                    roughness_factor: d.roughness_factor,
                    roughness_uv_transform: d.roughness_uv_transform,
                }),
                _ => None,
            },
            d.emissive_map.map(|map| crate::texture::EmissiveMap {
                map,
                strength: d.emissive_strength,
                uv_transform: d.emissive_uv_transform,
            }),
            mdc_model::MAX_GEOMETRY_VERTICES,
        )?;
    }
    Ok(())
}

pub fn render_scene_at(
    model: &Model,
    animation_index: usize,
    time: f32,
    size: (u32, u32),
    texture: Option<&crate::TextureMap>,
    fallback: u16,
) -> Result<BakedTexture, BakeError> {
    if size.0 == 0 || size.1 == 0 || size.0 > 1024 || size.1 > 1024 {
        return Err(BakeError::Budget(size.0, size.1));
    }
    let Some((min, max)) = scene_bind_bbox(model) else {
        return Err(BakeError::Decode("model has no geometry to render".into()));
    };
    let worlds = scene_worlds(model, animation_index, time);
    let skeleton = model.skeletons.iter().find(|s| !s.joint_nodes.is_empty());
    // N385: the scene's key light (a directional light overrides the default).
    let light = key_light(&model.lights);
    let mut pixels = vec![0u8; (size.0 as usize) * (size.1 as usize) * 2];
    let mut drawables = Vec::new();
    for (index, node) in model.nodes.iter().enumerate() {
        let Some(mesh) = node.mesh.and_then(|mi| model.meshes.get(mi)) else {
            continue;
        };
        let Some(geometry) = resolve_geometry(mesh) else {
            continue;
        };
        // N226: morph the geometry before skinning / placement.
        let morphed = morphed_positions(
            geometry,
            mesh,
            model.animations.get(animation_index),
            Some(index),
            time,
        );
        let deformed: Vec<[f32; 3]> = if !geometry.skin_joints.is_empty() {
            match skeleton {
                Some(skeleton) => skin_arrays_multi(
                    &geometry.skin_joints,
                    &geometry.skin_weights,
                    &geometry.skin_joints2,
                    &geometry.skin_weights2,
                    &morphed,
                    skeleton,
                    &worlds,
                ),
                None => morphed
                    .iter()
                    .map(|p| transform_point(&worlds[index], *p))
                    .collect(),
            }
        } else {
            morphed
                .iter()
                .map(|p| transform_point(&worlds[index], *p))
                .collect()
        };
        let depth = mean_z(&deformed);
        // N365: skin the normals so shading follows the pose.
        let deformed_normals: Vec<[f32; 3]> = match skeleton {
            Some(skeleton)
                if !geometry.skin_joints.is_empty()
                    && geometry.normals.len() >= geometry.positions.len() =>
            {
                skin_normals_multi(
                    &geometry.skin_joints,
                    &geometry.skin_weights,
                    &geometry.skin_joints2,
                    &geometry.skin_weights2,
                    &geometry.normals,
                    skeleton,
                    &worlds,
                )
            }
            _ => geometry.normals.clone(),
        };
        let points = fit_to_box(&deformed, min, max, size.0, size.1);
        let (uv_set, uv_set2) = material_of(model, mesh)
            .map(|m| (m.uv_set(), m.uv_set2()))
            .unwrap_or((0, 0));
        let uvs = select_uvs(geometry, uv_set);
        let uvs_secondary = (uv_set2 != uv_set)
            .then(|| select_uvs(geometry, uv_set2))
            .flatten();
        let tint = material_color(model, mesh);
        let mask = material_masked(model, mesh);
        drawables.push(SceneDrawable {
            depth,
            points,
            uvs,
            indices: &geometry.indices,
            tint,
            mask,
            fill: tint.unwrap_or(fallback),
            shade: triangle_shades(
                &deformed,
                &deformed_normals,
                &geometry.indices,
                light,
                material_ambient(model, mesh),
            ),
            blend: material_blend(model, mesh),
            texture,
            colors: if geometry.colors.len() >= geometry.positions.len()
                && !geometry.colors.is_empty()
            {
                Some(&geometry.colors)
            } else {
                None
            },
            uvs_secondary,
            uv_transform: material_of(model, mesh)
                .map(|m| m.uv_transform())
                .unwrap_or_default(),
            normals: None,
            tangents: None,
            normal_map: None,
            normal_scale: 1.0,
            normal_ambient: 0.9,
            light,
            specular: 0.0,
            shininess: 1.0,
            normal_uv_transform: mdc_model::UvTransform::default(),
            emissive_map: None,
            emissive_strength: 1.0,
            emissive_uv_transform: mdc_model::UvTransform::default(),
            roughness_map: None,
            roughness_factor: 1.0,
            roughness_uv_transform: mdc_model::UvTransform::default(),
        });
    }
    draw_scene_sorted(&mut pixels, size, drawables)?;
    Ok(BakedTexture {
        width: size.0,
        height: size.1,
        rgb565: pixels,
    })
}

/// Mean world-space z of a set of points (0 when empty). Larger z is farther
/// under the glTF (-z forward) convention, so it is drawn first.
fn mean_z(points: &[[f32; 3]]) -> f32 {
    if points.is_empty() {
        return 0.0;
    }
    points.iter().map(|p| p[2]).sum::<f32>() / points.len() as f32
}

/// N185: render `frames` scene poses into atlas frames.
pub fn bake_scene_frames(
    model: &Model,
    animation_index: usize,
    frames: u32,
    size: (u32, u32),
    texture: Option<&crate::TextureMap>,
    fallback: u16,
) -> Result<Vec<BakedTexture>, BakeError> {
    if frames == 0 || frames > 64 {
        return Err(BakeError::FrameCount(frames as usize, 64));
    }
    let (start, end) = model
        .animations
        .get(animation_index)
        .map(animation_span)
        .unwrap_or((0.0, 0.0));
    let mut out = Vec::with_capacity(frames as usize);
    for frame in 0..frames {
        let time = if frames > 1 {
            start + (end - start) * (frame as f32) / ((frames - 1) as f32)
        } else {
            start
        };
        out.push(render_scene_at(
            model,
            animation_index,
            time,
            size,
            texture,
            fallback,
        )?);
    }
    Ok(out)
}

/// N185: bake a whole multi-mesh scene into a `BakedSprites` package.
pub fn bake_scene_to_package(
    model: &Model,
    animation_index: usize,
    frames: u32,
    size: (u32, u32),
    texture: Option<&crate::TextureMap>,
    fallback: u16,
) -> Result<mdc_model::MdcPackage, BakeError> {
    let baked = bake_scene_frames(model, animation_index, frames, size, texture, fallback)?;
    frames_to_package(baked, frames, size)
}

/// N187: render a runtime package's mesh geometry to a frame. Uses the package's
/// own `texture` blob and material colours; falls back to the preview when a
/// mesh has no geometry.
pub fn render_package_mesh(
    package: &mdc_model::DevicePackage,
    size: (u32, u32),
) -> Result<BakedTexture, BakeError> {
    if size.0 == 0 || size.1 == 0 || size.0 > 1024 || size.1 > 1024 {
        return Err(BakeError::Budget(size.0, size.1));
    }
    let build = |blob: &mdc_model::TextureBlob| -> Result<crate::TextureMap, BakeError> {
        let map = crate::TextureMap::from_rgb565(blob.width, blob.height, blob.rgb565.clone())?;
        Ok(if blob.alpha.is_empty() {
            map
        } else {
            map.with_alpha(blob.alpha.clone())
        })
    };
    // N358: per-material textures (N353), else the single package texture.
    let mut textures: Vec<Option<crate::TextureMap>> =
        Vec::with_capacity(package.texture_blobs.len());
    for blob in &package.texture_blobs {
        textures.push(if blob.validate() {
            Some(build(blob)?)
        } else {
            None
        });
    }
    let single = match package.texture.as_ref().filter(|t| t.validate()) {
        Some(blob) => Some(build(blob)?),
        None => None,
    };
    let mut points = Vec::new();
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for mesh in &package.meshes {
        let Some(geometry) = resolve_geometry(mesh) else {
            continue;
        };
        for p in &geometry.positions {
            min[0] = min[0].min(p[0]);
            min[1] = min[1].min(p[1]);
            max[0] = max[0].max(p[0]);
            max[1] = max[1].max(p[1]);
            points.push(*p);
        }
    }
    if points.is_empty() {
        return Err(BakeError::Decode("package has no mesh geometry".into()));
    }
    let mut pixels = vec![0u8; (size.0 as usize) * (size.1 as usize) * 2];
    for mesh in &package.meshes {
        let Some(geometry) = resolve_geometry(mesh) else {
            continue;
        };
        let projected = fit_to_box(&geometry.positions, min, max, size.0, size.1);
        let (uv_set, uv_set2) = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .map(|m| (m.uv_set(), m.uv_set2()))
            .unwrap_or((0, 0));
        let uvs = select_uvs(geometry, uv_set);
        let uvs_secondary = (uv_set2 != uv_set)
            .then(|| select_uvs(geometry, uv_set2))
            .flatten();
        let tint = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .and_then(|material| material.color_rgb565());
        let mask = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .is_some_and(|material| material.is_masked());
        let fill = tint.unwrap_or(0xF800);
        let shade = triangle_shades(
            &geometry.positions,
            &geometry.normals,
            &geometry.indices,
            key_light(&package.lights),
            mesh.material
                .and_then(|mi| package.materials.get(mi))
                .map(|m| (SHADE_AMBIENT + 0.35 * m.roughness()).min(0.9))
                .unwrap_or(0.9),
        );
        let blend = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .map(|material| {
                if material.is_blended() {
                    material.alpha_u8()
                } else {
                    255
                }
            })
            .unwrap_or(255);
        let mesh_texture = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .and_then(|material| material.texture)
            .and_then(|index| textures.get(index))
            .and_then(|texture| texture.as_ref())
            .or(single.as_ref());
        crate::texture::draw_indexed(
            &mut pixels,
            &projected,
            uvs,
            uvs_secondary,
            &geometry.indices,
            size.0,
            size.1,
            mesh_texture,
            tint,
            mask,
            blend,
            fill,
            Some(&shade),
            if geometry.colors.len() >= geometry.positions.len() && !geometry.colors.is_empty() {
                Some(geometry.colors.as_slice())
            } else {
                None
            },
            mesh.material
                .and_then(|mi| package.materials.get(mi))
                .map(|m| m.uv_transform())
                .unwrap_or_default(),
            None,
            None,
            mdc_model::MAX_GEOMETRY_VERTICES,
        )?;
    }
    Ok(BakedTexture {
        width: size.0,
        height: size.1,
        rgb565: pixels,
    })
}

/// N380: pack a package's per-material textures into one bounded atlas page,
/// remapping each packed material's texture index and the UVs of the meshes that
/// use it. Returns `false` (leaving the package untouched) when the blobs do not
/// fit in `max_page`, when there are fewer than two, or when any carries an alpha
/// mask.
pub fn pack_material_atlas(package: &mut mdc_model::DevicePackage, max_page: u32) -> bool {
    // Only fully valid, opaque blobs are packed.
    let mut parts: Vec<(usize, u32, u32)> = Vec::new();
    for (index, blob) in package.texture_blobs.iter().enumerate() {
        if !blob.validate() || blob.width > max_page || blob.height > max_page {
            continue;
        }
        parts.push((index, blob.width, blob.height));
    }
    if parts.len() < 2 {
        return false;
    }
    // Shelf pack, tallest first, within a page `max_page` wide.
    parts.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    let mut placements: Vec<(usize, u32, u32, u32, u32)> = Vec::new(); // index,x,y,w,h
    let (mut x, mut y, mut row_h) = (0u32, 0u32, 0u32);
    for (index, w, h) in &parts {
        if x + w > max_page {
            y += row_h;
            x = 0;
            row_h = 0;
        }
        if y + h > max_page {
            return false;
        }
        placements.push((*index, x, y, *w, *h));
        x += w;
        row_h = row_h.max(*h);
    }
    let page_h = y + row_h;
    if page_h == 0 {
        return false;
    }
    // Blit each blob into the page, carrying any 1-bit alpha mask across.
    let mut page = vec![0u8; (max_page as usize) * (page_h as usize) * 2];
    let has_alpha = placements.iter().any(|(index, _, _, _, _)| {
        package
            .texture_blobs
            .get(*index)
            .is_some_and(|blob| !blob.alpha.is_empty())
    });
    let page_pixels = (max_page as usize) * (page_h as usize);
    let mut page_alpha = vec![0xFFu8; page_pixels.div_ceil(8)];
    for (index, px, py, w, h) in &placements {
        let Some(blob) = package.texture_blobs.get(*index) else {
            return false;
        };
        for row in 0..*h {
            let src = ((row * *w) * 2) as usize;
            let dst = (((py + row) * max_page + px) * 2) as usize;
            let len = (*w * 2) as usize;
            page[dst..dst + len].copy_from_slice(&blob.rgb565[src..src + len]);
            if !blob.alpha.is_empty() {
                for col in 0..*w {
                    let from = ((row * *w + col) / 8) as usize;
                    let from_bit = (row * *w + col) % 8;
                    let opaque = blob
                        .alpha
                        .get(from)
                        .is_some_and(|byte| byte & (1 << from_bit) != 0);
                    let to = ((py + row) * max_page + px + col) as usize;
                    if opaque {
                        page_alpha[to / 8] |= 1 << (to % 8);
                    } else {
                        page_alpha[to / 8] &= !(1 << (to % 8));
                    }
                }
            }
        }
    }
    // Remap each placed material and the UVs of the meshes that use it.
    let rect_of = |index: usize| -> Option<(u32, u32, u32, u32)> {
        placements
            .iter()
            .find(|(i, _, _, _, _)| *i == index)
            .map(|(_, px, py, w, h)| (*px, *py, *w, *h))
    };
    // The rect each material should sample from, based on its original blob.
    let material_rects: Vec<Option<(u32, u32, u32, u32)>> = package
        .materials
        .iter()
        .map(|material| material.texture.and_then(rect_of))
        .collect();
    for material in &mut package.materials {
        if material.texture.and_then(rect_of).is_some() {
            material.texture = Some(0);
        }
    }
    let page_w = max_page as f32;
    let page_hf = page_h as f32;
    for mesh in &mut package.meshes {
        let Some(material_index) = mesh.material else {
            continue;
        };
        let Some((px, py, w, h)) = material_rects.get(material_index).copied().flatten() else {
            continue;
        };
        let Some(geometry) = mesh.geometry.as_mut() else {
            continue;
        };
        if geometry.uv.len() < geometry.positions.len() || geometry.uv.is_empty() {
            continue;
        }
        for uv in &mut geometry.uv {
            uv[0] = (px as f32 + uv[0] * w as f32) / page_w;
            uv[1] = (py as f32 + uv[1] * h as f32) / page_hf;
        }
    }
    package.texture_blobs = vec![mdc_model::TextureBlob {
        width: max_page,
        height: page_h,
        rgb565: page,
        alpha: if has_alpha { page_alpha } else { Vec::new() },
    }];
    package.texture = package.texture_blobs.first().cloned();
    true
}

/// N385: the renderer's key light, from the first directional light (its
/// direction points away from the source, so it is negated; intensity scales it).
fn key_light(lights: &[mdc_model::Light]) -> [f32; 3] {
    lights
        .iter()
        .find(|light| light.is_directional())
        .map(|light| {
            let s = light.intensity.clamp(0.0, 4.0);
            [
                -light.direction[0] * s,
                -light.direction[1] * s,
                -light.direction[2] * s,
            ]
        })
        .filter(|v| v[0] * v[0] + v[1] * v[1] + v[2] * v[2] > 1e-6)
        .unwrap_or(SHADE_LIGHT)
}

/// N383: the mesh UV set a material samples (falls back to the first set).
fn select_uvs<'a>(geometry: &'a mdc_model::MeshGeometry, set: u8) -> Option<&'a [[f32; 2]]> {
    let usable = |uv: &'a [[f32; 2]]| {
        if uv.len() >= geometry.positions.len() && !uv.is_empty() {
            Some(uv)
        } else {
            None
        }
    };
    if set == 1 {
        usable(&geometry.uv2).or_else(|| usable(&geometry.uv))
    } else {
        usable(&geometry.uv)
    }
}

/// N194: the bind-pose XY box of every mesh node in a package.
fn package_bind_bbox(package: &mdc_model::DevicePackage) -> Option<([f32; 2], [f32; 2])> {
    let worlds = node_world_matrices(&package.nodes);
    let mut points = Vec::new();
    for (index, node) in package.nodes.iter().enumerate() {
        let Some(mesh) = node.mesh.and_then(|mi| package.meshes.get(mi)) else {
            continue;
        };
        let Some(geometry) = resolve_geometry(mesh) else {
            continue;
        };
        for position in &geometry.positions {
            points.push(transform_point(&worlds[index], *position));
        }
    }
    bbox_xy(&points)
}

/// N193: render a package's whole mesh scene at `time` (seconds). Nodes with a
/// mesh node open into the package's first animation; a skeleton-capable mesh is
/// skinned. The package's own texture blob and material colours are used, and the
/// bind-pose box keeps frames from jittering. This is the Form B device renderer.
pub fn render_package_scene(
    package: &mdc_model::DevicePackage,
    time: f32,
    size: (u32, u32),
) -> Result<BakedTexture, BakeError> {
    render_package_scene_clip(package, SceneClip::First, time, size)
}

/// N315: which package animation the device renderer poses at `time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SceneClip {
    /// No animation: place nodes at their static world transform.
    Static,
    /// Pose with `animations[index]`; an out-of-range index renders static.
    Index(usize),
    /// Pose with the first animation (the legacy default).
    #[default]
    First,
}

/// N315: like [`render_package_scene`] but selects the animation clip explicitly,
/// so a device can play more than `animations.first()`.
pub fn render_package_scene_clip(
    package: &mdc_model::DevicePackage,
    clip: SceneClip,
    time: f32,
    size: (u32, u32),
) -> Result<BakedTexture, BakeError> {
    render_package_scene_clip_filtered(package, clip, time, size, crate::Filter::Nearest)
}

/// N347: like [`render_package_scene_clip`] but samples the package texture with
/// the device's configured filter.
pub fn render_package_scene_clip_filtered(
    package: &mdc_model::DevicePackage,
    clip: SceneClip,
    time: f32,
    size: (u32, u32),
    filter: crate::Filter,
) -> Result<BakedTexture, BakeError> {
    render_package_scene_clip_mode(package, clip, time, size, filter, ShadeMode::Flat)
}

/// N386: how the package renderer shades each surface. `Flat` is the historical
/// per-triangle Lambert; `Smooth` shades per pixel from interpolated normals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShadeMode {
    #[default]
    Flat,
    Smooth,
    /// N387: smooth shading plus a Blinn-Phong specular highlight.
    SmoothSpecular,
}

/// N386: like [`render_package_scene_clip_filtered`] but with an explicit shade
/// mode (`Smooth` gives per-pixel Gouraud shading).
pub fn render_package_scene_clip_mode(
    package: &mdc_model::DevicePackage,
    clip: SceneClip,
    time: f32,
    size: (u32, u32),
    filter: crate::Filter,
    mode: ShadeMode,
) -> Result<BakedTexture, BakeError> {
    if size.0 == 0 || size.1 == 0 || size.0 > 1024 || size.1 > 1024 {
        return Err(BakeError::Budget(size.0, size.1));
    }
    let Some((min, max)) = package_bind_bbox(package) else {
        return Err(BakeError::Decode("package has no mesh geometry".into()));
    };
    let build_texture = |blob: &mdc_model::TextureBlob| -> Result<crate::TextureMap, BakeError> {
        let map = crate::TextureMap::from_rgb565(blob.width, blob.height, blob.rgb565.clone())?
            .with_filter(filter);
        Ok(if blob.alpha.is_empty() {
            map
        } else {
            map.with_alpha(blob.alpha.clone())
        })
    };
    // N353: per-material textures, indexed by `Material.texture`.
    let mut textures: Vec<Option<crate::TextureMap>> =
        Vec::with_capacity(package.texture_blobs.len());
    for blob in &package.texture_blobs {
        textures.push(if blob.validate() {
            Some(build_texture(blob)?)
        } else {
            None
        });
    }
    // The single `texture` is the fallback when a material has no per-material one.
    let single = match package.texture.as_ref().filter(|t| t.validate()) {
        Some(blob) => Some(build_texture(blob)?),
        None => None,
    };
    let animation = match clip {
        SceneClip::Static => None,
        SceneClip::First => package.animations.first(),
        SceneClip::Index(index) => package.animations.get(index),
    };
    let worlds = match animation {
        Some(animation) => pose_world_matrices(&package.nodes, animation, time),
        None => node_world_matrices(&package.nodes),
    };
    let skeleton = package.skeletons.iter().find(|s| !s.joint_nodes.is_empty());
    // N385: the package's key light (a directional light overrides the default).
    let light = key_light(&package.lights);
    // N386: a shared flat normal map drives smooth (per-pixel) shading.
    let flat_normal = crate::texture::flat_normal_map();
    let mut pixels = vec![0u8; (size.0 as usize) * (size.1 as usize) * 2];
    let mut drawables = Vec::new();
    for (index, node) in package.nodes.iter().enumerate() {
        let Some(mesh) = node.mesh.and_then(|mi| package.meshes.get(mi)) else {
            continue;
        };
        let Some(geometry) = resolve_geometry(mesh) else {
            continue;
        };
        // N226: morph the geometry before skinning / placement.
        let morphed = morphed_positions(geometry, mesh, animation, Some(index), time);
        let deformed: Vec<[f32; 3]> = if !geometry.skin_joints.is_empty() {
            match skeleton {
                Some(skeleton) => skin_arrays_multi(
                    &geometry.skin_joints,
                    &geometry.skin_weights,
                    &geometry.skin_joints2,
                    &geometry.skin_weights2,
                    &morphed,
                    skeleton,
                    &worlds,
                ),
                None => morphed
                    .iter()
                    .map(|p| transform_point(&worlds[index], *p))
                    .collect(),
            }
        } else {
            morphed
                .iter()
                .map(|p| transform_point(&worlds[index], *p))
                .collect()
        };
        let depth = mean_z(&deformed);
        // N365: skin the normals so shading follows the pose.
        let deformed_normals: Vec<[f32; 3]> = match skeleton {
            Some(skeleton)
                if !geometry.skin_joints.is_empty()
                    && geometry.normals.len() >= geometry.positions.len() =>
            {
                skin_normals_multi(
                    &geometry.skin_joints,
                    &geometry.skin_weights,
                    &geometry.skin_joints2,
                    &geometry.skin_weights2,
                    &geometry.normals,
                    skeleton,
                    &worlds,
                )
            }
            _ => geometry.normals.clone(),
        };
        let points = fit_to_box(&deformed, min, max, size.0, size.1);
        let (uv_set, uv_set2) = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .map(|m| (m.uv_set(), m.uv_set2()))
            .unwrap_or((0, 0));
        let uvs = select_uvs(geometry, uv_set);
        let uvs_secondary = (uv_set2 != uv_set)
            .then(|| select_uvs(geometry, uv_set2))
            .flatten();
        let tint = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .and_then(|material| material.color_rgb565());
        let mask = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .is_some_and(|material| material.is_masked());
        // N353: the mesh's own material texture, else the single package texture.
        let mesh_texture = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .and_then(|material| material.texture)
            .and_then(|index| textures.get(index))
            .and_then(|texture| texture.as_ref())
            .or(single.as_ref());
        // N379: a tangent-space normal map turns shading per-pixel. N386: the
        // `Smooth` mode does the same with a flat map at zero scale.
        let (normal_index, normal_scale) = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .map(|material| material.normal_map())
            .unwrap_or((None, 1.0));
        let normal_map = normal_index
            .and_then(|index| textures.get(index))
            .and_then(|texture| texture.as_ref());
        let normal_ambient = package_material_ambient(package, mesh);
        let has_normals = deformed_normals.len() >= deformed.len();
        let usable_normal_map = normal_map.is_some()
            && geometry.tangents.len() >= geometry.positions.len()
            && has_normals;
        // N394: a metallicRoughness texture also needs the per-pixel pass.
        let (roughness_index, roughness_factor) = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .map(|material| (material.roughness_texture(), material.roughness()))
            .unwrap_or((None, 1.0));
        let roughness_map = roughness_index
            .and_then(|index| textures.get(index))
            .and_then(|texture| texture.as_ref());
        let smooth = matches!(mode, ShadeMode::Smooth | ShadeMode::SmoothSpecular)
            && has_normals
            && !usable_normal_map;
        let per_pixel = (usable_normal_map || smooth || roughness_map.is_some()) && has_normals;
        // N387: material-derived Blinn-Phong parameters for the specular mode.
        let (specular, shininess) = if mode == ShadeMode::SmoothSpecular {
            let (roughness, metallic, specular_factor) = mesh
                .material
                .and_then(|mi| package.materials.get(mi))
                .map(|material| {
                    (
                        material.roughness(),
                        material.metallic(),
                        material.specular_factor(),
                    )
                })
                .unwrap_or((1.0, 1.0, 1.0));
            (
                (1.0 - roughness) * (0.4 + 0.6 * metallic) * 0.9 * specular_factor,
                8.0 + 120.0 * (1.0 - roughness),
            )
        } else {
            (0.0, 1.0)
        };
        // N393/N395: the emissive texture and per-slot UV transforms.
        let (emissive_index, emissive_strength) = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .map(|material| material.emissive_map())
            .unwrap_or((None, 1.0));
        let emissive_map = emissive_index
            .and_then(|index| textures.get(index))
            .and_then(|texture| texture.as_ref());
        let (emissive_uv_transform, normal_uv_transform, roughness_uv_transform) = mesh
            .material
            .and_then(|mi| package.materials.get(mi))
            .map(|material| {
                (
                    material.emissive_uv_transform(),
                    material.normal_uv_transform(),
                    material.roughness_uv_transform(),
                )
            })
            .unwrap_or_default();
        let occlusion = occlusion_factors(package, mesh, geometry, &textures);
        let shade = if per_pixel {
            // The per-pixel pass applies the Lambert term, so the per-triangle
            // factor carries occlusion only.
            occlusion.unwrap_or_else(|| vec![1.0; geometry.indices.len() / 3])
        } else {
            triangle_shades_ambients(
                &deformed,
                &deformed_normals,
                &geometry.indices,
                light,
                normal_ambient,
                occlusion.as_deref(),
            )
        };
        let use_flat = !usable_normal_map && per_pixel;
        let (drawable_map, drawable_scale) = if usable_normal_map {
            (normal_map, normal_scale)
        } else if use_flat {
            (Some(&flat_normal), 0.0)
        } else {
            (None, normal_scale)
        };
        drawables.push(SceneDrawable {
            depth,
            points,
            uvs,
            indices: &geometry.indices,
            tint,
            mask,
            fill: tint.unwrap_or(0xF800),
            shade,
            texture: mesh_texture,
            blend: mesh
                .material
                .and_then(|mi| package.materials.get(mi))
                .map(|material| {
                    if material.is_blended() {
                        material.alpha_u8()
                    } else {
                        255
                    }
                })
                .unwrap_or(255),
            colors: if geometry.colors.len() >= geometry.positions.len()
                && !geometry.colors.is_empty()
            {
                Some(&geometry.colors)
            } else {
                None
            },
            uvs_secondary,
            uv_transform: mesh
                .material
                .and_then(|mi| package.materials.get(mi))
                .map(|m| m.uv_transform())
                .unwrap_or_default(),
            normals: if per_pixel {
                Some(deformed_normals)
            } else {
                None
            },
            tangents: if per_pixel {
                Some(if geometry.tangents.len() >= geometry.positions.len() {
                    geometry.tangents.clone()
                } else {
                    vec![[1.0, 0.0, 0.0, 1.0]; deformed.len()]
                })
            } else {
                None
            },
            normal_map: if per_pixel { drawable_map } else { None },
            normal_scale: drawable_scale,
            normal_ambient,
            light,
            specular,
            shininess,
            normal_uv_transform,
            emissive_map,
            emissive_strength,
            emissive_uv_transform,
            roughness_map,
            roughness_factor,
            roughness_uv_transform,
        });
    }
    draw_scene_sorted(&mut pixels, size, drawables)?;
    Ok(BakedTexture {
        width: size.0,
        height: size.1,
        rgb565: pixels,
    })
}

/// N111: bake `frames` poses of the first skinned mesh in `model`, sampled from
/// `animation_index` across its time span. At most 64 frames. Solid colour.
pub fn bake_skinned_mesh(
    model: &Model,
    animation_index: usize,
    frames: u32,
    sprite: (u32, u32),
    color: u16,
) -> Result<Vec<BakedTexture>, BakeError> {
    let (per_frame, mesh) = skinned_point_frames(model, animation_index, frames, sprite)?;
    let mut baked = Vec::with_capacity(per_frame.len());
    for points in per_frame {
        let rgb565 = raster_points(&points, &mesh.preview_indices, sprite.0, sprite.1, color)?;
        baked.push(BakedTexture {
            width: sprite.0,
            height: sprite.1,
            rgb565,
        });
    }
    Ok(baked)
}

/// N115: bake skinned poses with the mesh's UVs sampled from `texture`. A mesh
/// with no UVs (or `texture` `None`) falls back to the solid `color`.
pub fn bake_textured_mesh(
    model: &Model,
    animation_index: usize,
    frames: u32,
    sprite: (u32, u32),
    texture: Option<&crate::TextureMap>,
    color: u16,
) -> Result<Vec<BakedTexture>, BakeError> {
    let (per_frame, mesh) = skinned_point_frames(model, animation_index, frames, sprite)?;
    let uvs: Option<&[[f32; 2]]> =
        if mesh.preview_uv.len() >= mesh.preview_positions.len() && !mesh.preview_uv.is_empty() {
            Some(&mesh.preview_uv)
        } else {
            None
        };
    let mut baked = Vec::with_capacity(per_frame.len());
    let tint = material_color(model, mesh);
    let mask = material_masked(model, mesh);
    for points in per_frame {
        let rgb565 = crate::texture::raster_textured(
            &points,
            uvs,
            &mesh.preview_indices,
            sprite.0,
            sprite.1,
            texture,
            tint,
            mask,
            color,
        )?;
        baked.push(BakedTexture {
            width: sprite.0,
            height: sprite.1,
            rgb565,
        });
    }
    Ok(baked)
}

/// Pack baked frames into a single-clip `BakedSprites` runtime package.
fn frames_to_package(
    baked: Vec<BakedTexture>,
    frames: u32,
    sprite: (u32, u32),
) -> Result<mdc_model::MdcPackage, BakeError> {
    let clips = vec![mdc_model::AtlasClip {
        name: "bake-mesh".into(),
        start: 0,
        frames,
        fps: 30,
        looping: true,
        fade_in_ms: 0,
        fade_out_ms: 0,
    }];
    frames_to_clip_package(baked, clips, sprite)
}

/// N345: pack baked frames into a `BakedSprites` package with one or more clips.
fn frames_to_clip_package(
    baked: Vec<BakedTexture>,
    clips: Vec<mdc_model::AtlasClip>,
    sprite: (u32, u32),
) -> Result<mdc_model::MdcPackage, BakeError> {
    let frames = baked.len() as u32;
    if frames == 0 {
        return Err(BakeError::FrameCount(0, 64));
    }
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
        clips,
    };
    let package = mdc_model::DevicePackage {
        target: "esp32-plane".into(),
        mode: mdc_model::PackageMode::BakedSprites,
        nodes: Vec::new(),
        meshes: Vec::new(),
        textures: Vec::new(),
        materials: Vec::new(),
        lights: Vec::new(),
        skeletons: Vec::new(),
        animations: Vec::new(),
        sprites: Vec::new(),
        layers: Vec::new(),
        estimated_bytes: atlas.rgb565.len() as u64,
        atlas: Some(atlas),
        texture: None,
        texture_blobs: Vec::new(),
    };
    mdc_model::MdcPackage::seal(package).map_err(|e| BakeError::Package(e.to_string()))
}

/// N345: bake several animations of one scene into a single multi-clip atlas
/// package, so a weak device can switch clips with `config animation`.
pub fn bake_scene_clips_to_package(
    model: &Model,
    clips: &[(usize, u32)],
    sprite: (u32, u32),
    texture: Option<&crate::TextureMap>,
    fallback: u16,
) -> Result<mdc_model::MdcPackage, BakeError> {
    if clips.is_empty() {
        return Err(BakeError::FrameCount(0, 64));
    }
    let mut baked = Vec::new();
    let mut atlas_clips = Vec::new();
    for (animation, frames) in clips {
        let frames = (*frames).clamp(1, 64);
        let part = bake_scene_frames(model, *animation, frames, sprite, texture, fallback)?;
        let name = model
            .animations
            .get(*animation)
            .map(|a| a.name.clone())
            .unwrap_or_else(|| format!("clip{animation}"));
        atlas_clips.push(mdc_model::AtlasClip {
            name,
            start: baked.len() as u32,
            frames: part.len() as u32,
            fps: 30,
            looping: true,
            fade_in_ms: 0,
            fade_out_ms: 0,
        });
        baked.extend(part);
    }
    if baked.len() > 64 {
        return Err(BakeError::FrameCount(baked.len(), 64));
    }
    frames_to_clip_package(baked, atlas_clips, sprite)
}

/// N112: bake solid skinned frames straight into a runtime `BakedSprites`
/// package so a device can receive the poses over the `PACKAGE` path.
pub fn bake_skinned_to_package(
    model: &Model,
    animation_index: usize,
    frames: u32,
    sprite: (u32, u32),
    color: u16,
) -> Result<mdc_model::MdcPackage, BakeError> {
    let baked = bake_skinned_mesh(model, animation_index, frames, sprite, color)?;
    frames_to_package(baked, frames, sprite)
}

/// N116: bake textured skinned frames into a runtime `BakedSprites` package.
pub fn bake_textured_to_package(
    model: &Model,
    animation_index: usize,
    frames: u32,
    sprite: (u32, u32),
    texture: Option<&crate::TextureMap>,
    color: u16,
) -> Result<mdc_model::MdcPackage, BakeError> {
    let baked = bake_textured_mesh(model, animation_index, frames, sprite, texture, color)?;
    frames_to_package(baked, frames, sprite)
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
            preview_uv: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
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
    fn textured_bake_samples_the_texture_not_the_fallback() {
        let model = skinned_model();
        // 2×1 texture: left red, right green.
        let texture = crate::TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
        let frames = bake_textured_mesh(&model, 0, 1, (16, 16), Some(&texture), 0x001F).unwrap();
        let frame = &frames[0];
        assert_eq!(frame.rgb565.len(), 16 * 16 * 2);
        // UV (0,0) lands on the origin texel: red. The blue fallback never shows.
        let at = |x: usize, y: usize| {
            u16::from_le_bytes([
                frame.rgb565[(y * 16 + x) * 2],
                frame.rgb565[(y * 16 + x) * 2 + 1],
            ])
        };
        assert_eq!(at(8, 8), 0xF800);
        assert!(frame
            .rgb565
            .chunks_exact(2)
            .all(|px| u16::from_le_bytes([px[0], px[1]]) != 0x001F));
    }

    #[test]
    fn textured_bake_to_package_round_trips() {
        let model = skinned_model();
        let texture = crate::TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
        let package =
            bake_textured_to_package(&model, 0, 3, (16, 16), Some(&texture), 0x001F).unwrap();
        assert_eq!(package.manifest.mode, mdc_model::PackageMode::BakedSprites);
        let bytes = package.to_bytes().unwrap();
        let back = mdc_model::MdcPackage::from_bytes(&bytes).unwrap();
        assert_eq!(back, package);
    }

    fn morph_model() -> Model {
        let mut model = skinned_model();
        // The morph channel targets the node that owns the mesh.
        model.nodes[1].mesh = Some(0);
        // No joint rotation; only a morph-weight channel drives the pose.
        model.meshes[0].morph_targets =
            vec![vec![[1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 0.0]]];
        model.meshes[0].morph_weights = vec![0.0];
        model.animations[0] = Animation {
            name: "smile".into(),
            channels: 1,
            tracks: vec![AnimationTrack {
                node: 1,
                path: "weights".into(),
                interpolation: "LINEAR".into(),
                times: vec![0.0, 1.0],
                values: vec![0.0, 1.0],
            }],
        };
        model
    }

    #[test]
    fn apply_morphs_adds_weighted_deltas() {
        let base = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        let targets = vec![vec![[1.0, 0.0, 0.0], [0.0, 2.0, 0.0]]];
        let out = apply_morphs(&base, &targets, &[0.5]);
        assert_eq!(out, vec![[0.5, 0.0, 0.0], [1.0, 1.0, 0.0]]);
    }

    #[test]
    fn morph_weights_sample_over_time() {
        let model = morph_model();
        let animation = &model.animations[0];
        assert_eq!(morph_weights_at(animation, 1, 0.0).unwrap(), vec![0.0]);
        assert_eq!(morph_weights_at(animation, 1, 1.0).unwrap(), vec![1.0]);
        assert!(morph_weights_at(animation, 0, 0.5).is_none());
    }

    #[test]
    fn morph_bake_frames_move_with_the_weights() {
        let model = morph_model();
        let frames = bake_skinned_mesh(&model, 0, 3, (16, 16), 0xF800).unwrap();
        assert_eq!(frames.len(), 3);
        assert_ne!(frames[0].rgb565, frames[2].rgb565, "morph moves the mesh");
    }

    #[test]
    fn render_frame_at_poses_a_single_frame() {
        let model = skinned_model();
        let bind = render_frame_at(&model, 0, 0.0, (16, 16), None, 0xF800).unwrap();
        let posed = render_frame_at(&model, 0, 1.0, (16, 16), None, 0xF800).unwrap();
        assert_eq!((bind.width, bind.height), (16, 16));
        assert_ne!(bind.rgb565, posed.rgb565);

        let texture = crate::TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
        let textured = render_frame_at(&model, 0, 0.0, (16, 16), Some(&texture), 0x001F).unwrap();
        assert!(textured.rgb565.chunks_exact(2).any(|px| px == [0x00, 0xF8]));
    }

    #[test]
    fn geometry_render_draws_the_full_mesh() {
        use mdc_model::MeshGeometry;
        let mut model = skinned_model();
        model.meshes[0].geometry = Some(MeshGeometry {
            positions: vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uv: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            uv2: Vec::new(),
            normals: Vec::new(),
            skin_joints: vec![[1, 0, 0, 0]; 4],
            skin_joints2: Vec::new(),
            skin_weights2: Vec::new(),
            skin_weights: vec![[1.0, 0.0, 0.0, 0.0]; 4],
            morph_targets: Vec::new(),
            colors: Vec::new(),
            tangents: Vec::new(),
        });
        let frame = render_geometry_at(&model, 0, 0.0, (16, 16), None, 0xF800).unwrap();
        let covered = frame
            .rgb565
            .chunks_exact(2)
            .filter(|px| px.iter().any(|b| *b != 0))
            .count();
        // A fitted quad at 16×16 covers many pixels, far more than one triangle.
        assert!(covered > 40, "quad area covered {covered}");
        assert_eq!((frame.width, frame.height), (16, 16));

        let package = bake_geometry_to_package(&model, 0, 3, (16, 16), None, 0xF800).unwrap();
        assert_eq!(package.manifest.mode, mdc_model::PackageMode::BakedSprites);
        let bytes = package.to_bytes().unwrap();
        assert_eq!(mdc_model::MdcPackage::from_bytes(&bytes).unwrap(), package);
    }

    fn quad_geometry() -> mdc_model::MeshGeometry {
        mdc_model::MeshGeometry {
            positions: vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uv: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            uv2: Vec::new(),
            normals: Vec::new(),
            skin_joints: Vec::new(),
            skin_joints2: Vec::new(),
            skin_weights2: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
            colors: Vec::new(),
            tangents: Vec::new(),
        }
    }

    fn two_mesh_model() -> Model {
        let quad = |name: &str, material: usize| mdc_model::Mesh {
            name: name.into(),
            vertices: 4,
            indices: 6,
            material: Some(material),
            geometry: Some(quad_geometry()),
            ..mdc_model::Mesh::default()
        };
        Model {
            source: "test".into(),
            nodes: vec![
                Node {
                    name: "root".into(),
                    children: vec![1, 2],
                    ..Node::default()
                },
                Node {
                    name: "left".into(),
                    mesh: Some(0),
                    translation: [-0.6, 0.0, 0.0],
                    ..Node::default()
                },
                Node {
                    name: "right".into(),
                    mesh: Some(1),
                    translation: [0.6, 0.0, 0.0],
                    ..Node::default()
                },
            ],
            meshes: vec![quad("left", 0), quad("right", 1)],
            materials: vec![
                mdc_model::Material {
                    name: "red".into(),
                    texture: None,
                    color: Some([255, 0, 0]),
                    ..mdc_model::Material::default()
                },
                mdc_model::Material {
                    name: "green".into(),
                    texture: None,
                    color: Some([0, 255, 0]),
                    ..mdc_model::Material::default()
                },
            ],
            ..Model::default()
        }
    }

    #[test]
    fn scene_render_draws_every_mesh_with_its_material() {
        let model = two_mesh_model();
        let frame = render_scene_at(&model, 0, 0.0, (32, 16), None, 0xFFFF).unwrap();
        let at = |x: usize, y: usize| {
            u16::from_le_bytes([
                frame.rgb565[(y * 32 + x) * 2],
                frame.rgb565[(y * 32 + x) * 2 + 1],
            ])
        };
        // The left word is red, the right is green.
        assert_eq!(at(6, 8), 0xF800, "left mesh red");
        assert_eq!(at(25, 8), 0x07E0, "right mesh green");

        let package = bake_scene_to_package(&model, 0, 3, (32, 16), None, 0xFFFF).unwrap();
        let bytes = package.to_bytes().unwrap();
        assert_eq!(mdc_model::MdcPackage::from_bytes(&bytes).unwrap(), package);
    }

    #[test]
    fn package_mesh_render_draws_textured_geometry() {
        let model = two_mesh_model();
        // A mesh-mode package carries the geometry (not a baked atlas).
        let package = crate::mesh_package_with_texture(
            &model,
            &mdc_model::RendererCapability::full_3d(),
            None,
        )
        .unwrap();
        let frame = render_package_mesh(&package.package, (16, 16)).unwrap();
        assert!(frame
            .rgb565
            .chunks_exact(2)
            .any(|px| px.iter().any(|b| *b != 0)));
    }

    #[test]
    fn package_scene_render_animates_node_transforms() {
        let mut model = two_mesh_model();
        model.animations = vec![Animation {
            name: "slide".into(),
            channels: 1,
            tracks: vec![AnimationTrack {
                node: 1,
                path: "translation".into(),
                interpolation: "LINEAR".into(),
                times: vec![0.0, 1.0],
                values: vec![-0.6, 0.0, 0.0, 0.6, 0.0, 0.0],
            }],
        }];
        let package = crate::mesh_package_with_texture(
            &model,
            &mdc_model::RendererCapability::full_3d(),
            None,
        )
        .unwrap();
        let bind = render_package_scene(&package.package, 0.0, (24, 24)).unwrap();
        let posed = render_package_scene(&package.package, 1.0, (24, 24)).unwrap();
        assert_eq!((bind.width, bind.height), (24, 24));
        assert_ne!(bind.rgb565, posed.rgb565, "node animation moved a mesh");
    }

    #[test]
    fn scene_clip_selects_between_package_animations() {
        let clip = |name: &str, x: f32| Animation {
            name: name.into(),
            channels: 1,
            tracks: vec![AnimationTrack {
                node: 1,
                path: "translation".into(),
                interpolation: "LINEAR".into(),
                times: vec![0.0, 1.0],
                values: vec![x, 0.0, 0.0, x, 0.0, 0.0],
            }],
        };
        let mut model = two_mesh_model();
        model.animations = vec![clip("left", -0.6), clip("right", 0.6)];
        let package = crate::mesh_package_with_texture(
            &model,
            &mdc_model::RendererCapability::full_3d(),
            None,
        )
        .unwrap();
        let first =
            render_package_scene_clip(&package.package, SceneClip::First, 0.0, (24, 24)).unwrap();
        let left = render_package_scene_clip(&package.package, SceneClip::Index(0), 0.0, (24, 24))
            .unwrap();
        let right = render_package_scene_clip(&package.package, SceneClip::Index(1), 0.0, (24, 24))
            .unwrap();
        let still =
            render_package_scene_clip(&package.package, SceneClip::Static, 0.0, (24, 24)).unwrap();
        // First == the first animation; the two clips and the static pose differ.
        assert_eq!(first.rgb565, left.rgb565);
        assert_ne!(
            left.rgb565, right.rgb565,
            "different clips pose differently"
        );
        assert_ne!(still.rgb565, right.rgb565);
        // An out-of-range index has no animation: it renders the static pose.
        let oob = render_package_scene_clip(&package.package, SceneClip::Index(9), 0.0, (24, 24))
            .unwrap();
        assert_eq!(oob.rgb565, still.rgb565);
    }

    #[test]
    fn nearer_mesh_is_drawn_over_a_farther_one() {
        let mesh = |name: &str, material: usize| mdc_model::Mesh {
            name: name.into(),
            vertices: 4,
            indices: 6,
            material: Some(material),
            geometry: Some(quad_geometry()),
            ..mdc_model::Mesh::default()
        };
        // The NEAR node is listed first; without depth ordering the far node
        // would draw last and win. Painter order must put the near node on top.
        let model = Model {
            source: "test".into(),
            nodes: vec![
                Node {
                    name: "near".into(),
                    mesh: Some(0),
                    translation: [0.0, 0.0, 0.0],
                    ..Node::default()
                },
                Node {
                    name: "far".into(),
                    mesh: Some(1),
                    translation: [0.0, 0.0, 1.0],
                    ..Node::default()
                },
            ],
            meshes: vec![mesh("near", 0), mesh("far", 1)],
            materials: vec![
                mdc_model::Material {
                    name: "red".into(),
                    color: Some([255, 0, 0]),
                    ..mdc_model::Material::default()
                },
                mdc_model::Material {
                    name: "blue".into(),
                    color: Some([0, 0, 255]),
                    ..mdc_model::Material::default()
                },
            ],
            ..Model::default()
        };
        let frame = render_scene_at(&model, 0, 0.0, (16, 16), None, 0xFFFF).unwrap();
        let at = |x: usize, y: usize| {
            u16::from_le_bytes([
                frame.rgb565[(y * 16 + x) * 2],
                frame.rgb565[(y * 16 + x) * 2 + 1],
            ])
        };
        assert_eq!(
            at(8, 8),
            0xF800,
            "the near (red) mesh over the far (blue) one"
        );
    }

    #[test]
    fn faces_tilted_away_from_the_light_are_shaded() {
        let mesh = |normals: Vec<[f32; 3]>| mdc_model::Mesh {
            name: "q".into(),
            vertices: 4,
            indices: 6,
            material: Some(0),
            geometry: Some(mdc_model::MeshGeometry {
                normals,
                ..quad_geometry()
            }),
            ..mdc_model::Mesh::default()
        };
        let material = mdc_model::Material {
            name: "red".into(),
            color: Some([255, 0, 0]),
            ..mdc_model::Material::default()
        };
        let render = |normals: Vec<[f32; 3]>| {
            let model = Model {
                source: "t".into(),
                nodes: vec![Node {
                    name: "n".into(),
                    mesh: Some(0),
                    ..Node::default()
                }],
                meshes: vec![mesh(normals)],
                materials: vec![material.clone()],
                ..Model::default()
            };
            render_scene_at(&model, 0, 0.0, (16, 16), None, 0xFFFF).unwrap()
        };
        let px = |f: &BakedTexture| {
            u16::from_le_bytes([f.rgb565[(8 * 16 + 8) * 2], f.rgb565[(8 * 16 + 8) * 2 + 1]])
        };
        // No normals -> geometric +z normal, facing the light: unshaded.
        let flat = px(&render(Vec::new()));
        assert_eq!(flat, 0xF800);
        // Explicit normals along +x graze the light: the surface is shaded down.
        let shaded = px(&render(vec![[1.0, 0.0, 0.0]; 4]));
        assert_ne!(shaded, 0xF800, "the side-facing surface is shaded");
        assert!(
            shaded >> 11 < flat >> 11,
            "the red channel is reduced by shading"
        );
    }

    #[test]
    fn directional_light_darkens_a_facing_surface() {
        // N385: a light travelling along +z leaves the +z-facing quad unlit.
        let render = |lights: Vec<mdc_model::Light>| {
            let model = Model {
                source: "t".into(),
                nodes: vec![Node {
                    name: "n".into(),
                    mesh: Some(0),
                    ..Node::default()
                }],
                meshes: vec![mdc_model::Mesh {
                    name: "q".into(),
                    vertices: 4,
                    indices: 6,
                    material: Some(0),
                    geometry: Some(mdc_model::MeshGeometry {
                        normals: vec![[0.0, 0.0, 1.0]; 4],
                        ..quad_geometry()
                    }),
                    ..mdc_model::Mesh::default()
                }],
                materials: vec![mdc_model::Material {
                    name: "m".into(),
                    color: Some([255, 255, 255]),
                    ..mdc_model::Material::default()
                }],
                lights,
                ..Model::default()
            };
            render_scene_at(&model, 0, 0.0, (16, 16), None, 0x0000).unwrap()
        };
        let at = |frame: &BakedTexture| {
            u16::from_le_bytes([
                frame.rgb565[(8 * 16 + 8) * 2],
                frame.rgb565[(8 * 16 + 8) * 2 + 1],
            ])
        };
        let default_light = render(Vec::new());
        let backlight = render(vec![mdc_model::Light {
            name: "back".into(),
            kind: "directional".into(),
            color: [255, 255, 255],
            intensity: 1.0,
            direction: [0.0, 0.0, 1.0],
        }]);
        assert!(
            at(&default_light) >> 11 > at(&backlight) >> 11,
            "backlight darker"
        );
    }

    #[test]
    fn eight_influence_skinning_uses_both_sets() {
        // N384: the second joint/weight set contributes to the same vertex.
        let skeleton = mdc_model::Skeleton {
            name: "s".into(),
            bones: 2,
            joint_nodes: vec![0, 1],
            inverse_bind: vec![mat4_identity(), mat4_identity()],
        };
        let worlds = vec![
            mat4_identity(),
            mat4_from_trs([10.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0]),
        ];
        let out = skin_arrays_multi(
            &[[0, 0, 0, 0]],
            &[[0.5, 0.0, 0.0, 0.0]],
            &[[1, 0, 0, 0]],
            &[[0.5, 0.0, 0.0, 0.0]],
            &[[0.0, 0.0, 0.0]],
            &skeleton,
            &worlds,
        );
        assert!((out[0][0] - 5.0).abs() < 1e-4, "blended: {:?}", out[0]);
    }

    #[test]
    fn material_uv_set_selects_the_second_uv_channel() {
        // N383: uv_set 1 samples TEXCOORD_1 (here the horizontal mirror).
        let texture = crate::TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
        let render = |uv_set: Option<u8>| {
            let mut geometry = quad_geometry();
            geometry.uv2 = geometry.uv.iter().map(|uv| [1.0 - uv[0], uv[1]]).collect();
            let model = Model {
                source: "t".into(),
                nodes: vec![Node {
                    name: "n".into(),
                    mesh: Some(0),
                    ..Node::default()
                }],
                meshes: vec![mdc_model::Mesh {
                    name: "q".into(),
                    vertices: 4,
                    indices: 6,
                    material: Some(0),
                    geometry: Some(geometry),
                    ..mdc_model::Mesh::default()
                }],
                materials: vec![mdc_model::Material {
                    name: "m".into(),
                    texture: Some(0),
                    color: Some([255, 255, 255]),
                    uv_set,
                    ..mdc_model::Material::default()
                }],
                ..Model::default()
            };
            render_scene_at(&model, 0, 0.0, (16, 16), Some(&texture), 0x0000).unwrap()
        };
        let at = |frame: &BakedTexture, x: usize| {
            u16::from_le_bytes([
                frame.rgb565[(8 * 16 + x) * 2],
                frame.rgb565[(8 * 16 + x) * 2 + 1],
            ])
        };
        let first = render(None);
        let second = render(Some(1));
        assert!(
            at(&first, 2) >> 11 > ((at(&first, 2) >> 5) & 0x3f),
            "set 0 left red"
        );
        assert!(
            ((at(&second, 2) >> 5) & 0x3f) > at(&second, 2) >> 11,
            "set 1 left green: {:#06x}",
            at(&second, 2)
        );
    }

    #[test]
    fn material_atlas_pack_remaps_uvs() {
        // N380: two per-material textures collapse into one page and each mesh's
        // UVs move into its packed sub-rect.
        use mdc_model::{Material, MeshGeometry};
        let quad = || MeshGeometry {
            positions: vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uv: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            uv2: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            tangents: Vec::new(),
            skin_joints: Vec::new(),
            skin_joints2: Vec::new(),
            skin_weights2: Vec::new(),
            skin_weights: Vec::new(),
            morph_targets: Vec::new(),
        };
        let model = Model {
            source: "t".into(),
            nodes: vec![
                Node {
                    name: "a".into(),
                    mesh: Some(0),
                    ..Node::default()
                },
                Node {
                    name: "b".into(),
                    mesh: Some(1),
                    ..Node::default()
                },
            ],
            meshes: vec![
                mdc_model::Mesh {
                    name: "a".into(),
                    vertices: 4,
                    indices: 6,
                    material: Some(0),
                    geometry: Some(quad()),
                    ..mdc_model::Mesh::default()
                },
                mdc_model::Mesh {
                    name: "b".into(),
                    vertices: 4,
                    indices: 6,
                    material: Some(1),
                    geometry: Some(quad()),
                    ..mdc_model::Mesh::default()
                },
            ],
            materials: vec![
                Material {
                    name: "a".into(),
                    texture: Some(0),
                    ..Material::default()
                },
                Material {
                    name: "b".into(),
                    texture: Some(1),
                    ..Material::default()
                },
            ],
            ..Model::default()
        };
        let mut package =
            mdc_model::compile_auto(&model, &mdc_model::RendererCapability::full_3d()).unwrap();
        let blob = |rgb565: Vec<u8>| mdc_model::TextureBlob {
            width: 2,
            height: 1,
            rgb565,
            alpha: Vec::new(),
        };
        package.texture_blobs = vec![
            blob(vec![0x00, 0xF8, 0xE0, 0x07]), // red, green
            blob(vec![0x1F, 0x00, 0xFF, 0xFF]), // blue, white
        ];
        assert!(pack_material_atlas(&mut package, 64));
        assert_eq!(package.texture_blobs.len(), 1, "one page");
        assert!(package.materials.iter().all(|m| m.texture == Some(0)));
        let uv0 = package.meshes[0].geometry.as_ref().unwrap().uv[1];
        let uv1 = package.meshes[1].geometry.as_ref().unwrap().uv[1];
        assert_ne!(uv0, uv1, "each mesh samples its own packed sub-rect");
        assert!(uv0[0] < 1.0 && uv1[0] <= 1.0 && uv0[0] >= 0.0 && uv1[1] >= 0.0);
    }

    #[test]
    fn uv_transform_offsets_texture_sampling() {
        // N373: KHR_texture_transform shifts which texel each pixel samples.
        let texture = crate::TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
        let render = |uv_transform: Option<mdc_model::UvTransform>| {
            let model = Model {
                source: "t".into(),
                nodes: vec![Node {
                    name: "n".into(),
                    mesh: Some(0),
                    ..Node::default()
                }],
                meshes: vec![mdc_model::Mesh {
                    name: "q".into(),
                    vertices: 4,
                    indices: 6,
                    material: Some(0),
                    geometry: Some(quad_geometry()),
                    ..mdc_model::Mesh::default()
                }],
                materials: vec![mdc_model::Material {
                    name: "m".into(),
                    texture: Some(0),
                    color: Some([255, 255, 255]),
                    uv_transform,
                    ..mdc_model::Material::default()
                }],
                ..Model::default()
            };
            render_scene_at(&model, 0, 0.0, (16, 16), Some(&texture), 0x0000).unwrap()
        };
        let at = |frame: &BakedTexture, x: usize| {
            u16::from_le_bytes([
                frame.rgb565[(8 * 16 + x) * 2],
                frame.rgb565[(8 * 16 + x) * 2 + 1],
            ])
        };
        let plain = render(None);
        let shifted = render(Some(mdc_model::UvTransform {
            offset: [0.5, 0.0],
            rotation: 0.0,
            scale: [1.0, 1.0],
        }));
        // No transform: the left is the red texel. Offset 0.5: it becomes green.
        assert!(
            at(&plain, 2) >> 11 > ((at(&plain, 2) >> 5) & 0x3f),
            "left red: {:#06x}",
            at(&plain, 2)
        );
        assert!(
            ((at(&shifted, 2) >> 5) & 0x3f) > at(&shifted, 2) >> 11,
            "left green: {:#06x}",
            at(&shifted, 2)
        );
    }

    #[test]
    fn skin_normals_rotate_with_the_joint() {
        let skeleton = mdc_model::Skeleton {
            name: "s".into(),
            bones: 1,
            joint_nodes: vec![0],
            inverse_bind: vec![mat4_identity()],
        };
        let s = std::f32::consts::FRAC_1_SQRT_2;
        // A 90° rotation about X.
        let world = mat4_from_trs([0.0, 0.0, 0.0], [s, 0.0, 0.0, s], [1.0, 1.0, 1.0]);
        let out = skin_normals(
            &[[0, 0, 0, 0]],
            &[[1.0, 0.0, 0.0, 0.0]],
            &[[0.0, 0.0, 1.0]],
            &skeleton,
            &[world],
        );
        assert!(
            out[0][2].abs() < 1e-3 && out[0][1].abs() > 0.9,
            "normal tilted to y: {:?}",
            out[0]
        );
    }

    #[test]
    fn vertex_colours_multiply_the_fill() {
        let mut geometry = quad_geometry();
        geometry.colors = vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
        ];
        let model = Model {
            source: "t".into(),
            nodes: vec![Node {
                name: "n".into(),
                mesh: Some(0),
                ..Node::default()
            }],
            meshes: vec![mdc_model::Mesh {
                name: "q".into(),
                vertices: 4,
                indices: 6,
                material: Some(0),
                geometry: Some(geometry),
                ..mdc_model::Mesh::default()
            }],
            materials: vec![mdc_model::Material {
                name: "white".into(),
                color: Some([255, 255, 255]),
                ..mdc_model::Material::default()
            }],
            ..Model::default()
        };
        let frame = render_scene_at(&model, 0, 0.0, (16, 16), None, 0xFFFF).unwrap();
        let at = |x: usize| {
            u16::from_le_bytes([
                frame.rgb565[(8 * 16 + x) * 2],
                frame.rgb565[(8 * 16 + x) * 2 + 1],
            ])
        };
        let left = at(2);
        let right = at(13);
        assert!(left >> 11 > (left & 0x1f), "left is red: {left:#06x}");
        assert!(
            (right & 0x1f) > (right >> 11),
            "right is blue: {right:#06x}"
        );
    }

    #[test]
    fn unlit_material_ignores_shading() {
        let render = |unlit: bool| {
            let model = Model {
                source: "t".into(),
                nodes: vec![Node {
                    name: "n".into(),
                    mesh: Some(0),
                    ..Node::default()
                }],
                meshes: vec![mdc_model::Mesh {
                    name: "q".into(),
                    vertices: 4,
                    indices: 6,
                    material: Some(0),
                    geometry: Some(mdc_model::MeshGeometry {
                        normals: vec![[1.0, 0.0, 0.0]; 4],
                        ..quad_geometry()
                    }),
                    ..mdc_model::Mesh::default()
                }],
                materials: vec![mdc_model::Material {
                    name: "m".into(),
                    color: Some([255, 0, 0]),
                    unlit: Some(unlit),
                    ..mdc_model::Material::default()
                }],
                ..Model::default()
            };
            render_scene_at(&model, 0, 0.0, (16, 16), None, 0x0000).unwrap()
        };
        let px = |f: &BakedTexture| {
            u16::from_le_bytes([f.rgb565[(8 * 16 + 8) * 2], f.rgb565[(8 * 16 + 8) * 2 + 1]])
        };
        let lit = px(&render(false));
        let unlit = px(&render(true));
        assert!(unlit >> 11 > lit >> 11, "unlit brighter: {unlit:#06x}");
        assert_eq!(unlit, 0xF800, "unlit red is unshaded");
    }

    #[test]
    fn multi_clip_atlas_package_has_one_clip_per_animation() {
        let mut model = two_mesh_model();
        model.animations = vec![
            Animation {
                name: "idle".into(),
                channels: 1,
                tracks: vec![AnimationTrack {
                    node: 1,
                    path: "translation".into(),
                    interpolation: "LINEAR".into(),
                    times: vec![0.0, 1.0],
                    values: vec![-0.4, 0.0, 0.0, -0.4, 0.0, 0.0],
                }],
            },
            Animation {
                name: "walk".into(),
                channels: 1,
                tracks: vec![AnimationTrack {
                    node: 1,
                    path: "translation".into(),
                    interpolation: "LINEAR".into(),
                    times: vec![0.0, 1.0],
                    values: vec![0.4, 0.0, 0.0, 0.4, 0.0, 0.0],
                }],
            },
        ];
        let package =
            bake_scene_clips_to_package(&model, &[(0, 2), (1, 2)], (16, 16), None, 0xFFFF).unwrap();
        let atlas = package.package.atlas.as_ref().expect("atlas");
        assert_eq!(atlas.clips.len(), 2);
        assert_eq!(atlas.clips[0].name, "idle");
        assert_eq!(atlas.clips[0].frames, 2);
        assert_eq!(atlas.clips[1].name, "walk");
        assert_eq!(atlas.clips[1].start, 2);
        assert_eq!(atlas.frames, 4);
    }

    #[test]
    fn package_carries_per_material_textures() {
        let png = |color: [u8; 3]| {
            let image = image::RgbImage::from_pixel(1, 1, image::Rgb(color));
            let mut out = Vec::new();
            image
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            out
        };
        let red = png([255, 0, 0]);
        let blue = png([0, 0, 255]);
        let mut model = two_mesh_model();
        for (index, material) in model.materials.iter_mut().enumerate() {
            material.color = Some([255, 255, 255]);
            material.texture = Some(index);
        }
        model.textures = vec![
            mdc_model::Texture {
                name: "red".into(),
                width: 1,
                height: 1,
                format: "image/png".into(),
                bytes: red.len() as u32,
                data: Some(red),
                wrap: None,
            },
            mdc_model::Texture {
                name: "blue".into(),
                width: 1,
                height: 1,
                format: "image/png".into(),
                bytes: blue.len() as u32,
                data: Some(blue),
                wrap: None,
            },
        ];
        let blobs = crate::attach_model_textures(&model, &crate::BakeLimits::default());
        assert_eq!(blobs.len(), 2);
        let package = crate::mesh_package_with_textures(
            &model,
            &mdc_model::RendererCapability::full_3d(),
            blobs,
        )
        .unwrap();
        assert_eq!(package.package.texture_blobs.len(), 2);
        let frame =
            render_package_scene_clip(&package.package, SceneClip::Static, 0.0, (32, 16)).unwrap();
        let at = |x: usize| {
            u16::from_le_bytes([
                frame.rgb565[(8 * 32 + x) * 2],
                frame.rgb565[(8 * 32 + x) * 2 + 1],
            ])
        };
        let left = at(6);
        let right = at(26);
        assert!(
            left >> 11 > 20 && (left & 0x1f) < 8,
            "left red: {left:#06x}"
        );
        assert!(
            (right >> 11) < 8 && (right & 0x1f) > 20,
            "right blue: {right:#06x}"
        );
    }

    #[test]
    fn roughness_raises_the_ambient_floor() {
        let render = |roughness: f32| {
            let model = Model {
                source: "t".into(),
                nodes: vec![Node {
                    name: "n".into(),
                    mesh: Some(0),
                    ..Node::default()
                }],
                meshes: vec![mdc_model::Mesh {
                    name: "q".into(),
                    vertices: 4,
                    indices: 6,
                    material: Some(0),
                    geometry: Some(mdc_model::MeshGeometry {
                        normals: vec![[1.0, 0.0, 0.0]; 4],
                        ..quad_geometry()
                    }),
                    ..mdc_model::Mesh::default()
                }],
                materials: vec![mdc_model::Material {
                    name: "m".into(),
                    color: Some([255, 0, 0]),
                    roughness: Some(roughness),
                    ..mdc_model::Material::default()
                }],
                ..Model::default()
            };
            render_scene_at(&model, 0, 0.0, (16, 16), None, 0x0000).unwrap()
        };
        let px = |f: &BakedTexture| {
            u16::from_le_bytes([f.rgb565[(8 * 16 + 8) * 2], f.rgb565[(8 * 16 + 8) * 2 + 1]])
        };
        // A grazing normal: a rough material is flatter (brighter) than a smooth one.
        let smooth = px(&render(0.0));
        let rough = px(&render(1.0));
        assert!(
            rough >> 11 > smooth >> 11,
            "rough {rough:#06x} brighter than smooth {smooth:#06x}"
        );
    }

    #[test]
    fn blended_material_is_translucent() {
        let render = |mode: &str| {
            let model = Model {
                source: "t".into(),
                nodes: vec![Node {
                    name: "n".into(),
                    mesh: Some(0),
                    ..Node::default()
                }],
                meshes: vec![mdc_model::Mesh {
                    name: "q".into(),
                    vertices: 4,
                    indices: 6,
                    material: Some(0),
                    geometry: Some(quad_geometry()),
                    ..mdc_model::Mesh::default()
                }],
                materials: vec![mdc_model::Material {
                    name: "half".into(),
                    color: Some([255, 0, 0]),
                    alpha_mode: Some(mode.into()),
                    alpha: Some(0.5),
                    ..mdc_model::Material::default()
                }],
                ..Model::default()
            };
            render_scene_at(&model, 0, 0.0, (16, 16), None, 0x0000).unwrap()
        };
        let px = |f: &BakedTexture| {
            u16::from_le_bytes([f.rgb565[(8 * 16 + 8) * 2], f.rgb565[(8 * 16 + 8) * 2 + 1]])
        };
        // OPAQUE ignores the alpha factor.
        assert_eq!(px(&render("OPAQUE")), 0xF800);
        // BLEND sources 50% red over the black background -> a quarter-bright red.
        let blended = px(&render("BLEND"));
        assert_ne!(blended, 0xF800);
        let red = blended >> 11;
        assert!(red > 0 && red < 0x1f, "half-alpha red channel: {red}");
    }

    #[test]
    fn geometry_morphs_animate_with_the_weights() {
        let mut geometry = quad_geometry();
        geometry.morph_targets = vec![vec![
            [0.5, 0.0, 0.0],
            [0.5, 0.0, 0.0],
            [0.5, 0.0, 0.0],
            [0.5, 0.0, 0.0],
        ]];
        let model = Model {
            source: "test".into(),
            nodes: vec![Node {
                name: "root".into(),
                mesh: Some(0),
                ..Node::default()
            }],
            meshes: vec![mdc_model::Mesh {
                name: "face".into(),
                vertices: 4,
                indices: 6,
                material: Some(0),
                geometry: Some(geometry),
                morph_weights: vec![0.0],
                ..mdc_model::Mesh::default()
            }],
            materials: vec![mdc_model::Material {
                name: "red".into(),
                texture: None,
                color: Some([255, 0, 0]),
                ..mdc_model::Material::default()
            }],
            animations: vec![Animation {
                name: "smile".into(),
                channels: 1,
                tracks: vec![AnimationTrack {
                    node: 0,
                    path: "weights".into(),
                    interpolation: "LINEAR".into(),
                    times: vec![0.0, 1.0],
                    values: vec![0.0, 1.0],
                }],
            }],
            ..Model::default()
        };
        let neutral = render_scene_at(&model, 0, 0.0, (24, 24), None, 0xFFFF).unwrap();
        let smiled = render_scene_at(&model, 0, 1.0, (24, 24), None, 0xFFFF).unwrap();
        assert_ne!(
            neutral.rgb565, smiled.rgb565,
            "geometry morph moved the quad"
        );
    }

    #[test]
    fn cubicspline_uses_in_out_tangents() {
        // One component: [in0, val0, out0, in1, val1, out1] with zero tangents
        // (ease-in-out) over one second.
        let track = AnimationTrack {
            node: 0,
            path: "translation".into(),
            interpolation: "CUBICSPLINE".into(),
            times: vec![0.0, 1.0],
            values: vec![0.0, 0.0, 0.0, 0.0, 10.0, 0.0],
        };
        assert_eq!(sample_channel(&track, 1, 0.0), vec![0.0]);
        assert_eq!(sample_channel(&track, 1, 1.0), vec![10.0]);
        assert_eq!(sample_channel(&track, 1, 0.5), vec![5.0]);
        // Ease-in: the quarter point is below the linear value.
        let quarter = sample_channel(&track, 1, 0.25)[0];
        assert!(quarter < 2.5, "ease-in quarter {quarter}");
    }

    #[test]
    fn looped_sampling_wraps_into_the_span() {
        let track = AnimationTrack {
            node: 0,
            path: "translation".into(),
            interpolation: "LINEAR".into(),
            times: vec![0.0, 1.0],
            values: vec![0.0, 10.0],
        };
        // 2.5s wraps to 0.5s.
        assert_eq!(
            sample_channel_looped(&track, 1, 2.5, 0.0, 1.0, true),
            vec![5.0]
        );
        // Not looping: clamped to the last value.
        assert_eq!(
            sample_channel_looped(&track, 1, 2.5, 0.0, 1.0, false),
            vec![10.0]
        );
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
