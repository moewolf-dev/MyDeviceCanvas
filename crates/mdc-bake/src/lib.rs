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
        return Err(BakeError::InputTooLarge(bytes.len(), limits.max_input_bytes));
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
    let resized = image::imageops::resize(
        &image.to_rgb8(),
        target.0,
        target.1,
        FilterType::Triangle,
    );

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
}
