//! `mdc-bake` — host-side pixel baking (N49–N52).
//!
//! The model compiler works on metadata; this crate does the actual pixels:
//! decode a source texture (PNG/JPEG), resize it to the device target, and emit
//! tightly packed RGB565 little-endian rows. `AtlasPacker` then lays baked
//! frames onto the grid an `mdc_model::AtlasPlan` describes.
//!
//! Like the importer, it reads the declared dimensions **before** decoding, so a
//! decompression bomb is refused before any pixel buffer is allocated.
use image::imageops::FilterType;
use image::io::Reader as ImageReader;
pub use mdc_model::AtlasPlan;
use std::io::Cursor;
use thiserror::Error;

mod skin;
mod texture;
pub use skin::{
    animation_span, apply_morphs, bake_geometry_frames, bake_geometry_to_package,
    bake_scene_frames, bake_scene_to_package, bake_skinned_mesh, bake_skinned_to_package,
    bake_textured_mesh, bake_textured_to_package, mat4_from_trs, mat4_identity, mat4_mul,
    morph_weights_at, node_world_matrices, pose_world_matrices, render_frame_at,
    render_geometry_at, render_package_mesh, render_package_scene, render_package_scene_clip,
    render_scene_at, sample_channel, sample_channel_looped, skin_arrays, skin_points, skin_preview,
    transform_point, Mat4, SceneClip,
};
pub use texture::{Filter, TextureMap};

/// Largest source file accepted (8 MiB) before decoding.
pub const MAX_BAKE_INPUT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BakeError {
    #[error("source is {0} bytes, over the {1}-byte limit")]
    InputTooLarge(usize, usize),
    #[error("source dimensions {0}x{1} over the {2}x{2} limit")]
    Dimensions(u32, u32, u32),
    #[error("target {0}x{1} exceeds the byte budget")]
    Budget(u32, u32),
    #[error("decode failed: {0}")]
    Decode(String),
    #[error("expected {0} frames, got {1}")]
    FrameCount(usize, usize),
    #[error("a frame is {0}x{1}, expected {2}x{2}")]
    FrameSize(u32, u32, u32, u32),
    #[error("package: {0}")]
    Package(String),
}

/// Bake limits: input, source dimensions and output budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BakeLimits {
    pub max_input_bytes: usize,
    pub max_source_dimension: u32,
    pub max_output_bytes: u64,
}
impl Default for BakeLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: MAX_BAKE_INPUT_BYTES,
            max_source_dimension: 8192,
            max_output_bytes: 32 * 1024 * 1024,
        }
    }
}

/// A decoded, resized texture as RGB565 little-endian rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BakedTexture {
    pub width: u32,
    pub height: u32,
    pub rgb565: Vec<u8>,
}

/// Decode a PNG/JPEG and resize it to `target`, returning RGB565 rows.
pub fn decode_resize_rgb565(
    bytes: &[u8],
    target: (u32, u32),
    limits: &BakeLimits,
) -> Result<BakedTexture, BakeError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(BakeError::InputTooLarge(
            bytes.len(),
            limits.max_input_bytes,
        ));
    }
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| BakeError::Decode(e.to_string()))?;
    // Read the header first: a bomb is refused before a pixel buffer exists.
    let (source_w, source_h) = reader
        .into_dimensions()
        .map_err(|e| BakeError::Decode(e.to_string()))?;
    let limit = limits.max_source_dimension;
    if source_w > limit || source_h > limit {
        return Err(BakeError::Dimensions(source_w, source_h, limit));
    }
    let output_bytes = u64::from(target.0)
        .saturating_mul(u64::from(target.1))
        .saturating_mul(2);
    if target.0 == 0 || target.1 == 0 || output_bytes > limits.max_output_bytes {
        return Err(BakeError::Budget(target.0, target.1));
    }

    let image = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| BakeError::Decode(e.to_string()))?
        .decode()
        .map_err(|e| BakeError::Decode(e.to_string()))?;
    let resized =
        image::imageops::resize(&image.to_rgb8(), target.0, target.1, FilterType::Triangle);

    let mut rgb565 = Vec::with_capacity(output_bytes as usize);
    for pixel in resized.pixels() {
        let value = rgb565_of(pixel[0], pixel[1], pixel[2]);
        rgb565.push((value & 0xff) as u8);
        rgb565.push((value >> 8) as u8);
    }
    Ok(BakedTexture {
        width: target.0,
        height: target.1,
        rgb565,
    })
}

/// Decode a PNG/JPEG at its native dimensions into RGB565 rows. Used to load a
/// texture map for the textured bake.
pub fn decode_rgb565_native(bytes: &[u8], limits: &BakeLimits) -> Result<BakedTexture, BakeError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(BakeError::InputTooLarge(
            bytes.len(),
            limits.max_input_bytes,
        ));
    }
    let (width, height) = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| BakeError::Decode(e.to_string()))?
        .into_dimensions()
        .map_err(|e| BakeError::Decode(e.to_string()))?;
    decode_resize_rgb565(bytes, (width, height), limits)
}

/// N284: decode a PNG/JPEG with alpha at native size, returning RGB565 rows and
/// a bit-packed alpha mask (1 = opaque). Reads the header first (bomb guard).
pub fn decode_rgba_native(
    bytes: &[u8],
    limits: &BakeLimits,
) -> Result<(BakedTexture, Vec<u8>), BakeError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(BakeError::InputTooLarge(
            bytes.len(),
            limits.max_input_bytes,
        ));
    }
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| BakeError::Decode(e.to_string()))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| BakeError::Decode(e.to_string()))?;
    let limit = limits.max_source_dimension;
    if width > limit || height > limit {
        return Err(BakeError::Dimensions(width, height, limit));
    }
    let output_bytes = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(2);
    if width == 0 || height == 0 || output_bytes > limits.max_output_bytes {
        return Err(BakeError::Budget(width, height));
    }
    let image = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| BakeError::Decode(e.to_string()))?
        .decode()
        .map_err(|e| BakeError::Decode(e.to_string()))?;
    let rgba = image.to_rgba8();
    let mut rgb565 = Vec::with_capacity(output_bytes as usize);
    let mut alpha = vec![0u8; (width as usize * height as usize).div_ceil(8)];
    for (index, pixel) in rgba.pixels().enumerate() {
        let value = rgb565_of(pixel[0], pixel[1], pixel[2]);
        rgb565.push((value & 0xff) as u8);
        rgb565.push((value >> 8) as u8);
        if pixel[3] >= 128 {
            alpha[index / 8] |= 1 << (index % 8);
        }
    }
    Ok((
        BakedTexture {
            width,
            height,
            rgb565,
        },
        alpha,
    ))
}

/// N284: build a `TextureMap` (with a 1-bit alpha mask) from an image.
pub fn texture_map_with_alpha(bytes: &[u8], limits: &BakeLimits) -> Result<TextureMap, BakeError> {
    let (baked, alpha) = decode_rgba_native(bytes, limits)?;
    Ok(TextureMap::from_baked(&baked)?.with_alpha(alpha))
}

/// Pack one RGB888 pixel into RGB565.
pub fn rgb565_of(r: u8, g: u8, b: u8) -> u16 {
    (u16::from(r >> 3) << 11) | (u16::from(g >> 2) << 5) | u16::from(b >> 3)
}

/// Raster a triangle fan into a tightly packed RGB565 framebuffer.
/// `points[0]` is the fan origin. At most 64 points; surface at most 1024².
pub fn raster_triangle_fan(
    points: &[(i16, i16)],
    width: u32,
    height: u32,
    color: u16,
) -> Result<Vec<u8>, BakeError> {
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return Err(BakeError::Budget(width, height));
    }
    if points.len() > 64 {
        return Err(BakeError::FrameCount(64, points.len()));
    }
    let mut pixels = vec![0u8; (width as usize) * (height as usize) * 2];
    if points.len() >= 3 {
        let origin = points[0];
        for window in points[1..].windows(2) {
            fill_triangle(
                &mut pixels,
                width,
                height,
                origin,
                window[0],
                window[1],
                color,
            );
        }
    }
    Ok(pixels)
}

/// Draw `points` into a fresh RGB565 framebuffer, either as the triangle list
/// `indices` names or, when `indices` is empty, as a triangle fan.
pub(crate) fn raster_points(
    points: &[(i16, i16)],
    indices: &[u16],
    width: u32,
    height: u32,
    color: u16,
) -> Result<Vec<u8>, BakeError> {
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return Err(BakeError::Budget(width, height));
    }
    if points.len() > 64 {
        return Err(BakeError::FrameCount(64, points.len()));
    }
    let mut pixels = vec![0u8; (width as usize) * (height as usize) * 2];
    if indices.len() >= 3 {
        for triangle in indices.chunks_exact(3) {
            let (Some(a), Some(b), Some(c)) = (
                points.get(triangle[0] as usize).copied(),
                points.get(triangle[1] as usize).copied(),
                points.get(triangle[2] as usize).copied(),
            ) else {
                continue;
            };
            fill_triangle(&mut pixels, width, height, a, b, c, color);
        }
    } else if points.len() >= 3 {
        let origin = points[0];
        for window in points[1..].windows(2) {
            fill_triangle(
                &mut pixels,
                width,
                height,
                origin,
                window[0],
                window[1],
                color,
            );
        }
    }
    Ok(pixels)
}

/// Sample one rigid motion of a mesh preview. Frame `i` shifts the triangle by
/// `i` pixels on X, so consecutive frames are not copies of one source image.
/// This is not skeletal skinning; see [`bake_skinned_mesh`]. At most 64 frames.
pub fn bake_displaced_mesh(
    points: &[(i16, i16)],
    frame_count: u32,
    sprite: (u32, u32),
    color: u16,
) -> Result<Vec<BakedTexture>, BakeError> {
    if frame_count == 0 || frame_count > 64 {
        return Err(BakeError::FrameCount(frame_count as usize, 64));
    }
    if points.len() < 3 {
        return Err(BakeError::Decode("mesh preview needs three points".into()));
    }
    let mut frames = Vec::with_capacity(frame_count as usize);
    for index in 0..frame_count {
        let shifted: Vec<(i16, i16)> = points
            .iter()
            .map(|(x, y)| (x.saturating_add(index as i16), *y))
            .collect();
        let rgb565 = raster_triangle_fan(&shifted, sprite.0, sprite.1, color)?;
        frames.push(BakedTexture {
            width: sprite.0,
            height: sprite.1,
            rgb565,
        });
    }
    Ok(frames)
}

fn fill_triangle(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    a: (i16, i16),
    b: (i16, i16),
    c: (i16, i16),
    color: u16,
) {
    let low = (color & 0xff) as u8;
    let high = (color >> 8) as u8;
    let min_x = a.0.min(b.0).min(c.0).max(0) as u32;
    let max_x = a.0.max(b.0).max(c.0).max(0) as u32;
    let min_y = a.1.min(b.1).min(c.1).max(0) as u32;
    let max_y = a.1.max(b.1).max(c.1).max(0) as u32;
    let max_x = max_x.min(width.saturating_sub(1));
    let max_y = max_y.min(height.saturating_sub(1));
    let area = edge(a, b, c);
    if area == 0 {
        return;
    }
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let p = (x as i16, y as i16);
            let w0 = edge(b, c, p);
            let w1 = edge(c, a, p);
            let w2 = edge(a, b, p);
            let inside = if area > 0 {
                w0 >= 0 && w1 >= 0 && w2 >= 0
            } else {
                w0 <= 0 && w1 <= 0 && w2 <= 0
            };
            if !inside {
                continue;
            }
            let index = ((y * width + x) * 2) as usize;
            if index + 1 < pixels.len() {
                pixels[index] = low;
                pixels[index + 1] = high;
            }
        }
    }
}

fn edge(a: (i16, i16), b: (i16, i16), p: (i16, i16)) -> i32 {
    (i32::from(p.0) - i32::from(a.0)) * (i32::from(b.1) - i32::from(a.1))
        - (i32::from(p.1) - i32::from(a.1)) * (i32::from(b.0) - i32::from(a.0))
}

/// Mix `src` into `dst` by `quarters` / 4 (0 keeps dst, 4 replaces with src).
pub fn mix_rgb565(dst: &mut [u8], src: &[u8], quarters: u8) {
    let q = u32::from(quarters.min(4));
    let keep = 4 - q;
    let pixels = dst.len().min(src.len()) / 2;
    for i in 0..pixels {
        let d = u16::from_le_bytes([dst[i * 2], dst[i * 2 + 1]]);
        let s = u16::from_le_bytes([src[i * 2], src[i * 2 + 1]]);
        let channel = |shift: u32, mask: u16| {
            let dv = u32::from((d >> shift) & mask);
            let sv = u32::from((s >> shift) & mask);
            (((dv * keep + sv * q) / 4) as u16) << shift
        };
        let mixed = channel(11, 0x1f) | channel(5, 0x3f) | channel(0, 0x1f);
        let bytes = mixed.to_le_bytes();
        dst[i * 2] = bytes[0];
        dst[i * 2 + 1] = bytes[1];
    }
}

/// An atlas of baked frames laid out on an `mdc_model::AtlasPlan` grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BakedAtlas {
    pub width: u32,
    pub height: u32,
    pub sprite_width: u32,
    pub sprite_height: u32,
    pub frames: u32,
    pub columns: u32,
    pub rows: u32,
    /// RGB565 little-endian atlas pixels.
    pub rgb565: Vec<u8>,
    /// N70: playable clips (one per motion).
    pub clips: Vec<mdc_model::AtlasClip>,
}

impl BakedAtlas {
    /// Byte offset of the top-left pixel of frame `index`, or `None` when out of
    /// range. Frames fill the grid left-to-right, top-to-bottom.
    pub fn frame_offset(&self, index: u32) -> Option<usize> {
        if index >= self.frames {
            return None;
        }
        let column = (index % self.columns) as usize;
        let row = (index / self.columns) as usize;
        let pixel = row * self.sprite_height as usize * self.width as usize
            + column * self.sprite_width as usize;
        Some(pixel * 2)
    }
}

/// Lay `frames` onto the grid `plan` describes. Every frame must match the
/// sprite size and the count must match the plan.
pub fn pack(plan: &mdc_model::AtlasPlan, frames: &[BakedTexture]) -> Result<BakedAtlas, BakeError> {
    if frames.len() != plan.frames as usize {
        return Err(BakeError::FrameCount(plan.frames as usize, frames.len()));
    }
    for frame in frames {
        if frame.width != plan.sprite_width || frame.height != plan.sprite_height {
            return Err(BakeError::FrameSize(
                frame.width,
                frame.height,
                plan.sprite_width,
                plan.sprite_height,
            ));
        }
    }
    let width = plan.columns.saturating_mul(plan.sprite_width);
    let height = plan.rows.saturating_mul(plan.sprite_height);
    let row_bytes = (plan.sprite_width as usize) * 2;
    let mut rgb565 = vec![0u8; (width as usize) * (height as usize) * 2];
    for (index, frame) in frames.iter().enumerate() {
        let column = index as u32 % plan.columns;
        let row = index as u32 / plan.columns;
        for y in 0..plan.sprite_height as usize {
            let src = y * row_bytes;
            let dst = ((row as usize) * plan.sprite_height as usize + y) * (width as usize) * 2
                + (column as usize) * row_bytes;
            rgb565[dst..dst + row_bytes].copy_from_slice(&frame.rgb565[src..src + row_bytes]);
        }
    }
    Ok(BakedAtlas {
        width,
        height,
        sprite_width: plan.sprite_width,
        sprite_height: plan.sprite_height,
        frames: plan.frames,
        columns: plan.columns,
        rows: plan.rows,
        rgb565,
        clips: Vec::new(),
    })
}

use serde::{Deserialize, Serialize};

/// One source image available for baking, by name.
#[derive(Debug, Clone)]
pub struct SpriteSource {
    pub name: String,
    pub image: Vec<u8>,
}

/// Serializable manifest for a baked atlas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BakedManifest {
    pub plan: AtlasPlan,
    pub atlas_bytes: u64,
}

impl BakedManifest {
    pub fn to_json(&self) -> Result<String, BakeError> {
        serde_json::to_string_pretty(self).map_err(|e| BakeError::Decode(e.to_string()))
    }
}

/// A baked atlas plus its manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BakedSpritePackage {
    pub atlas: BakedAtlas,
    pub manifest: BakedManifest,
}

/// Bake every sampled frame into one atlas. Each frame is produced from a source
/// image (cycled when there are fewer sources than frames); the target grid comes
/// from `mdc_model::AtlasBaker` for the device texture limit.
#[allow(clippy::too_many_arguments)]
pub fn bake_sprites(
    sources: &[SpriteSource],
    motions: &[mdc_model::MotionInfo],
    sprite: (u32, u32),
    max_texture: u32,
    limits: &BakeLimits,
) -> Result<BakedSpritePackage, BakeError> {
    if sources.is_empty() {
        return Err(BakeError::Decode("no source images".into()));
    }
    let plan = mdc_model::AtlasBaker::default()
        .bake(sprite, max_texture, motions)
        .map_err(|e| BakeError::Decode(e.to_string()))?;
    let mut frames = Vec::with_capacity(plan.frames as usize);
    for index in 0..plan.frames as usize {
        let source = &sources[index % sources.len()];
        frames.push(decode_resize_rgb565(
            &source.image,
            (plan.sprite_width, plan.sprite_height),
            limits,
        )?);
    }
    let atlas = pack(&plan, &frames)?;
    let clips = plan
        .motions
        .iter()
        .zip(motions.iter())
        .map(|(range, motion)| mdc_model::AtlasClip {
            name: motion.name.clone(),
            start: range.start,
            frames: range.count,
            fps: motion.fps.max(0.0) as u32,
            looping: motion.looping,
            fade_in_ms: (motion.fade_in_sec.max(0.0) * 1000.0) as u32,
            fade_out_ms: (motion.fade_out_sec.max(0.0) * 1000.0) as u32,
        })
        .collect();
    let mut atlas = atlas;
    atlas.clips = clips;
    let manifest = BakedManifest {
        atlas_bytes: atlas.rgb565.len() as u64,
        plan,
    };
    Ok(BakedSpritePackage { atlas, manifest })
}

/// N177/N294: decode an image at native size into a device texture blob, with its
/// 1-bit alpha mask when the source has transparency.
pub fn bake_texture_blob(
    bytes: &[u8],
    limits: &BakeLimits,
) -> Result<mdc_model::TextureBlob, BakeError> {
    let (baked, alpha) = decode_rgba_native(bytes, limits)?;
    let has_transparency = alpha.iter().any(|byte| *byte != 0xff);
    Ok(mdc_model::TextureBlob {
        width: baked.width,
        height: baked.height,
        rgb565: baked.rgb565,
        alpha: if has_transparency { alpha } else { Vec::new() },
    })
}

/// N177: compile a model with the capability-selected backend and attach an
/// optional RGB565 texture for a mesh-capable device.
pub fn mesh_package_with_texture(
    model: &mdc_model::Model,
    caps: &mdc_model::RendererCapability,
    texture: Option<mdc_model::TextureBlob>,
) -> Result<mdc_model::MdcPackage, BakeError> {
    let mut package =
        mdc_model::compile_auto(model, caps).map_err(|e| BakeError::Package(e.to_string()))?;
    if let Some(blob) = texture {
        if !blob.validate() {
            return Err(BakeError::Package(
                "texture pixel size does not match its dimensions".into(),
            ));
        }
        package.texture = Some(blob);
    }
    mdc_model::MdcPackage::seal(package).map_err(|e| BakeError::Package(e.to_string()))
}

/// N324: decode the model's own base-colour texture (the first material that has
/// an embedded one), so a caller does not need a separate `--texture`.
pub fn auto_texture_blob(
    model: &mdc_model::Model,
    limits: &BakeLimits,
) -> Result<Option<mdc_model::TextureBlob>, BakeError> {
    for material in 0..model.materials.len() {
        if let Some(bytes) = model.material_texture(material) {
            return Ok(Some(bake_texture_blob(bytes, limits)?));
        }
    }
    Ok(None)
}

/// N324: the model's own texture or the caller's override. An override always
/// wins; `None` means the model had no usable embedded texture.
pub fn resolve_texture_blob(
    model: &mdc_model::Model,
    override_blob: Option<mdc_model::TextureBlob>,
    limits: &BakeLimits,
) -> Result<Option<mdc_model::TextureBlob>, BakeError> {
    match override_blob {
        Some(blob) => Ok(Some(blob)),
        None => auto_texture_blob(model, limits),
    }
}

/// N60: bake a sprite set straight into a sealed runtime package the device can
/// receive over the `PACKAGE` path.
#[allow(clippy::too_many_arguments)]
pub fn bake_to_package(
    sources: &[SpriteSource],
    motions: &[mdc_model::MotionInfo],
    sprite: (u32, u32),
    max_texture: u32,
    limits: &BakeLimits,
) -> Result<mdc_model::MdcPackage, BakeError> {
    let baked = bake_sprites(sources, motions, sprite, max_texture, limits)?;
    let estimated_bytes = baked.atlas.rgb565.len() as u64;
    let atlas = mdc_model::AtlasBlob {
        sprite_width: baked.atlas.sprite_width,
        sprite_height: baked.atlas.sprite_height,
        frames: baked.atlas.frames,
        columns: baked.atlas.columns,
        rows: baked.atlas.rows,
        rgb565: baked.atlas.rgb565,
        clips: baked.atlas.clips,
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
        estimated_bytes,
        atlas: Some(atlas),
        texture: None,
    };
    mdc_model::MdcPackage::seal(package).map_err(|e| BakeError::Package(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn png_of(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
        let image = RgbImage::from_pixel(width, height, Rgb(color));
        let mut out = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn auto_texture_resolution_prefers_override_then_model() {
        let png = png_of(2, 2, [0, 0, 255]);
        let model = mdc_model::Model {
            materials: vec![mdc_model::Material {
                name: "m".into(),
                texture: Some(0),
                ..Default::default()
            }],
            textures: vec![mdc_model::Texture {
                name: "t".into(),
                width: 2,
                height: 2,
                format: "image/png".into(),
                bytes: png.len() as u32,
                data: Some(png.clone()),
            }],
            ..Default::default()
        };
        // N324: the model's own texture is decoded without a CLI override.
        let auto = auto_texture_blob(&model, &BakeLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!((auto.width, auto.height), (2, 2));
        assert_eq!(&auto.rgb565[0..2], &[0x1F, 0x00]); // blue 0x001F little-endian

        // An override always wins.
        let override_blob =
            bake_texture_blob(&png_of(1, 1, [255, 0, 0]), &BakeLimits::default()).unwrap();
        let chosen = resolve_texture_blob(&model, Some(override_blob), &BakeLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!((chosen.width, chosen.height), (1, 1));

        // No override and no embedded texture -> no texture.
        let empty = mdc_model::Model::default();
        assert!(resolve_texture_blob(&empty, None, &BakeLimits::default())
            .unwrap()
            .is_none());
    }

    #[test]
    fn decodes_resizes_and_packs_rgb565() {
        let png = png_of(8, 8, [255, 0, 0]);
        let baked = decode_resize_rgb565(&png, (4, 4), &BakeLimits::default()).unwrap();
        assert_eq!((baked.width, baked.height), (4, 4));
        assert_eq!(baked.rgb565.len(), 4 * 4 * 2);
        // Pure red packs to 0xF800 little-endian.
        assert_eq!(&baked.rgb565[0..2], &[0x00, 0xF8]);
    }

    #[test]
    fn input_and_output_budgets_are_enforced() {
        let png = png_of(8, 8, [0, 255, 0]);
        let limits = BakeLimits {
            max_input_bytes: 16,
            ..BakeLimits::default()
        };
        assert!(matches!(
            decode_resize_rgb565(&png, (4, 4), &limits),
            Err(BakeError::InputTooLarge(_, 16))
        ));

        let limits = BakeLimits {
            max_output_bytes: 8,
            ..BakeLimits::default()
        };
        assert!(matches!(
            decode_resize_rgb565(&png, (4, 4), &limits),
            Err(BakeError::Budget(_, _))
        ));
    }

    #[test]
    fn oversized_source_dimensions_are_refused_before_decode() {
        let png = png_of(64, 64, [0, 0, 255]);
        let limits = BakeLimits {
            max_source_dimension: 16,
            ..BakeLimits::default()
        };
        assert!(matches!(
            decode_resize_rgb565(&png, (8, 8), &limits),
            Err(BakeError::Dimensions(64, 64, 16))
        ));
    }

    #[test]
    fn bake_to_package_round_trips_as_a_runtime_package() {
        let source = SpriteSource {
            name: "src".into(),
            image: png_of(8, 8, [10, 20, 30]),
        };
        let package = bake_to_package(
            std::slice::from_ref(&source),
            &[motion(0.1, 30.0)],
            (4, 4),
            2048,
            &BakeLimits::default(),
        )
        .unwrap();
        assert_eq!(package.manifest.mode, mdc_model::PackageMode::BakedSprites);
        let bytes = package.to_bytes().unwrap();
        let back = mdc_model::MdcPackage::from_bytes(&bytes).unwrap();
        assert_eq!(back, package);
        let atlas = back.package.atlas.expect("atlas present");
        assert_eq!(atlas.frames, 4);
        assert_eq!((atlas.columns, atlas.rows), (2, 2));
        assert_eq!(atlas.rgb565.len(), 4 * 4 * 4 * 2);
        assert_eq!(atlas.clips.len(), 1);
        assert_eq!(atlas.clips[0].name, "idle");
        assert_eq!(atlas.clips[0].frames, 4);
        assert!(atlas.clips[0].looping);
        assert_eq!(atlas.frame_offset(0), Some(0));
    }

    fn solid(width: u32, height: u32, value: u16) -> BakedTexture {
        let mut rgb565 = Vec::with_capacity((width * height * 2) as usize);
        for _ in 0..width * height {
            rgb565.push((value & 0xff) as u8);
            rgb565.push((value >> 8) as u8);
        }
        BakedTexture {
            width,
            height,
            rgb565,
        }
    }

    fn plan_of(sprite: u32, frames: u32, columns: u32, rows: u32) -> mdc_model::AtlasPlan {
        mdc_model::AtlasPlan {
            sprite_width: sprite,
            sprite_height: sprite,
            frames,
            columns,
            rows,
            total_bytes: u64::from(frames) * u64::from(sprite) * u64::from(sprite) * 2,
            motions: Vec::new(),
        }
    }

    #[test]
    fn packs_frames_left_to_right_top_to_bottom() {
        let plan = plan_of(2, 2, 2, 1);
        let red = solid(2, 2, 0xF800);
        let green = solid(2, 2, 0x07E0);
        let atlas = pack(&plan, &[red, green]).unwrap();
        assert_eq!((atlas.width, atlas.height), (4, 2));
        assert_eq!(atlas.rgb565.len(), 4 * 2 * 2);
        assert_eq!(atlas.frame_offset(0), Some(0));
        assert_eq!(atlas.frame_offset(1), Some(4));
        assert_eq!(atlas.frame_offset(2), None);
        assert_eq!(&atlas.rgb565[0..2], &[0x00, 0xF8]);
        assert_eq!(&atlas.rgb565[4..6], &[0xE0, 0x07]);
    }

    #[test]
    fn frame_count_and_size_mismatches_are_rejected() {
        let plan = plan_of(2, 2, 2, 1);
        assert!(matches!(
            pack(&plan, &[solid(2, 2, 0)]),
            Err(BakeError::FrameCount(2, 1))
        ));
        assert!(matches!(
            pack(&plan, &[solid(4, 4, 0), solid(2, 2, 0)]),
            Err(BakeError::FrameSize(4, 4, 2, 2))
        ));
    }

    fn motion(duration: f64, fps: f64) -> mdc_model::MotionInfo {
        mdc_model::MotionInfo {
            name: "idle".into(),
            channels: 1,
            duration_sec: duration,
            fps,
            fade_in_sec: 0.0,
            fade_out_sec: 0.0,
            looping: true,
            curves: Vec::new(),
        }
    }

    #[test]
    fn bake_sprites_cycles_sources_and_produces_a_manifest() {
        let red = SpriteSource {
            name: "red".into(),
            image: png_of(8, 8, [255, 0, 0]),
        };
        let green = SpriteSource {
            name: "green".into(),
            image: png_of(8, 8, [0, 255, 0]),
        };
        // 0.1s @ 30fps -> 4 frames; sprite 4x4 -> 2x2 grid.
        let package = bake_sprites(
            &[red, green],
            &[motion(0.1, 30.0)],
            (4, 4),
            2048,
            &BakeLimits::default(),
        )
        .unwrap();
        assert_eq!(package.atlas.frames, 4);
        assert_eq!((package.atlas.columns, package.atlas.rows), (2, 2));
        assert_eq!(
            package.manifest.atlas_bytes,
            package.atlas.rgb565.len() as u64
        );
        assert_eq!(package.manifest.plan.frames, 4);
        // Frame 0 is red, frame 1 is green.
        assert_eq!(&package.atlas.rgb565[0..2], &[0x00, 0xF8]);
        assert_eq!(
            &package.atlas.rgb565[package.atlas.frame_offset(1).unwrap()..][..2],
            &[0xE0, 0x07]
        );
        let json = package.manifest.to_json().unwrap();
        assert!(json.contains("\"frames\": 4"));
    }

    #[test]
    fn displaced_mesh_frames_are_not_copies() {
        let frames = bake_displaced_mesh(&[(1, 1), (4, 1), (1, 4)], 3, (8, 8), 0xF800).unwrap();
        assert_eq!(frames.len(), 3);
        assert_ne!(frames[0].rgb565, frames[1].rgb565);
        assert_eq!(&frames[0].rgb565[18..][..2], &[0x00, 0xF8]);
        assert_eq!(&frames[1].rgb565[18..][..2], &[0, 0]);
    }

    #[test]
    fn triangle_fan_covers_the_origin_pixel() {
        let pixels = raster_triangle_fan(&[(1, 1), (6, 1), (1, 6)], 8, 8, 0xF800).unwrap();
        assert_eq!(&pixels[18..][..2], &[0x00, 0xF8]);
        assert_eq!(&pixels[0..2], &[0, 0]);
    }

    #[test]
    fn mix_quarters_blends_red_toward_green() {
        let mut dst = vec![0x00u8, 0xF8];
        mix_rgb565(&mut dst, &[0xE0, 0x07], 2);
        let mixed = u16::from_le_bytes([dst[0], dst[1]]);
        assert!(mixed != 0xF800);
        assert!(mixed != 0x07E0);
    }

    #[test]
    fn bake_sprites_requires_a_source() {
        assert!(matches!(
            bake_sprites(
                &[],
                &[motion(0.1, 30.0)],
                (4, 4),
                2048,
                &BakeLimits::default()
            ),
            Err(BakeError::Decode(_))
        ));
    }
}
