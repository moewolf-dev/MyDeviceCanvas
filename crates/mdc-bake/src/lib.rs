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

/// Pack one RGB888 pixel into RGB565.
pub fn rgb565_of(r: u8, g: u8, b: u8) -> u16 {
    (u16::from(r >> 3) << 11) | (u16::from(g >> 2) << 5) | u16::from(b >> 3)
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
    let manifest = BakedManifest {
        atlas_bytes: atlas.rgb565.len() as u64,
        plan,
    };
    Ok(BakedSpritePackage { atlas, manifest })
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
    fn malformed_input_is_a_decode_error() {
        assert!(matches!(
            decode_resize_rgb565(b"not an image", (4, 4), &BakeLimits::default()),
            Err(BakeError::Decode(_))
        ));
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
