//! Fake device peer for Host Core / protocol integration tests.
use mdc_core::{validate_tile_base, FrameAssembler, Tile, TransactionNack, MAX_FRAME_BYTES};
use mdc_protocol::{
    decode_control, decode_frame_payload, decode_package_payload, decode_tile_payload,
    encode_control, Ack, BootInfo, Capabilities, ConfigCommand, ConfigReply, ErrorPayload,
    FramePayload, Hello, InputEvent, MessageType, MetricReport, MirrorCommand, OtaCommand,
    PackagePayload, Packet, Surface, TelemetryEvent, VERSION,
};
use mdc_transport::{MemoryLink, Transport, TransportError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

/// 5×7 glyphs for digits, A–Z and common punctuation. Columns are the low 7
/// bits; bit 0 is the top row. Lowercase letters render as their uppercase.
fn glyph_columns(ch: u8) -> Option<[u8; 5]> {
    if ch == b' ' {
        return Some([0, 0, 0, 0, 0]);
    }
    // N476: lowercase glyphs are distinct from uppercase.
    if ch.is_ascii_lowercase() {
        const LOWER: [[u8; 5]; 26] = [
            [0x20, 0x54, 0x54, 0x54, 0x78], // a
            [0x7f, 0x48, 0x44, 0x44, 0x38], // b
            [0x38, 0x44, 0x44, 0x44, 0x20], // c
            [0x38, 0x44, 0x44, 0x48, 0x7f], // d
            [0x38, 0x54, 0x54, 0x54, 0x18], // e
            [0x08, 0x7e, 0x09, 0x01, 0x02], // f
            [0x18, 0xa4, 0xa4, 0xa4, 0x7c], // g
            [0x7f, 0x08, 0x04, 0x04, 0x78], // h
            [0x00, 0x44, 0x7d, 0x40, 0x00], // i
            [0x20, 0x40, 0x44, 0x3d, 0x00], // j
            [0x7f, 0x10, 0x28, 0x44, 0x00], // k
            [0x00, 0x41, 0x7f, 0x40, 0x00], // l
            [0x7c, 0x04, 0x18, 0x04, 0x78], // m
            [0x7c, 0x08, 0x04, 0x04, 0x78], // n
            [0x38, 0x44, 0x44, 0x44, 0x38], // o
            [0xfc, 0x24, 0x24, 0x24, 0x18], // p
            [0x18, 0x24, 0x24, 0x24, 0xfc], // q
            [0x7c, 0x08, 0x04, 0x04, 0x08], // r
            [0x48, 0x54, 0x54, 0x54, 0x24], // s
            [0x04, 0x3f, 0x44, 0x40, 0x20], // t
            [0x3c, 0x40, 0x40, 0x20, 0x7c], // u
            [0x1c, 0x20, 0x40, 0x20, 0x1c], // v
            [0x3c, 0x40, 0x30, 0x40, 0x3c], // w
            [0x44, 0x28, 0x10, 0x28, 0x44], // x
            [0x4c, 0x90, 0x90, 0x90, 0x7c], // y
            [0x44, 0x64, 0x54, 0x4c, 0x44], // z
        ];
        return Some(LOWER[(ch - b'a') as usize]);
    }
    let upper = ch.to_ascii_uppercase();
    let index = match upper {
        b'0'..=b'9' => (upper - b'0') as usize,
        b'A'..=b'Z' => 10 + (upper - b'A') as usize,
        b'.' => 36,
        b',' => 37,
        b':' => 38,
        b'!' => 39,
        b'?' => 40,
        b'-' => 41,
        b'_' => 42,
        b'+' => 43,
        b'/' => 44,
        b'(' => 45,
        b')' => 46,
        b'=' => 47,
        b'*' => 48,
        b'#' => 49,
        // N494: more punctuation.
        b'"' => 50,
        b'\'' => 51,
        b'%' => 52,
        b'&' => 53,
        b'@' => 54,
        b';' => 55,
        b'<' => 56,
        b'>' => 57,
        b'~' => 58,
        _ => return None,
    };
    const FONT: [[u8; 5]; 59] = [
        [0x3e, 0x51, 0x49, 0x45, 0x3e],
        [0x00, 0x42, 0x7f, 0x40, 0x00],
        [0x42, 0x61, 0x51, 0x49, 0x46],
        [0x21, 0x41, 0x45, 0x4b, 0x31],
        [0x18, 0x14, 0x12, 0x7f, 0x10],
        [0x27, 0x45, 0x45, 0x45, 0x39],
        [0x3c, 0x4a, 0x49, 0x49, 0x30],
        [0x01, 0x71, 0x09, 0x05, 0x03],
        [0x36, 0x49, 0x49, 0x49, 0x36],
        [0x06, 0x49, 0x49, 0x29, 0x1e],
        [0x7e, 0x09, 0x09, 0x09, 0x7e],
        [0x7f, 0x49, 0x49, 0x49, 0x36],
        [0x3e, 0x41, 0x41, 0x41, 0x22],
        [0x7f, 0x41, 0x41, 0x22, 0x1c],
        [0x7f, 0x49, 0x49, 0x49, 0x41],
        [0x7f, 0x09, 0x09, 0x09, 0x01],
        [0x3e, 0x41, 0x49, 0x49, 0x7a],
        [0x7f, 0x08, 0x08, 0x08, 0x7f],
        [0x00, 0x41, 0x7f, 0x41, 0x00],
        [0x20, 0x40, 0x41, 0x3f, 0x01],
        [0x7f, 0x08, 0x14, 0x22, 0x41],
        [0x7f, 0x40, 0x40, 0x40, 0x40],
        [0x7f, 0x02, 0x0c, 0x02, 0x7f],
        [0x7f, 0x04, 0x08, 0x10, 0x7f],
        [0x3e, 0x41, 0x41, 0x41, 0x3e],
        [0x7f, 0x09, 0x09, 0x09, 0x06],
        [0x3e, 0x41, 0x51, 0x21, 0x5e],
        [0x7f, 0x09, 0x19, 0x29, 0x46],
        [0x46, 0x49, 0x49, 0x49, 0x31],
        [0x01, 0x01, 0x7f, 0x01, 0x01],
        [0x3f, 0x40, 0x40, 0x40, 0x3f],
        [0x1f, 0x20, 0x40, 0x20, 0x1f],
        [0x3f, 0x40, 0x38, 0x40, 0x3f],
        [0x63, 0x14, 0x08, 0x14, 0x63],
        [0x07, 0x08, 0x70, 0x08, 0x07],
        [0x61, 0x51, 0x49, 0x45, 0x43],
        [0x00, 0x00, 0x00, 0x00, 0x40],
        [0x00, 0x00, 0x00, 0x40, 0x40],
        [0x00, 0x20, 0x00, 0x20, 0x00],
        [0x00, 0x00, 0x5f, 0x00, 0x00],
        [0x02, 0x01, 0x51, 0x09, 0x06],
        [0x08, 0x08, 0x08, 0x08, 0x08],
        [0x40, 0x40, 0x40, 0x40, 0x40],
        [0x08, 0x08, 0x3e, 0x08, 0x08],
        [0x20, 0x10, 0x08, 0x04, 0x02],
        [0x00, 0x1c, 0x22, 0x41, 0x00],
        [0x00, 0x41, 0x22, 0x1c, 0x00],
        [0x14, 0x14, 0x14, 0x14, 0x14],
        [0x14, 0x08, 0x3e, 0x08, 0x14],
        [0x14, 0x7f, 0x14, 0x7f, 0x14],
        // N494: " ' % & @ ; < > ~
        [0x03, 0x00, 0x03, 0x00, 0x00],
        [0x03, 0x00, 0x00, 0x00, 0x00],
        [0x63, 0x13, 0x08, 0x64, 0x63],
        [0x36, 0x49, 0x55, 0x22, 0x50],
        [0x3e, 0x41, 0x5d, 0x55, 0x3e],
        [0x00, 0x56, 0x36, 0x00, 0x00],
        [0x08, 0x14, 0x22, 0x41, 0x00],
        [0x00, 0x41, 0x22, 0x14, 0x08],
        [0x08, 0x04, 0x08, 0x10, 0x08],
    ];
    FONT.get(index).copied()
}

/// N318: wrap `text` for a `max_cols`-wide cell grid. Explicit `\n` always
/// starts a new line; spaces are break points, and a word longer than the line
/// is hard-broken. Bounded by the input length.
fn wrap_text(text: &str, max_cols: usize) -> Vec<String> {
    let max_cols = max_cols.max(1);
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current: Vec<char> = Vec::new();
        for word in paragraph.split(' ') {
            let word_chars: Vec<char> = word.chars().collect();
            if word_chars.len() > max_cols {
                // A word that cannot fit on any line: hard-break it.
                if !current.is_empty() {
                    lines.push(current.iter().collect());
                    current.clear();
                }
                let mut chunk: Vec<char> = Vec::new();
                for ch in word_chars {
                    chunk.push(ch);
                    if chunk.len() == max_cols {
                        lines.push(chunk.iter().collect());
                        chunk.clear();
                    }
                }
                current = chunk;
                continue;
            }
            let needed = current.len() + usize::from(!current.is_empty()) + word_chars.len();
            if needed <= max_cols {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.extend(word_chars);
            } else {
                lines.push(current.iter().collect());
                current = word_chars;
            }
        }
        // A trailing hard-break can leave nothing; do not add a phantom line.
        if !current.is_empty() {
            lines.push(current.iter().collect());
        } else if lines.is_empty() {
            lines.push(String::new());
        }
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    x: u16,
    y: u16,
    text: &str,
    color: u16,
    scale_x: u32,
    scale_y: u32,
    alpha: u8,
) {
    let scale_x = scale_x.max(1);
    let scale_y = scale_y.max(1);
    for (index, ch) in text.bytes().enumerate() {
        let Some(columns) = glyph_columns(ch) else {
            continue;
        };
        let origin_x = u32::from(x) + index as u32 * 6 * scale_x;
        for (col, bits) in columns.iter().enumerate() {
            for row in 0..7u32 {
                if bits & (1 << row) == 0 {
                    continue;
                }
                for dy in 0..scale_y {
                    for dx in 0..scale_x {
                        let px = origin_x + col as u32 * scale_x + dx;
                        let py = u32::from(y) + row * scale_y + dy;
                        if px >= width || py >= height {
                            continue;
                        }
                        let at = ((py * width + px) * 2) as usize;
                        put_pixel(pixels, at, color, alpha);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// N556: decode a tightly packed `w*h` RGB565 hex word string (4 nibbles each).
fn decode_hex_words(hex: &str, count: usize) -> Option<Vec<u16>> {
    if hex.len() != count * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let start = index * 4;
        out.push(u16::from_str_radix(&hex[start..start + 4], 16).ok()?);
    }
    Some(out)
}

/// N556: sample a decoded source grid at `(fx, fy)` in `0..=1`, nearest (default)
/// or bilinear. Edge samples clamp to the border texel.
fn sample_source(source: &[u16], src_w: u32, src_h: u32, fx: f32, fy: f32, linear: bool) -> u16 {
    let max_x = src_w.saturating_sub(1);
    let max_y = src_h.saturating_sub(1);
    if !linear {
        let sx = ((fx * src_w as f32) as u32).min(max_x);
        let sy = ((fy * src_h as f32) as u32).min(max_y);
        return source[(sy * src_w + sx) as usize];
    }
    // Clamp the (continuous) sample position to the source edges first, so a
    // border pixel blends toward the edge texel rather than reading outside it.
    let px = (fx * src_w as f32 - 0.5).clamp(0.0, max_x as f32);
    let py = (fy * src_h as f32 - 0.5).clamp(0.0, max_y as f32);
    let x0 = px.floor() as u32;
    let y0 = py.floor() as u32;
    let x1 = (x0 + 1).min(max_x);
    let y1 = (y0 + 1).min(max_y);
    let tx = px - x0 as f32;
    let ty = py - y0 as f32;
    let at = |x: u32, y: u32| source[(y * src_w + x) as usize];
    let mix = |a: u16, b: u16, t: f32| -> u16 {
        let channel = |shift: u32| -> u16 {
            let av = (a >> shift) & 0x1f;
            let bv = (b >> shift) & 0x1f;
            ((av as f32) + ((bv as f32) - (av as f32)) * t).round() as u16 & 0x1f
        };
        (channel(11) << 11) | (channel(5) << 5) | channel(0)
    };
    let top = mix(at(x0, y0), at(x1, y0), tx);
    let bottom = mix(at(x0, y1), at(x1, y1), tx);
    mix(top, bottom, ty)
}

/// N556/N565: the transform flags an `image` node's `mode` selects.
#[derive(Debug, Clone, Copy, Default)]
struct ImageMode {
    linear: bool,
    flip_h: bool,
    flip_v: bool,
    /// N575: preserve aspect ratio (contain) instead of stretching to the rect.
    fit: bool,
}

/// N565/N575: parse comma-separated `mode` tokens for an `image` node.
fn parse_image_mode(mode: Option<&str>) -> ImageMode {
    let mut parsed = ImageMode::default();
    for token in mode.unwrap_or("").split(',').map(str::trim) {
        match token {
            "linear" => parsed.linear = true,
            "flip-h" => parsed.flip_h = true,
            "flip-v" => parsed.flip_v = true,
            "fit" => parsed.fit = true,
            _ => {}
        }
    }
    parsed
}

/// N556/N565: blit a hex RGB565 image, optionally scaling a `src_w × src_h`
/// source grid into the `w × h` destination with nearest or bilinear sampling,
/// a horizontal/vertical flip, and a quarter-turn rotation.
#[allow(clippy::too_many_arguments)]
fn blit_hex_rgb565(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    hex: &str,
    alpha: u8,
    src_w: u16,
    src_h: u16,
    mode: ImageMode,
    rotate: u16,
    align: &str,
) -> bool {
    if w == 0 || h == 0 || src_w == 0 || src_h == 0 {
        return false;
    }
    let Some(source) = decode_hex_words(hex, usize::from(src_w) * usize::from(src_h)) else {
        return false;
    };
    // N575: `fit` preserves the source aspect ratio (letterboxed in the rect);
    // the default stretches each axis independently.
    let (draw_w, draw_h, off_x, off_y) = if mode.fit {
        let scale = (f32::from(w) / f32::from(src_w)).min(f32::from(h) / f32::from(src_h));
        let draw_w = ((f32::from(src_w) * scale).round() as u16).clamp(1, w);
        let draw_h = ((f32::from(src_h) * scale).round() as u16).clamp(1, h);
        // N585: horizontal alignment of the letterbox (default centred).
        let off_x = match align {
            "left" => 0,
            "right" => w - draw_w,
            _ => (w - draw_w) / 2,
        };
        (draw_w, draw_h, off_x, (h - draw_h) / 2)
    } else {
        (w, h, 0, 0)
    };
    let mut drew = false;
    for row in 0..u32::from(draw_h) {
        let py = u32::from(y) + u32::from(off_y) + row;
        if py >= height {
            break;
        }
        for col in 0..u32::from(draw_w) {
            let px = u32::from(x) + u32::from(off_x) + col;
            if px >= width {
                break;
            }
            let mut fx = (col as f32 + 0.5) / f32::from(draw_w);
            let mut fy = (row as f32 + 0.5) / f32::from(draw_h);
            // N565: flip first, then rotate (both about the rect centre).
            if mode.flip_h {
                fx = 1.0 - fx;
            }
            if mode.flip_v {
                fy = 1.0 - fy;
            }
            match rotate {
                90 => {
                    let t = fx;
                    fx = fy;
                    fy = 1.0 - t;
                }
                180 => {
                    fx = 1.0 - fx;
                    fy = 1.0 - fy;
                }
                270 => {
                    let t = fx;
                    fx = 1.0 - fy;
                    fy = t;
                }
                _ => {}
            }
            let value = sample_source(
                &source,
                u32::from(src_w),
                u32::from(src_h),
                fx,
                fy,
                mode.linear,
            );
            let at = ((py * width + px) * 2) as usize;
            put_pixel(pixels, at, value, alpha);
            drew = true;
        }
    }
    drew
}

/// N556: a `"sw,sh"` source dimension spec for an `image` node.
fn parse_source_dims(spec: Option<&str>) -> Option<(u16, u16)> {
    let (w, h) = spec?.split_once(',')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

#[allow(clippy::too_many_arguments)]
fn blit_sprite(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    atlas: &mdc_model::AtlasBlob,
    index: u32,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    alpha: u8,
) -> bool {
    let Some(offset) = atlas.frame_offset(index) else {
        return false;
    };
    if w == 0 || h == 0 || atlas.sprite_width == 0 || atlas.sprite_height == 0 {
        return false;
    }
    let sprite_w = atlas.sprite_width as usize;
    let atlas_row = (atlas.columns as usize) * sprite_w * 2;
    let mut drew = false;
    for row in 0..u32::from(h) {
        for col in 0..u32::from(w) {
            let px = u32::from(x) + col;
            let py = u32::from(y) + row;
            if px >= width || py >= height {
                continue;
            }
            let sx = (col * atlas.sprite_width / u32::from(w)) as usize;
            let sy = (row * atlas.sprite_height / u32::from(h)) as usize;
            let src = offset + sy * atlas_row + sx * 2;
            let dst = ((py * width + px) * 2) as usize;
            if src + 1 >= atlas.rgb565.len() || dst + 1 >= pixels.len() {
                continue;
            }
            let value = u16::from_le_bytes([atlas.rgb565[src], atlas.rgb565[src + 1]]);
            put_pixel(pixels, dst, value, alpha);
            drew = true;
        }
    }
    drew
}

fn fill_fan(pixels: &mut [u8], width: u32, height: u32, points: &[(i16, i16)], color: u16) {
    if points.len() < 3 || width == 0 || height == 0 {
        return;
    }
    let origin = points[0];
    for window in points[1..].windows(2) {
        fill_triangle(pixels, width, height, origin, window[0], window[1], color);
    }
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
    let max_x = (a.0.max(b.0).max(c.0).max(0) as u32).min(width.saturating_sub(1));
    let min_y = a.1.min(b.1).min(c.1).max(0) as u32;
    let max_y = (a.1.max(b.1).max(c.1).max(0) as u32).min(height.saturating_sub(1));
    let area = fan_edge(a, b, c);
    if area == 0 {
        return;
    }
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let p = (x as i16, y as i16);
            let w0 = fan_edge(b, c, p);
            let w1 = fan_edge(c, a, p);
            let w2 = fan_edge(a, b, p);
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

/// N178: fill a triangle with `texture` sampled through interpolated UVs.
/// N206: the material colour (`tint`) multiplies the sampled texel.
/// N296: when `mask`, transparent texels are skipped (MASK cutout).
#[allow(clippy::too_many_arguments)]
fn fill_textured_triangle(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    a: (i16, i16),
    uva: [f32; 2],
    b: (i16, i16),
    uvb: [f32; 2],
    c: (i16, i16),
    uvc: [f32; 2],
    texture: &mdc_model::TextureBlob,
    tint: u16,
    mask: bool,
) {
    let min_x = a.0.min(b.0).min(c.0).max(0) as u32;
    let max_x = (a.0.max(b.0).max(c.0).max(0) as u32).min(width.saturating_sub(1));
    let min_y = a.1.min(b.1).min(c.1).max(0) as u32;
    let max_y = (a.1.max(b.1).max(c.1).max(0) as u32).min(height.saturating_sub(1));
    let area = fan_edge(a, b, c);
    if area == 0 {
        return;
    }
    let area_f = area as f32;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let p = (x as i16, y as i16);
            let e0 = fan_edge(b, c, p);
            let e1 = fan_edge(c, a, p);
            let e2 = fan_edge(a, b, p);
            let inside = if area > 0 {
                e0 >= 0 && e1 >= 0 && e2 >= 0
            } else {
                e0 <= 0 && e1 <= 0 && e2 <= 0
            };
            if !inside {
                continue;
            }
            let w0 = e0 as f32 / area_f;
            let w1 = e1 as f32 / area_f;
            let w2 = e2 as f32 / area_f;
            let u = w0 * uva[0] + w1 * uvb[0] + w2 * uvc[0];
            let v = w0 * uva[1] + w1 * uvb[1] + w2 * uvc[1];
            if mask && !texture.alpha_at(u, v) {
                continue;
            }
            let bytes = rgb565_mul(texture.sample(u, v), tint).to_le_bytes();
            let index = ((y * width + x) * 2) as usize;
            if index + 1 < pixels.len() {
                pixels[index] = bytes[0];
                pixels[index + 1] = bytes[1];
            }
        }
    }
}

/// N206: multiply two RGB565 colours channel-wise (0xFFFF is the identity).
fn rgb565_mul(a: u16, b: u16) -> u16 {
    let channel = |shift: u32, mask: u32| {
        let av = u32::from((a >> shift) & mask as u16);
        let bv = u32::from((b >> shift) & mask as u16);
        (((av * bv + mask / 2) / mask) as u16 & mask as u16) << shift
    };
    channel(11, 0x1f) | channel(5, 0x3f) | channel(0, 0x1f)
}

fn fade_toward_black(pixels: &mut [u8], quarters: u8) {
    let keep = u32::from(quarters.min(3));
    for chunk in pixels.chunks_exact_mut(2) {
        let value = u16::from_le_bytes([chunk[0], chunk[1]]);
        let channel = |shift: u32, mask: u16| {
            let sample = u32::from((value >> shift) & mask);
            ((sample * keep / 4) as u16) << shift
        };
        let mixed = channel(11, 0x1f) | channel(5, 0x3f) | channel(0, 0x1f);
        let bytes = mixed.to_le_bytes();
        chunk[0] = bytes[0];
        chunk[1] = bytes[1];
    }
}

/// N153: mix one atlas frame over `pixels` by `quarters`/4 (0 keeps the pixels,
/// 4 replaces them with the frame). Used by the animation cross-fade.
/// N330: blend `src` over `dst` (both whole RGB565 LE buffers) with `quarters/4`
/// weight toward `src`. 0 keeps `dst`, 4 copies `src`.
fn mix_rgb565(dst: &mut [u8], src: &[u8], quarters: u8) {
    let q = u32::from(quarters.min(4));
    let keep = 4 - q;
    if q == 0 {
        return;
    }
    let n = (dst.len().min(src.len())) / 2 * 2;
    if q >= 4 {
        dst[..n].copy_from_slice(&src[..n]);
        return;
    }
    for i in (0..n).step_by(2) {
        let dv = u16::from_le_bytes([dst[i], dst[i + 1]]);
        let sv = u16::from_le_bytes([src[i], src[i + 1]]);
        let channel = |shift: u32, mask: u16| -> u16 {
            let d = u32::from((dv >> shift) & mask);
            let s = u32::from((sv >> shift) & mask);
            ((d * keep + s * q) / 4) as u16
        };
        let out = (channel(11, 0x1f) << 11) | (channel(5, 0x3f) << 5) | channel(0, 0x1f);
        let bytes = out.to_le_bytes();
        dst[i] = bytes[0];
        dst[i + 1] = bytes[1];
    }
}

fn mix_atlas_frame(
    pixels: &mut [u8],
    atlas: &mdc_model::AtlasBlob,
    index: u32,
    quarters: u8,
    dst_row_bytes: usize,
) {
    let q = u32::from(quarters.min(4));
    let keep = 4 - q;
    if q == 0 || dst_row_bytes == 0 {
        return;
    }
    let Some(offset) = atlas.frame_offset(index) else {
        return;
    };
    let sprite_w = atlas.sprite_width as usize;
    let sprite_h = atlas.sprite_height as usize;
    let row_bytes = sprite_w * 2;
    if sprite_w == 0 || row_bytes == 0 || row_bytes > dst_row_bytes {
        return;
    }
    let atlas_row = (atlas.columns as usize) * sprite_w * 2;
    let rows = sprite_h.min(pixels.len() / dst_row_bytes);
    for y in 0..rows {
        let src = offset + y * atlas_row;
        let dst = y * dst_row_bytes;
        if src + row_bytes > atlas.rgb565.len() || dst + row_bytes > pixels.len() {
            break;
        }
        for x in 0..sprite_w {
            let d = &mut pixels[dst + x * 2..dst + x * 2 + 2];
            let s = &atlas.rgb565[src + x * 2..src + x * 2 + 2];
            let dv = u16::from_le_bytes([d[0], d[1]]);
            let sv = u16::from_le_bytes([s[0], s[1]]);
            let channel = |shift: u32, mask: u16| {
                let a = u32::from((dv >> shift) & mask);
                let b = u32::from((sv >> shift) & mask);
                (((a * keep + b * q) / 4) as u16) << shift
            };
            let mixed = channel(11, 0x1f) | channel(5, 0x3f) | channel(0, 0x1f);
            let bytes = mixed.to_le_bytes();
            d[0] = bytes[0];
            d[1] = bytes[1];
        }
    }
}

fn fan_edge(a: (i16, i16), b: (i16, i16), p: (i16, i16)) -> i32 {
    (i32::from(p.0) - i32::from(a.0)) * (i32::from(b.1) - i32::from(a.1))
        - (i32::from(p.1) - i32::from(a.1)) * (i32::from(b.0) - i32::from(a.0))
}

/// N138: an accumulated scene transform. Offsets are pixels; scales are
/// permille (1000 = 1×).
#[derive(Clone, Copy)]
struct SceneTransform {
    ox: i32,
    oy: i32,
    sx: i32,
    sy: i32,
}

impl SceneTransform {
    fn identity() -> Self {
        Self {
            ox: 0,
            oy: 0,
            sx: 1000,
            sy: 1000,
        }
    }

    /// Apply the transform to a geometry rect (clamped to `u16`).
    fn apply(&self, rect: [u16; 4]) -> [u16; 4] {
        let scale = |value: u16, permille: i32| {
            (i32::from(value) * permille / 1000).clamp(0, i32::from(u16::MAX)) as u16
        };
        [
            (self.ox + i32::from(rect[0]) * self.sx / 1000).clamp(0, i32::from(u16::MAX)) as u16,
            (self.oy + i32::from(rect[1]) * self.sy / 1000).clamp(0, i32::from(u16::MAX)) as u16,
            scale(rect[2], self.sx),
            scale(rect[3], self.sy),
        ]
    }

    /// N138: a `group` node's rect is a child offset + scale. A zero width or
    /// height means "do not scale that axis".
    fn child(self, rect: [u16; 4]) -> Self {
        let scale = |value: u16, permille: i32| {
            if value == 0 {
                permille
            } else {
                permille * i32::from(value) / 1000
            }
        };
        Self {
            ox: self.ox + i32::from(rect[0]) * self.sx / 1000,
            oy: self.oy + i32::from(rect[1]) * self.sy / 1000,
            sx: scale(rect[2], self.sx),
            sy: scale(rect[3], self.sy),
        }
    }

    /// N368: a `clip` node's rect offsets its children without scaling them; the
    /// rect only bounds what is composited back.
    fn offset(self, rect: [u16; 4]) -> Self {
        Self {
            ox: self.ox + i32::from(rect[0]) * self.sx / 1000,
            oy: self.oy + i32::from(rect[1]) * self.sy / 1000,
            sx: self.sx,
            sy: self.sy,
        }
    }
}

/// N355: fill a rect with rounded corners of `radius` (clamped to half the
/// smaller side). The pixels outside the corner quarter-circles are skipped.
#[allow(clippy::too_many_arguments)]
fn fill_rounded_rect(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    rect: [u16; 4],
    color: u16,
    radius: u16,
    alpha: u8,
) {
    let [x, y, w, h] = rect;
    if w == 0 || h == 0 {
        return;
    }
    let r = i32::from(radius).min(i32::from(w.min(h)) / 2);
    for yy in y..y.saturating_add(h) {
        if u32::from(yy) >= height {
            break;
        }
        for xx in x..x.saturating_add(w) {
            if u32::from(xx) >= width {
                break;
            }
            // Distance test only inside the four corner boxes.
            let cx =
                i32::from(xx).min(i32::from(x) + i32::from(w) - 1 - i32::from(xx)) - i32::from(x);
            let cy =
                i32::from(yy).min(i32::from(y) + i32::from(h) - 1 - i32::from(yy)) - i32::from(y);
            if cx < r && cy < r {
                let dx = r - 1 - cx;
                let dy = r - 1 - cy;
                if dx * dx + dy * dy > r * r {
                    continue;
                }
            }
            let index = ((u32::from(yy) * width + u32::from(xx)) * 2) as usize;
            put_pixel(pixels, index, color, alpha);
        }
    }
}

/// N348: interpolate two RGB565 colours by `t` (0..=255).
fn lerp_rgb565(a: u16, b: u16, t: u8) -> u16 {
    let tt = u32::from(t);
    let k = 255 - tt;
    let channel = |shift: u32, mask: u16| -> u16 {
        let av = u32::from((a >> shift) & mask);
        let bv = u32::from((b >> shift) & mask);
        ((av * k + bv * tt) / 255) as u16
    };
    (channel(11, 0x1f) << 11) | (channel(5, 0x3f) << 5) | channel(0, 0x1f)
}

/// N535: the radial centre as fractions of the rect (`"50,50"` = centred).
fn parse_focus(spec: Option<&str>) -> (f32, f32) {
    let Some(spec) = spec else {
        return (0.5, 0.5);
    };
    let mut parts = spec.split(',');
    let parse = |value: Option<&str>| -> Option<f32> {
        value?
            .trim()
            .trim_end_matches('%')
            .parse::<f32>()
            .ok()
            .map(|v| (v / 100.0).clamp(0.0, 1.0))
    };
    match (parse(parts.next()), parse(parts.next())) {
        (Some(x), Some(y)) => (x, y),
        _ => (0.5, 0.5),
    }
}

/// N505: parse a gradient's colour stops. `value2` may be a comma-separated
/// `offset%:#RRGGBB` list (e.g. `0:#FF0000,50:#00FF00,100:#0000FF`); anything
/// else falls back to the two-stop `value` -> `value2` ramp.
fn parse_gradient_stops(start: u16, end: u16, spec: Option<&str>) -> Vec<(u8, u16)> {
    let fallback = vec![(0u8, start), (255u8, end)];
    let Some(spec) = spec else {
        return fallback;
    };
    if !spec.contains(',') {
        return fallback;
    }
    let mut stops = Vec::new();
    for item in spec.split(',') {
        let Some((position, color)) = item.split_once(':') else {
            return fallback;
        };
        let Ok(percent) = position.trim().parse::<u32>() else {
            return fallback;
        };
        let at = ((percent.min(100) * 255) / 100) as u8;
        stops.push((at, mdc_protocol::parse_color(color.trim())));
    }
    if stops.len() < 2 {
        return fallback;
    }
    stops.sort_by_key(|(at, _)| *at);
    stops
}

/// N505: the colour at `t` (0..=255) across a stop list.
fn gradient_color(stops: &[(u8, u16)], t: u8) -> u16 {
    if let Some((_, first)) = stops.first() {
        if t <= stops[0].0 {
            return *first;
        }
    }
    for pair in stops.windows(2) {
        let (a_at, a) = pair[0];
        let (b_at, b) = pair[1];
        if t <= b_at {
            let span = u32::from(b_at.saturating_sub(a_at)).max(1);
            let local = (u32::from(t.saturating_sub(a_at)) * 255 / span).min(255) as u8;
            return lerp_rgb565(a, b, local);
        }
    }
    stops.last().map(|(_, color)| *color).unwrap_or(0xFFFF)
}

/// N338: source-over write of an RGB565 value at `index` with source alpha.
/// N480: add `value` (scaled by `alpha`) to the framebuffer, saturating per channel.
fn add_pixel(pixels: &mut [u8], index: usize, value: u16, alpha: u8) {
    if index + 1 >= pixels.len() || alpha == 0 {
        return;
    }
    let dst = u16::from_le_bytes([pixels[index], pixels[index + 1]]);
    let scale = |channel: u16, mask: u16| -> u16 {
        ((u32::from(channel & mask) * u32::from(alpha)) / 255) as u16
    };
    let src_r = scale(value, 0xf800) >> 11;
    let src_g = scale(value, 0x07e0) >> 5;
    let src_b = scale(value, 0x001f);
    let r = ((dst >> 11) & 0x1f).saturating_add(src_r);
    let g = ((dst >> 5) & 0x3f).saturating_add(src_g);
    let b = (dst & 0x1f).saturating_add(src_b);
    let out = (r.min(0x1f) << 11) | (g.min(0x3f) << 5) | b.min(0x1f);
    pixels[index..index + 2].copy_from_slice(&out.to_le_bytes());
}

/// N489: mix a new e-ink pixel over the old one, keeping a faint remnant.
/// N499: because the mix is relative to the current pixel, repeating the same
/// partial refresh fades the ghost, while a full frame replaces the pixel
/// exactly (no ghost).
fn ghost_mix(old: u16, new: u16) -> u16 {
    let mix = |shift: u32, mask: u16| -> u16 {
        let o = u32::from((old >> shift) & mask);
        let n = u32::from((new >> shift) & mask);
        ((o * 15 + n * 85) / 100) as u16
    };
    (mix(11, 0x1f) << 11) | (mix(5, 0x3f) << 5) | mix(0, 0x1f)
}

fn put_pixel(pixels: &mut [u8], index: usize, value: u16, alpha: u8) {
    if index + 1 >= pixels.len() || alpha == 0 {
        return;
    }
    let out = if alpha == u8::MAX {
        value
    } else {
        let dst = u16::from_le_bytes([pixels[index], pixels[index + 1]]);
        let af = u32::from(alpha);
        let kf = 255 - af;
        let channel = |shift: u32, mask: u16| -> u16 {
            let d = u32::from((dst >> shift) & mask);
            let s = u32::from((value >> shift) & mask);
            ((d * kf + s * af) / 255) as u16
        };
        (channel(11, 0x1f) << 11) | (channel(5, 0x3f) << 5) | channel(0, 0x1f)
    };
    let bytes = out.to_le_bytes();
    pixels[index] = bytes[0];
    pixels[index + 1] = bytes[1];
}

/// N156: fill the ellipse inscribed in `rect` with `color`.
fn fill_circle(pixels: &mut [u8], width: u32, height: u32, rect: [u16; 4], color: u16, alpha: u8) {
    let [x, y, w, h] = rect;
    if w == 0 || h == 0 {
        return;
    }
    let cx = f32::from(x) + (f32::from(w) - 1.0) / 2.0;
    let cy = f32::from(y) + (f32::from(h) - 1.0) / 2.0;
    let rx = f32::from(w) / 2.0;
    let ry = f32::from(h) / 2.0;
    for yy in y..y.saturating_add(h) {
        if u32::from(yy) >= height {
            break;
        }
        for xx in x..x.saturating_add(w) {
            if u32::from(xx) >= width {
                break;
            }
            let dx = (f32::from(xx) - cx) / rx;
            let dy = (f32::from(yy) - cy) / ry;
            if dx * dx + dy * dy <= 1.0 {
                let index = ((u32::from(yy) * width + u32::from(xx)) * 2) as usize;
                put_pixel(pixels, index, color, alpha);
            }
        }
    }
}

/// N157: Bresenham line from `rect[0..2]` to `rect[2..4]`, clipped to bounds.
fn draw_line(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    rect: [u16; 4],
    color: u16,
    alpha: u8,
    // N497: 0 draws a solid line; otherwise this is the dash length in pixels.
    dash: u16,
) {
    let (mut x0, mut y0) = (i64::from(rect[0]), i64::from(rect[1]));
    let (x1, y1) = (i64::from(rect[2]), i64::from(rect[3]));
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut step: u32 = 0;
    loop {
        // N497: in a dashed line, only the first `dash` pixels of each period
        // are painted.
        let paint = dash == 0 || step % (2 * u32::from(dash)) < u32::from(dash);
        if paint && x0 >= 0 && y0 >= 0 && (x0 as u32) < width && (y0 as u32) < height {
            let index = ((y0 as u32 * width + x0 as u32) * 2) as usize;
            put_pixel(pixels, index, color, alpha);
        }
        step = step.saturating_add(1);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

/// N169: aspect-fit model points into a `width × height` framebuffer (Y flipped,
/// centered) for device-side geometry preview.
fn fit_preview(points: &[[f32; 3]], width: u32, height: u32) -> Vec<(i16, i16)> {
    let Some(first) = points.first() else {
        return Vec::new();
    };
    let mut min = [first[0], first[1]];
    let mut max = [first[0], first[1]];
    for p in points {
        min[0] = min[0].min(p[0]);
        min[1] = min[1].min(p[1]);
        max[0] = max[0].max(p[0]);
        max[1] = max[1].max(p[1]);
    }
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

/// N264: byte ranges where `current` differs from `previous`, coalescing gaps
/// smaller than `max_gap` bytes. Bounded by the frame length.
fn changed_ranges(previous: &[u8], current: &[u8], max_gap: usize) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let len = previous.len().min(current.len());
    let mut index = 0usize;
    while index < len {
        if previous[index] == current[index] {
            index += 1;
            continue;
        }
        let start = index;
        let mut last_diff = index;
        let mut scan = index;
        while scan < len {
            if previous[scan] != current[scan] {
                last_diff = scan;
            } else if scan - last_diff > max_gap {
                break;
            }
            scan += 1;
        }
        let end = last_diff + 1;
        ranges.push((start, end));
        index = end;
    }
    ranges
}

/// N418: the topmost scene node whose (transformed) rect contains the point.
fn hit_test_node(
    node: &mdc_protocol::SceneNode,
    transform: SceneTransform,
    x: u16,
    y: u16,
) -> Option<String> {
    // N450: a hidden node (and its children) is not hit-testable.
    if !node.is_visible() {
        return None;
    }
    let drawable = !matches!(node.kind.as_str(), "group" | "clip");
    let child_transform = if drawable {
        transform
    } else if node.kind == "clip" {
        match node.rect {
            Some(rect) => transform.offset(rect),
            None => transform,
        }
    } else {
        match node.rect {
            Some(rect) => transform.child(rect),
            None => transform,
        }
    };
    // N485: the highest `z` (then the last in document order) is on top.
    let mut children: Vec<&mdc_protocol::SceneNode> = node.children.iter().collect();
    children.sort_by_key(|child| (child.z(), std::cmp::Reverse(())));
    children.reverse();
    for child in children {
        if let Some(name) = hit_test_node(child, child_transform, x, y) {
            return Some(name);
        }
    }
    if drawable {
        if let Some(rect) = node.rect {
            // N475: hit-test against the rotated geometry.
            let rect = match node.rotate.unwrap_or(0) % 360 {
                90 | 270 => [rect[0], rect[1], rect[3], rect[2]],
                _ => rect,
            };
            let [rx, ry, rw, rh] = transform.apply(rect);
            let inside =
                x >= rx && y >= ry && x < rx.saturating_add(rw) && y < ry.saturating_add(rh);
            if inside {
                return Some(node.name.clone());
            }
        }
    }
    None
}

/// N80/N138: depth-first scene rasterizer; `rect` nodes fill from their colour,
/// `group` (or unknown) nodes transform their children.
fn fill_scene(
    node: &mdc_protocol::SceneNode,
    pixels: &mut [u8],
    width: u32,
    height: u32,
    atlas: Option<&mdc_model::AtlasBlob>,
    transform: SceneTransform,
    drew: &mut bool,
) {
    // N450: a hidden node (and its children) is not drawn.
    if !node.is_visible() {
        return;
    }
    let drawable = matches!(
        node.kind.as_str(),
        "rect" | "text" | "image" | "sprite" | "circle" | "line" | "gradient" | "rounded-rect"
    );
    // N338: source alpha for every rasterised kind.
    let alpha = node.opacity_u8();
    // N475: a quarter-turn swaps the rect's axes for axis-aligned fills.
    let rotate = node.rotate.unwrap_or(0) % 360;
    let rect = node.rect.map(|[x, y, w, h]| match rotate {
        90 | 270 => [x, y, h, w],
        _ => [x, y, w, h],
    });
    // N368: a `clip` node bounds its children to its rect. Children draw into a
    // copy of the target, then only the clip rect is composited back.
    if node.kind == "clip" {
        if let Some(rect) = rect {
            let [x, y, w, h] = transform.apply(rect);
            let mut scratch = pixels.to_vec();
            let mut child_drew = false;
            let child_transform = transform.offset(rect);
            for child in &node.children {
                fill_scene(
                    child,
                    &mut scratch,
                    width,
                    height,
                    atlas,
                    child_transform,
                    &mut child_drew,
                );
            }
            let x0 = u32::from(x);
            let y0 = u32::from(y);
            let x1 = (x0 + u32::from(w)).min(width);
            let y1 = (y0 + u32::from(h)).min(height);
            for yy in y0..y1 {
                let row = (yy * width) as usize * 2;
                let start = row + x0 as usize * 2;
                let end = row + x1 as usize * 2;
                pixels[start..end].copy_from_slice(&scratch[start..end]);
            }
            if child_drew {
                *drew = true;
            }
        }
        return;
    }
    if node.kind == "rect" {
        if let Some(rect) = rect {
            let [x, y, w, h] = transform.apply(rect);
            let color = node.color_rgb565();
            for yy in y..y.saturating_add(h) {
                if u32::from(yy) >= height {
                    break;
                }
                for xx in x..x.saturating_add(w) {
                    if u32::from(xx) >= width {
                        break;
                    }
                    let index = ((u32::from(yy) * width + u32::from(xx)) * 2) as usize;
                    // N480: additive blending for glow overlays.
                    if node.is_additive() {
                        add_pixel(pixels, index, color, alpha);
                    } else {
                        put_pixel(pixels, index, color, alpha);
                    }
                }
            }
            *drew = true;
        }
    } else if node.kind == "text" {
        if let (Some(rect), Some(text)) = (rect, node.value.as_deref()) {
            let [x, y, w, h] = transform.apply(rect);
            // N128/N318: the rect height sets a uniform glyph scale (1..=8); the
            // rect width bounds how many columns a line may use.
            let scale = u32::from((h / 7).clamp(1, 8));
            let scale_x = scale;
            let scale_y = scale;
            // N318: wrap to the rect width; the rect height sets the glyph scale,
            // and lines are clipped by the framebuffer (not the rect height).
            let max_cols = (u32::from(w) / (6 * scale_x)).max(1) as usize;
            // N495: `wrap` selects word/char/none line breaking.
            let lines = match node.wrap.as_deref() {
                Some("none") => text.split('\n').map(str::to_string).collect::<Vec<_>>(),
                Some("char") => text
                    .split('\n')
                    .flat_map(|line| {
                        line.as_bytes()
                            .chunks(max_cols)
                            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>(),
                _ => wrap_text(text, max_cols),
            };
            let line_height = (8 * scale_y).max(1);
            let color = node.color_rgb565();
            for (line_index, line) in lines.iter().enumerate() {
                let ly = u32::from(y) + line_index as u32 * line_height;
                if ly >= height {
                    break;
                }
                // N484: align the line within the rect width.
                let line_width = line.chars().count() as u32 * 6 * scale_x;
                let offset = match node.align.as_deref() {
                    Some("center") => u32::from(w).saturating_sub(line_width) / 2,
                    Some("right") => u32::from(w).saturating_sub(line_width),
                    _ => 0,
                };
                draw_text(
                    pixels,
                    width,
                    height,
                    x.saturating_add(offset.min(u32::from(u16::MAX)) as u16),
                    ly as u16,
                    line,
                    color,
                    scale_x,
                    scale_y,
                    alpha,
                );
            }
            *drew = true;
        }
    } else if node.kind == "image" {
        if let (Some(rect), Some(hex)) = (rect, node.value.as_deref()) {
            let [x, y, w, h] = transform.apply(rect);
            // N556: `value2` gives the source grid ("sw,sh", default the rect
            // size); `mode` lists `linear`/`flip-h`/`flip-v`. N565: `rotate`
            // turns the sampled image.
            let (src_w, src_h) = parse_source_dims(node.value2.as_deref()).unwrap_or((w, h));
            let mode = parse_image_mode(node.mode.as_deref());
            if blit_hex_rgb565(
                pixels,
                width,
                height,
                x,
                y,
                w,
                h,
                hex,
                alpha,
                src_w,
                src_h,
                mode,
                node.rotate.unwrap_or(0),
                node.align.as_deref().unwrap_or("center"),
            ) {
                *drew = true;
            }
        }
    } else if node.kind == "sprite" {
        if let (Some(atlas), Some(rect), Some(index)) = (
            atlas,
            rect,
            node.value.as_deref().and_then(|v| v.parse::<u32>().ok()),
        ) {
            let [x, y, w, h] = transform.apply(rect);
            if blit_sprite(pixels, width, height, atlas, index, x, y, w, h, alpha) {
                *drew = true;
            }
        }
    } else if node.kind == "circle" {
        if let Some(rect) = rect {
            fill_circle(
                pixels,
                width,
                height,
                transform.apply(rect),
                node.color_rgb565(),
                alpha,
            );
            *drew = true;
        }
    } else if node.kind == "line" {
        if let Some(rect) = rect {
            // N497: `value2` is an optional dash length.
            let dash = node
                .value2
                .as_deref()
                .and_then(|v| v.trim().parse::<u16>().ok())
                .unwrap_or(0);
            draw_line(
                pixels,
                width,
                height,
                transform.apply(rect),
                node.color_rgb565(),
                alpha,
                dash,
            );
            *drew = true;
        }
    } else if node.kind == "rounded-rect" {
        // N355: a rect with rounded corners; `value2` is the corner radius.
        if let Some(rect) = rect {
            let radius = node
                .value2
                .as_deref()
                .and_then(|v| v.trim().parse::<u16>().ok())
                .unwrap_or(0);
            fill_rounded_rect(
                pixels,
                width,
                height,
                transform.apply(rect),
                node.color_rgb565(),
                radius,
                alpha,
            );
            *drew = true;
        }
    } else if node.kind == "gradient" {
        // N348: vertical linear gradient from `value` to `value2` over `rect`.
        if let Some(rect) = rect {
            let [x, y, w, h] = transform.apply(rect);
            let start = node.color_rgb565();
            let end = node.color2_rgb565();
            // N505: a comma-separated `value2` becomes a multi-stop gradient.
            let stops = parse_gradient_stops(start, end, node.value2.as_deref());
            // N515: `mode: "radial"` interpolates by distance from the centre.
            let radial = node.mode.as_deref() == Some("radial");
            if radial {
                // N535: `focus` moves the centre (percentages of the rect).
                let (fx, fy) = parse_focus(node.focus.as_deref());
                let cx = f32::from(w) * fx;
                let cy = f32::from(h) * fy;
                let max_r = (f32::from(w) * (1.0 - fx).max(fx))
                    .hypot(f32::from(h) * (1.0 - fy).max(fy))
                    .max(1.0);
                for yy in y..y.saturating_add(h) {
                    if u32::from(yy) >= height {
                        break;
                    }
                    for xx in x..x.saturating_add(w) {
                        if u32::from(xx) >= width {
                            break;
                        }
                        let dx = f32::from(xx.saturating_sub(x)) + 0.5 - cx;
                        let dy = f32::from(yy.saturating_sub(y)) + 0.5 - cy;
                        let distance = (dx * dx + dy * dy).sqrt() / max_r;
                        let t = (distance.clamp(0.0, 1.0) * 255.0) as u8;
                        let color = gradient_color(&stops, t);
                        let index = ((u32::from(yy) * width + u32::from(xx)) * 2) as usize;
                        if node.is_additive() {
                            add_pixel(pixels, index, color, alpha);
                        } else {
                            put_pixel(pixels, index, color, alpha);
                        }
                    }
                }
            } else {
                // N525: the ramp direction follows `rotate` (0 = top-to-bottom,
                // 90 = left-to-right, 180/270 reverse, any other angle is a
                // diagonal). The projection is normalised across the rect.
                let theta = (f64::from(rotate) * std::f64::consts::PI / 180.0) as f32;
                let (dx, dy) = (theta.sin(), theta.cos());
                let (px, py) = (dx.max(0.0), dy.max(0.0));
                let (nx, ny) = (dx.min(0.0), dy.min(0.0));
                let span = ((px - nx) + (py - ny)).max(f32::EPSILON);
                // Pixel-index coordinates so the rect's edges hit the exact
                // endpoint colours (matching the pre-N525 integer ramp).
                let (fx, fy) = (
                    f32::from(w.saturating_sub(1)).max(1.0),
                    f32::from(h.saturating_sub(1)).max(1.0),
                );
                for yy in y..y.saturating_add(h) {
                    if u32::from(yy) >= height {
                        break;
                    }
                    let v = if h > 1 {
                        f32::from(yy.saturating_sub(y)) / fy
                    } else {
                        0.0
                    };
                    for xx in x..x.saturating_add(w) {
                        if u32::from(xx) >= width {
                            break;
                        }
                        let u = if w > 1 {
                            f32::from(xx.saturating_sub(x)) / fx
                        } else {
                            0.0
                        };
                        let t = (((u * dx + v * dy) - (nx + ny)) / span).clamp(0.0, 1.0);
                        let color = gradient_color(&stops, (t * 255.0) as u8);
                        let index = ((u32::from(yy) * width + u32::from(xx)) * 2) as usize;
                        if node.is_additive() {
                            add_pixel(pixels, index, color, alpha);
                        } else {
                            put_pixel(pixels, index, color, alpha);
                        }
                    }
                }
            }
            *drew = true;
        }
    }
    // A non-drawable node (group) offsets and scales its children.
    let child_transform = if drawable {
        transform
    } else {
        match node.rect {
            Some(rect) => transform.child(rect),
            None => transform,
        }
    };
    // N485: draw siblings in ascending `z` (stable, so ties keep document order).
    let mut children: Vec<&mdc_protocol::SceneNode> = node.children.iter().collect();
    children.sort_by_key(|child| child.z());
    for child in children {
        fill_scene(child, pixels, width, height, atlas, child_transform, drew);
    }
}

/// 虚拟时钟上，begin 之后这么多个 tick 没有 chunk 就回滚。
const OTA_CHUNK_TIMEOUT_TICKS: u64 = 5_000;
/// N330: mesh clip cross-fade window in virtual milliseconds.
const MESH_CROSSFADE_MS: u32 = 300;

/// N41: baseline managed configuration.
fn default_config() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert("brightness".into(), "128".into());
    map.insert("rotation".into(), "0".into());
    map.insert("wifi_ssid".into(), "mdc-sim".into());
    map.insert("wifi_password".into(), "mdc-sim-secret".into());
    // N586: how many unacknowledged tiles the host may accumulate before it must
    // resend a full frame (0 = resynchronise on the first missing tile).
    map.insert(mdc_core::TILES_MISSING_THRESHOLD_CONFIG.into(), "0".into());
    map
}

/// N41: secret values are never echoed back in a reply.
fn is_secret_key(key: &str) -> bool {
    key.contains("password") || key.contains("token")
}

/// N41: reject obviously invalid values before storing them.
fn validate_config(key: &str, value: &str) -> Result<(), &'static str> {
    if value.len() > 256 {
        return Err("E_VALUE");
    }
    match key {
        "brightness" => value.parse::<u8>().map(|_| ()).map_err(|_| "E_VALUE"),
        "rotation" => match value {
            "0" | "90" | "180" | "270" => Ok(()),
            _ => Err("E_VALUE"),
        },
        "wifi_ssid" => {
            if value.len() <= 32 {
                Ok(())
            } else {
                Err("E_VALUE")
            }
        }
        "wifi_password" => {
            if !value.is_empty() && value.len() <= 63 {
                Ok(())
            } else {
                Err("E_VALUE")
            }
        }
        // N586: a bounded tile-miss threshold.
        "tile_missing_threshold" => value.parse::<u64>().map(|_| ()).map_err(|_| "E_VALUE"),
        _ => Ok(()),
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SimError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("disconnected")]
    Disconnected,
    #[error("base frame mismatch")]
    BaseMismatch,
}

/// Virtual time counter (no wall clock).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtualClock {
    pub tick: u64,
}
impl VirtualClock {
    pub fn advance(&mut self, by: u64) {
        self.tick = self.tick.saturating_add(by);
    }
}

/// Optional fault injection for simulator peer tests.
#[derive(Debug, Clone, Default)]
pub struct FaultInjector {
    pub drop_next: bool,
    /// 先放行这么多个包，再丢掉接下来的一个，并清掉未完成的分块重组。
    pub drop_skip: u32,
    pub armed_drop: bool,
    /// Defer processing for this many virtual ticks after enqueue.
    pub delay_ticks: u64,
    pub disconnect: bool,
}
impl FaultInjector {
    pub fn drop_next(&mut self) {
        self.drop_next = true;
    }
    /// `accept` 个包正常处理，然后丢掉下一个包。用来丢掉分块帧的 commit。
    pub fn drop_one_after(&mut self, accept: u32) {
        self.drop_skip = accept;
        self.armed_drop = true;
    }
    pub fn delay(&mut self, ticks: u64) {
        self.delay_ticks = ticks;
    }
    pub fn request_disconnect(&mut self) {
        self.disconnect = true;
    }
}

/// Board profile dimensions used for Capabilities / pixel buffer.
#[derive(Debug, Clone)]
pub struct BoardProfile {
    pub device_id: String,
    pub width: u16,
    pub height: u16,
    pub touch: bool,
    pub ota: bool,
    /// 彩色刷新承诺。墨水屏必须为 false，不能把彩屏 FPS 写成该板能力。
    pub color_fps: bool,
    /// 触摸坐标旋转，仅接受 0/90/180/270。
    pub rotation: u16,
    /// 第二块 Surface。宽或高为 0 表示没有 overlay。
    pub overlay_width: u16,
    pub overlay_height: u16,
    /// Form B: device renders locally and mirrors frames back to the host.
    pub mirror: bool,
    pub mirror_fps: u16,
    /// N263: advertise `dirty-region` mirroring (a weak device may prefer it).
    pub mirror_dirty_region: bool,
    pub local_display_fps: u16,
    /// N27: local renderer capability advertised on the wire (not inferred from
    /// `mirror`). Drives the model-compiler target backend.
    pub renderer_sprite: bool,
    pub renderer_mesh2d: bool,
    pub renderer_mesh3d: bool,
    pub renderer_skeleton: bool,
    pub renderer_max_texture: u32,
    pub renderer_max_vertices: u32,
    /// N41: configuration keys this board accepts.
    pub config_keys: Vec<String>,
    /// N308: device-advertised preferred tile edge (0 = no preference; the host
    /// then uses its own default). Reported from the device, not assumed.
    pub preferred_tile_size: u16,
    /// N308: the device accepts partial (tile/dirty) updates.
    pub supports_partial_update: bool,
    /// N308: the device accepts a full framebuffer push (Form A).
    pub supports_full_framebuffer: bool,
}
impl Default for BoardProfile {
    fn default() -> Self {
        Self {
            device_id: "sim-esp32-jc3248w535".into(),
            width: 480,
            height: 320,
            touch: true,
            ota: true,
            color_fps: true,
            rotation: 0,
            overlay_width: 0,
            overlay_height: 0,
            // ESP32-S3 class: Form A only; no GPU to render + mirror at once.
            mirror: false,
            mirror_fps: 0,
            mirror_dirty_region: false,
            local_display_fps: 30,
            // MCU class: sprites only.
            renderer_sprite: true,
            renderer_mesh2d: false,
            renderer_mesh3d: false,
            renderer_skeleton: false,
            renderer_max_texture: 512,
            renderer_max_vertices: 0,
            // N41: a baseline board accepts a few managed settings.
            config_keys: vec![
                "brightness".into(),
                "rotation".into(),
                "wifi_ssid".into(),
                "wifi_password".into(),
                // N71: atlas animation control (only meaningful with a baked atlas).
                "animation".into(),
                // N347: device texture sampling filter.
                "filtering".into(),
                // N586: tolerated unacknowledged tiles before a full frame.
                "tile_missing_threshold".into(),
            ],
            // N308: an MCU-class panel prefers 32-pixel tiles and takes both
            // partial and full updates (it advertises tile support).
            preferred_tile_size: 32,
            supports_partial_update: true,
            supports_full_framebuffer: true,
        }
    }
}
impl BoardProfile {
    /// Resolve a known board id used by CLI `--board`.
    pub fn from_board_id(id: &str) -> Self {
        match id {
            "esp32-jc3248w535-sim" | "default" | "" => Self::default(),
            "linux-virt" => Self {
                device_id: "sim-linux-virt".into(),
                width: 800,
                height: 480,
                touch: false,
                ota: false,
                color_fps: true,
                // Form B: a Linux board with a GPU renders and mirrors.
                mirror: true,
                mirror_fps: 10,
                mirror_dirty_region: true,
                renderer_mesh2d: true,
                renderer_mesh3d: true,
                renderer_skeleton: true,
                renderer_max_texture: 2048,
                renderer_max_vertices: 200_000,
                // N308: a bigger panel can take bigger tiles.
                preferred_tile_size: 64,
                ..Self::default()
            },
            "pi4-hdmi" => Self {
                device_id: "sim-pi4-hdmi".into(),
                width: 640,
                height: 480,
                touch: false,
                ota: false,
                color_fps: true,
                mirror: true,
                mirror_fps: 15,
                mirror_dirty_region: true,
                renderer_mesh2d: true,
                renderer_mesh3d: true,
                renderer_skeleton: true,
                renderer_max_texture: 2048,
                renderer_max_vertices: 200_000,
                ..Self::default()
            },
            "waveshare-eink" => Self {
                device_id: "sim-waveshare-eink".into(),
                width: 800,
                height: 480,
                touch: false,
                ota: false,
                color_fps: false,
                ..Self::default()
            },
            "dual-sim" => Self {
                device_id: "sim-dual".into(),
                width: 8,
                height: 4,
                touch: false,
                ota: false,
                color_fps: true,
                overlay_width: 4,
                overlay_height: 2,
                ..Self::default()
            },
            other => Self {
                device_id: format!("sim-{other}"),
                ..Self::default()
            },
        }
    }
}

/// Counters from the last simulate / poll session (inspector JSON).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SimMetrics {
    pub device_id: String,
    pub frames_received: u64,
    pub tiles_received: u64,
    pub inputs_emitted: u64,
    pub ota_commands: u64,
    pub ota_rollbacks: u64,
    pub acks_sent: u64,
    pub errors_sent: u64,
    pub mirror_frames: u64,
    pub mirror_commands: u64,
    pub packages_received: u64,
    pub config_commands: u64,
    pub scene_commands: u64,
    /// N316: diagnostic records (metrics + boot info) emitted to the host.
    pub diagnostics_emitted: u64,
    pub current_frame_id: u64,
    pub pixel_bytes: usize,
}

struct PendingDelay {
    ready_at: u64,
    packet: Packet,
}

/// FakeDevice peer speaking Protocol v1 over any [`Transport`].
pub struct FakeDevice {
    link: Box<dyn Transport>,
    profile: BoardProfile,
    pixels: Vec<u8>,
    overlay_pixels: Vec<u8>,
    current_frame_id: u64,
    assembler: Option<(String, u16, u16, FrameAssembler)>,
    pub clock: VirtualClock,
    pub faults: FaultInjector,
    rx: Vec<u8>,
    max_message: usize,
    pub max_chunk: usize,
    delayed: Vec<PendingDelay>,
    connected: bool,
    pub metrics: SimMetrics,
    ota_active: bool,
    /// begin 之后若虚拟时钟越过这个 tick 还没有 chunk，就回滚并解除冻结。
    ota_deadline: Option<u64>,
    firmware: String,
    pending_version: Option<String>,
    /// Emitted once after the next displayed full frame. TCP peers use this
    /// because the host cannot call `emit_input` on another process.
    input_after_frame: Option<InputEvent>,
    /// 仍按下的 pointer。断线时按最后一次合法坐标补发 cancel。
    active_pointers: BTreeMap<u16, (String, u16, u16)>,
    /// Virtual tick when an e-ink refresh finishes. Color panels leave this at 0.
    busy_until: u64,
    /// Form B: set by a host MirrorCtrl start; emit_mirror_frame is the device side.
    mirror_active: bool,
    /// N263: negotiated mirror mode ("full-frame" | "dirty-region").
    mirror_mode: String,
    /// N264: the previous mirrored frame, for dirty-region diffing (bounded).
    mirror_prev: Vec<u8>,
    /// N23: runtime package being assembled from PACKAGE chunks.
    package_assembler: Option<mdc_model::PackageAssembler>,
    /// N23: true once a rendered-ready package was accepted.
    package_ready: bool,
    /// N41: managed configuration values.
    config: BTreeMap<String, String>,
    /// N54: last accepted scene (node count); 0 with `scene_present` false = none.
    scene_nodes: u32,
    scene_present: bool,
    /// N80: the accepted scene for rasterization.
    scene: Option<mdc_protocol::SceneNode>,
    /// N61: baked atlas grid from the last accepted package.
    atlas_frames: u32,
    atlas_sprite: (u32, u32),
    /// N71: the accepted baked atlas + running clip.
    atlas: Option<mdc_model::AtlasBlob>,
    /// N178: the accepted mesh package's RGB565 texture, if any.
    texture: Option<mdc_model::TextureBlob>,
    /// N195: the accepted mesh package, rendered as a multi-mesh scene.
    mesh_scene: Option<mdc_model::DevicePackage>,
    /// N315: which package animation the device poses (default: first).
    mesh_clip: mdc_bake::SceneClip,
    /// N347: texture sampling filter for package scenes (device config).
    texture_filter: mdc_bake::Filter,
    /// N330: the frame frozen at the last mesh-clip change, blended out.
    mesh_prev_pixels: Vec<u8>,
    mesh_crossfade_ms: u32,
    mesh_blend_start: u64,
    /// N178: UVs for the accepted geometry preview, parallel to `preview_xy`.
    preview_uv: Vec<[f32; 2]>,
    /// N176/N178: fill colour for the preview when no texture is sampled.
    preview_color: u16,
    /// N297: whether the accepted material cuts out transparent texels.
    preview_masked: bool,
    anim_clip: Option<String>,
    anim_start_tick: u64,
    /// N154: the frame frozen when the clip last changed, cross-faded out.
    anim_prev_frame: Option<u32>,
    anim_crossfade_ms: u32,
    anim_blend_start: u64,
    /// First accepted mesh preview, in framebuffer pixels.
    preview_xy: Vec<(i16, i16)>,
    preview_indices: Vec<u16>,
    /// N304: device boot identity reported in Capabilities. `restart` bumps it.
    boot_id: u64,
    /// N304: monotonic run id for the current boot, echoed in Acks.
    run_id: u64,
}
impl FakeDevice {
    pub fn from_transport(link: Box<dyn Transport>, profile: BoardProfile) -> Self {
        let pixels = vec![0u8; usize::from(profile.width) * usize::from(profile.height) * 2];
        let overlay_pixels = if profile.overlay_width > 0 && profile.overlay_height > 0 {
            vec![0u8; usize::from(profile.overlay_width) * usize::from(profile.overlay_height) * 2]
        } else {
            Vec::new()
        };
        let device_id = profile.device_id.clone();
        Self {
            link,
            profile,
            pixels,
            overlay_pixels,
            current_frame_id: 0,
            assembler: None,
            clock: VirtualClock::default(),
            faults: FaultInjector::default(),
            rx: Vec::new(),
            max_message: MAX_FRAME_BYTES,
            max_chunk: mdc_protocol::DEFAULT_MAX_CHUNK,
            delayed: Vec::new(),
            connected: true,
            metrics: SimMetrics {
                device_id,
                pixel_bytes: 0,
                ..SimMetrics::default()
            },
            ota_active: false,
            ota_deadline: None,
            firmware: "1.0.0".into(),
            pending_version: None,
            input_after_frame: None,
            active_pointers: BTreeMap::new(),
            busy_until: 0,
            mirror_active: false,
            mirror_mode: String::new(),
            mirror_prev: Vec::new(),
            package_assembler: None,
            package_ready: false,
            config: default_config(),
            scene_nodes: 0,
            scene_present: false,
            scene: None,
            atlas_frames: 0,
            atlas_sprite: (0, 0),
            atlas: None,
            texture: None,
            mesh_scene: None,
            mesh_clip: mdc_bake::SceneClip::First,
            texture_filter: mdc_bake::Filter::Nearest,
            mesh_prev_pixels: Vec::new(),
            mesh_crossfade_ms: 0,
            mesh_blend_start: 0,
            preview_uv: Vec::new(),
            preview_color: 0xF800,
            preview_masked: false,
            anim_clip: None,
            anim_start_tick: 0,
            anim_prev_frame: None,
            anim_crossfade_ms: 0,
            anim_blend_start: 0,
            preview_xy: Vec::new(),
            preview_indices: Vec::new(),
            boot_id: 0x4D44_0001,
            run_id: 1,
        }
    }

    pub fn pair(profile: BoardProfile) -> (MemoryLink, Self) {
        let (host, device) = MemoryLink::pair();
        (host, Self::from_transport(Box::new(device), profile))
    }
    pub fn with_default_profile() -> (MemoryLink, Self) {
        Self::pair(BoardProfile::default())
    }

    /// Replace the byte channel (e.g. after host reconnect with a new MemoryLink pair).
    pub fn replace_transport(&mut self, link: Box<dyn Transport>) {
        self.link = link;
        self.connected = true;
        self.rx.clear();
        self.delayed.clear();
        self.assembler = None;
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn current_frame_id(&self) -> u64 {
        self.current_frame_id
    }
    /// N304: the boot identity reported in Capabilities.
    pub fn boot_id(&self) -> u64 {
        self.boot_id
    }
    /// N304: the current run id echoed in Acks.
    pub fn run_id(&self) -> u64 {
        self.run_id
    }
    /// N304: simulate a device reboot. Bumps `boot_id`/`run_id` and drops the
    /// transient render state so the host can detect the new run.
    pub fn restart(&mut self) {
        self.boot_id = self.boot_id.wrapping_add(1);
        self.run_id = self.run_id.saturating_add(1);
        self.current_frame_id = 0;
        self.assembler = None;
        self.mirror_active = false;
        self.mirror_prev.clear();
        self.stop_animation();
    }
    pub fn firmware(&self) -> &str {
        &self.firmware
    }
    /// Queue one input event to send after the next displayed full frame.
    pub fn emit_input_after_next_frame(&mut self, event: InputEvent) {
        self.input_after_frame = Some(event);
    }
    /// TCP peer helper: one `down` event after the next displayed full frame.
    pub fn emit_down_after_next_frame(&mut self, x: u16, y: u16) {
        self.emit_input_after_next_frame(InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: "down".into(),
            x,
            y,

            node: None,
        });
    }
    fn flush_scripted_input(&mut self) -> Result<(), SimError> {
        if let Some(event) = self.input_after_frame.take() {
            return self.emit_input(event);
        }
        Ok(())
    }
    pub fn pending_version(&self) -> Option<&str> {
        self.pending_version.as_deref()
    }
    pub fn profile(&self) -> &BoardProfile {
        &self.profile
    }
    pub fn metrics_snapshot(&self) -> SimMetrics {
        let mut m = self.metrics.clone();
        m.current_frame_id = self.current_frame_id;
        m.pixel_bytes = self.pixels.len();
        m.device_id = self.profile.device_id.clone();
        m
    }
    pub fn overlay_pixels(&self) -> &[u8] {
        &self.overlay_pixels
    }
    /// N465: read one overlay pixel as RGB565.
    pub fn overlay_pixel_rgb565(&self, x: u16, y: u16) -> Option<u16> {
        if x >= self.profile.overlay_width || y >= self.profile.overlay_height {
            return None;
        }
        let index =
            ((u32::from(y) * u32::from(self.profile.overlay_width) + u32::from(x)) * 2) as usize;
        let bytes = self.overlay_pixels.get(index..index + 2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }
    /// N465: render into the overlay surface (the scene when one is set, else a
    /// white fill). Returns false when the device has no overlay.
    pub fn render_overlay(&mut self) -> bool {
        if self.profile.overlay_width == 0 || self.profile.overlay_height == 0 {
            return false;
        }
        let width = u32::from(self.profile.overlay_width);
        let height = u32::from(self.profile.overlay_height);
        self.overlay_pixels.fill(0);
        match self.scene.clone() {
            Some(mut scene) => {
                mdc_protocol::apply_scene_animation(&mut scene, self.clock.tick);
                let mut drew = false;
                fill_scene(
                    &scene,
                    &mut self.overlay_pixels,
                    width,
                    height,
                    self.atlas.as_ref(),
                    SceneTransform::identity(),
                    &mut drew,
                );
                drew
            }
            None => {
                // A visible placeholder so the surface is not blank.
                for chunk in self.overlay_pixels.chunks_exact_mut(2) {
                    chunk.copy_from_slice(&0xFFFFu16.to_le_bytes());
                }
                true
            }
        }
    }
    fn surface_size(&self, surface_id: &str) -> Option<(u16, u16)> {
        if surface_id == "main" {
            Some((self.profile.width, self.profile.height))
        } else if surface_id == "overlay"
            && self.profile.overlay_width > 0
            && self.profile.overlay_height > 0
        {
            Some((self.profile.overlay_width, self.profile.overlay_height))
        } else {
            None
        }
    }
    fn store_surface_pixels(
        &mut self,
        surface_id: &str,
        width: u16,
        height: u16,
        bytes: Vec<u8>,
    ) -> Result<(), SimError> {
        let Some((expect_w, expect_h)) = self.surface_size(surface_id) else {
            return Err(SimError::Protocol("unknown surface".into()));
        };
        if width != expect_w || height != expect_h {
            return Err(SimError::Protocol("geometry".into()));
        }
        let expected = usize::from(expect_w) * usize::from(expect_h) * 2;
        if bytes.len() != expected {
            return Err(SimError::Protocol("geometry".into()));
        }
        if surface_id == "overlay" {
            self.overlay_pixels = bytes;
        } else {
            self.pixels = bytes;
        }
        Ok(())
    }
    pub fn assert_pixels_eq(&self, expected: &[u8]) {
        assert_eq!(self.pixels, expected, "pixel buffer mismatch");
    }
    /// N316: emit a metric sample on the diagnostics channel. Diagnostics are
    /// independent of the frame/ack path.
    pub fn emit_metric(
        &mut self,
        name: &str,
        value: f64,
        unit: Option<&str>,
    ) -> Result<(), SimError> {
        self.metrics.diagnostics_emitted = self.metrics.diagnostics_emitted.saturating_add(1);
        let report = MetricReport {
            name: name.to_string(),
            value,
            unit: unit.map(str::to_string),
            boot_id: self.boot_id,
            run_id: self.run_id,
            event_seq: self.metrics.diagnostics_emitted,
        };
        self.write_control(MessageType::Metric, 0, &report)
    }
    /// N547: report the applied-tile counter on the diagnostics channel so the
    /// host can reconcile it against its own `tiles_sent`.
    pub fn emit_tiles_received(&mut self) -> Result<(), SimError> {
        let count = self.metrics.tiles_received as f64;
        self.emit_metric(mdc_core::TILES_RECEIVED_METRIC, count, Some("tiles"))
    }
    /// N316: emit a boot notification (firmware + run identity).
    pub fn emit_boot_info(&mut self) -> Result<(), SimError> {
        self.metrics.diagnostics_emitted = self.metrics.diagnostics_emitted.saturating_add(1);
        let boot = BootInfo {
            firmware: self.firmware.clone(),
            boot_id: self.boot_id,
            run_id: self.run_id,
            reset_reason: Some("power-on".into()),
        };
        self.write_control(MessageType::BootInfo, 0, &boot)
    }
    /// N367: emit a crash/fault report on the diagnostics channel.
    pub fn emit_crash(
        &mut self,
        reason: &str,
        task: Option<&str>,
        pc: Option<u32>,
    ) -> Result<(), SimError> {
        self.metrics.diagnostics_emitted = self.metrics.diagnostics_emitted.saturating_add(1);
        let report = mdc_protocol::CrashReport {
            reason: reason.to_string(),
            task: task.map(str::to_string),
            pc,
            boot_id: self.boot_id,
            run_id: self.run_id,
        };
        self.write_control(MessageType::Crash, 0, &report)
    }
    /// N335: emit a structured telemetry event (name + level + string fields).
    pub fn emit_telemetry(
        &mut self,
        name: &str,
        level: Option<&str>,
        fields: std::collections::BTreeMap<String, String>,
    ) -> Result<(), SimError> {
        self.metrics.diagnostics_emitted = self.metrics.diagnostics_emitted.saturating_add(1);
        let event = TelemetryEvent {
            name: name.to_string(),
            level: level.map(str::to_string),
            fields,
            boot_id: self.boot_id,
            run_id: self.run_id,
            event_seq: self.metrics.diagnostics_emitted,
        };
        self.write_control(MessageType::Telemetry, 0, &event)
    }
    /// Emit a touch/input event toward the host session (capability-gated).
    /// N418: the topmost scene node under a device-space point.
    pub fn hit_test(&self, x: u16, y: u16) -> Option<String> {
        let mut scene = self.scene.clone()?;
        // N463: hit-test against the animated geometry.
        mdc_protocol::apply_scene_animation(&mut scene, self.clock.tick);
        hit_test_node(&scene, SceneTransform::identity(), x, y)
    }

    pub fn emit_input(&mut self, event: InputEvent) -> Result<(), SimError> {
        if !self.profile.touch {
            return Err(SimError::Protocol("touch capability disabled".into()));
        }
        let (x, y) = mdc_core::transform_touch(
            self.profile.width,
            self.profile.height,
            self.profile.rotation,
            event.x,
            event.y,
        )
        .map_err(|_| SimError::Protocol("touch out of bounds".into()))?;
        // N418: attach the hit-tested scene node when the caller did not.
        let node = event.node.clone().or_else(|| self.hit_test(x, y));
        let event = InputEvent {
            x,
            y,
            node,
            ..event
        };
        match event.phase.as_str() {
            "up" | "cancel" => {
                self.active_pointers.remove(&event.pointer_id);
            }
            _ => {
                self.active_pointers.insert(
                    event.pointer_id,
                    (event.surface_id.clone(), event.x, event.y),
                );
            }
        }
        self.metrics.inputs_emitted = self.metrics.inputs_emitted.saturating_add(1);
        self.write_control(MessageType::Input, 0, &event)
    }
    fn emit_pointer_cancels(&mut self) -> Result<(), SimError> {
        let pending: Vec<InputEvent> = self
            .active_pointers
            .iter()
            .map(|(pointer_id, (surface_id, x, y))| InputEvent {
                surface_id: surface_id.clone(),
                pointer_id: *pointer_id,
                phase: "cancel".into(),
                x: *x,
                y: *y,

                node: None,
            })
            .collect();
        self.active_pointers.clear();
        for event in pending {
            self.metrics.inputs_emitted = self.metrics.inputs_emitted.saturating_add(1);
            self.write_control(MessageType::Input, 0, &event)?;
        }
        Ok(())
    }
    pub fn capabilities(&self) -> Capabilities {
        let stride = u32::from(self.profile.width) * 2;
        let mut surfaces = vec![Surface {
            id: "main".into(),
            width: self.profile.width,
            height: self.profile.height,
            pixel_format: "RGB565".into(),
            stride,
            rotation: self.profile.rotation,
            preferred_tile_size: self.profile.preferred_tile_size,
            supports_partial_update: self.profile.supports_partial_update,
            supports_full_framebuffer: self.profile.supports_full_framebuffer,
        }];
        if self.profile.overlay_width > 0 && self.profile.overlay_height > 0 {
            surfaces.push(Surface {
                id: "overlay".into(),
                width: self.profile.overlay_width,
                height: self.profile.overlay_height,
                pixel_format: "RGB565".into(),
                stride: u32::from(self.profile.overlay_width) * 2,
                rotation: 0,
                preferred_tile_size: 0,
                supports_partial_update: false,
                supports_full_framebuffer: true,
            });
        }
        Capabilities {
            device_id: self.profile.device_id.clone(),
            firmware: self.firmware.clone(),
            surfaces,
            frame: true,
            tile: true,
            touch: self.profile.touch,
            ota: self.profile.ota,
            max_message: self.max_message as u32,
            max_chunk: self.max_chunk as u32,
            max_in_flight: 1,
            max_fps: if self.profile.color_fps { 30 } else { 0 },
            mirror: if self.profile.mirror {
                let mut mirror = mdc_protocol::MirrorCapability::full_frame(
                    self.profile.width,
                    self.profile.height,
                    self.profile.mirror_fps,
                );
                if self.profile.mirror_dirty_region {
                    mirror.mode.push("dirty-region".into());
                }
                mirror
            } else {
                mdc_protocol::MirrorCapability::disabled()
            },
            local_display_fps: if self.profile.color_fps {
                self.profile.local_display_fps
            } else {
                0
            },
            mirror_fps: if self.profile.mirror {
                self.profile.mirror_fps
            } else {
                0
            },
            mirror_bandwidth: if self.profile.mirror {
                // Advertise 1 byte/px/row budget at the mirror fps; advisory only.
                u64::from(self.profile.width)
                    .saturating_mul(u64::from(self.profile.height))
                    .saturating_mul(2)
                    .saturating_mul(u64::from(self.profile.mirror_fps))
            } else {
                0
            },
            // N27: local renderer capability from the board profile (not inferred
            // from `mirror`).
            renderer: Some(mdc_protocol::RendererCapability {
                sprite_2d: self.profile.renderer_sprite,
                mesh2d: self.profile.renderer_mesh2d,
                mesh3d: self.profile.renderer_mesh3d,
                skeleton: self.profile.renderer_skeleton,
                max_texture: self.profile.renderer_max_texture,
                max_vertices: self.profile.renderer_max_vertices,
            }),
            // N41: supported configuration keys from the board profile.
            config: mdc_protocol::ConfigCapability {
                keys: self.profile.config_keys.clone(),
            },
            scene: mdc_protocol::SceneCapability::enabled(64, 4),
            boot_id: self.boot_id,
            run_id: self.run_id,
        }
    }
    /// Advance virtual clock and process inbound packets.
    pub fn poll(&mut self) -> Result<(), SimError> {
        if self.faults.disconnect {
            self.faults.disconnect = false;
            self.emit_pointer_cancels()?;
            self.connected = false;
            self.link.close();
            return Err(SimError::Disconnected);
        }
        if !self.connected {
            self.link.close();
            return Err(SimError::Disconnected);
        }
        self.expire_ota();
        while let Some(bytes) = self
            .link
            .read()
            .map_err(|e| SimError::Transport(e.to_string()))?
        {
            self.rx.extend(bytes);
        }
        loop {
            match mdc_protocol::decode_stream(&self.rx, self.max_message) {
                mdc_protocol::StreamStep::Packet(packet, consumed) => {
                    self.rx.drain(..consumed);
                    self.enqueue_or_handle(packet)?;
                }
                mdc_protocol::StreamStep::NeedMore { discard } => {
                    if discard > 0 {
                        self.rx.drain(..discard.min(self.rx.len()));
                    }
                    break;
                }
                mdc_protocol::StreamStep::Rejected { error, discard } => {
                    if discard > 0 {
                        self.rx.drain(..discard.min(self.rx.len()));
                    }
                    return Err(SimError::Protocol(error.to_string()));
                }
            }
        }
        self.flush_delayed()?;
        Ok(())
    }
    fn expire_ota(&mut self) {
        let Some(deadline) = self.ota_deadline else {
            return;
        };
        if !self.ota_active || self.clock.tick < deadline {
            return;
        }
        self.pending_version = None;
        self.ota_active = false;
        self.ota_deadline = None;
        self.metrics.ota_rollbacks = self.metrics.ota_rollbacks.saturating_add(1);
    }
    pub fn advance(&mut self, ticks: u64) -> Result<(), SimError> {
        self.clock.advance(ticks);
        self.poll()
    }
    fn enqueue_or_handle(&mut self, packet: Packet) -> Result<(), SimError> {
        if self.faults.armed_drop {
            if self.faults.drop_skip > 0 {
                self.faults.drop_skip -= 1;
            } else {
                self.faults.armed_drop = false;
                self.assembler = None;
                return self.reply_error(packet.request_id, "E_DROPPED", "packet dropped");
            }
        }
        if self.faults.drop_next {
            self.faults.drop_next = false;
            self.assembler = None;
            return Ok(());
        }
        if self.faults.delay_ticks > 0 {
            let ready_at = self.clock.tick.saturating_add(self.faults.delay_ticks);
            self.faults.delay_ticks = 0;
            self.delayed.push(PendingDelay { ready_at, packet });
            return Ok(());
        }
        self.handle_packet(packet)
    }
    fn flush_delayed(&mut self) -> Result<(), SimError> {
        let mut still = Vec::new();
        let mut ready = Vec::new();
        for item in self.delayed.drain(..) {
            if item.ready_at <= self.clock.tick {
                ready.push(item.packet);
            } else {
                still.push(item);
            }
        }
        self.delayed = still;
        for packet in ready {
            self.handle_packet(packet)?;
        }
        Ok(())
    }
    fn handle_packet(&mut self, packet: Packet) -> Result<(), SimError> {
        match packet.kind {
            MessageType::Hello => {
                let hello: Hello = decode_control(&packet.payload)
                    .map_err(|e| SimError::Protocol(e.to_string()))?;
                if hello.major != VERSION.major {
                    return self.reply_error(packet.request_id, "E_VERSION", "unsupported major");
                }
                self.max_message = self
                    .max_message
                    .min(hello.max_message as usize)
                    .min(MAX_FRAME_BYTES);
                if hello.max_chunk > 0 {
                    self.max_chunk = self.max_chunk.min(hello.max_chunk as usize);
                }
                let caps = self.capabilities();
                self.write_control(MessageType::Capabilities, packet.request_id, &caps)
            }
            MessageType::Frame => self.handle_frame(packet),
            MessageType::Tile => self.handle_tile(packet),
            MessageType::Ping => {
                self.write_packet(MessageType::Pong, packet.request_id, Vec::new())
            }
            MessageType::Ota => self.handle_ota(packet),
            MessageType::MirrorCtrl => self.handle_mirror_ctrl(packet),
            MessageType::Package => self.handle_package(packet),
            MessageType::Config => self.handle_config(packet),
            MessageType::Scene => self.handle_scene(packet),
            MessageType::Input => {
                // Host→device input is unusual; reject unless touch is enabled.
                if !self.profile.touch {
                    return self.reply_error(
                        packet.request_id,
                        "E_UNSUPPORTED",
                        "capability disabled",
                    );
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    fn handle_ota(&mut self, packet: Packet) -> Result<(), SimError> {
        if !self.profile.ota {
            return self.reply_error(
                packet.request_id,
                "E_UNSUPPORTED",
                "ota capability disabled",
            );
        }
        let cmd: OtaCommand =
            decode_control(&packet.payload).map_err(|e| SimError::Protocol(e.to_string()))?;
        self.metrics.ota_commands = self.metrics.ota_commands.saturating_add(1);
        let rollback = |device: &mut Self| {
            device.pending_version = None;
            device.ota_active = false;
            device.ota_deadline = None;
            device.metrics.ota_rollbacks = device.metrics.ota_rollbacks.saturating_add(1);
        };
        match cmd.action.as_str() {
            "begin" => {
                if cmd.size.unwrap_or(0) > 1024 * 1024 {
                    return self.reply_error(packet.request_id, "E_OTA", "image exceeds 1 MiB");
                }
                self.pending_version = Some(cmd.version.unwrap_or_else(|| "pending".into()));
                self.ota_active = true;
                self.ota_deadline = Some(self.clock.tick.saturating_add(OTA_CHUNK_TIMEOUT_TICKS));
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
            "chunk" => {
                if cmd.sha256.as_deref() == Some("bad") {
                    rollback(self);
                    return self.reply_error(packet.request_id, "E_OTA", "bad image");
                }
                self.ota_deadline = Some(self.clock.tick.saturating_add(OTA_CHUNK_TIMEOUT_TICKS));
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
            "abort" | "timeout" => {
                rollback(self);
                if cmd.action == "timeout" {
                    self.reply_error(packet.request_id, "E_OTA", "chunk timeout")
                } else {
                    self.ack(packet.request_id, self.current_frame_id, true, false)
                }
            }
            "confirm" => {
                if !self.ota_active {
                    return self.reply_error(packet.request_id, "E_OTA", "no active ota");
                }
                if let Some(version) = self.pending_version.take() {
                    self.firmware = version;
                }
                self.ota_active = false;
                self.ota_deadline = None;
                self.ack(packet.request_id, self.current_frame_id, true, true)
            }
            other => self.reply_error(
                packet.request_id,
                "E_OTA",
                &format!("unknown action {other}"),
            ),
        }
    }
    /// Form B: host asked to start/stop mirroring. Capability-gated.
    fn handle_mirror_ctrl(&mut self, packet: Packet) -> Result<(), SimError> {
        if !self.profile.mirror {
            return self.reply_error(
                packet.request_id,
                "E_UNSUPPORTED",
                "mirror capability disabled",
            );
        }
        let cmd: MirrorCommand =
            decode_control(&packet.payload).map_err(|e| SimError::Protocol(e.to_string()))?;
        self.metrics.mirror_commands = self.metrics.mirror_commands.saturating_add(1);
        match cmd.action.as_str() {
            "start" => {
                // N457: the simulator only rasterises `main`; refuse another
                // surface instead of silently sending the wrong frames.
                if !cmd.surface_id.is_empty() && cmd.surface_id != "main" {
                    return self.reply_error(
                        packet.request_id,
                        "E_UNSUPPORTED",
                        &format!("surface {} is not mirrored", cmd.surface_id),
                    );
                }
                self.mirror_active = true;
                self.mirror_mode = cmd.mode.clone();
                self.mirror_prev.clear();
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
            "stop" => {
                self.mirror_active = false;
                self.mirror_mode.clear();
                self.mirror_prev.clear();
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
            other => self.reply_error(
                packet.request_id,
                "E_MIRROR",
                &format!("unknown action {other}"),
            ),
        }
    }
    /// N23: assemble a host-uploaded runtime package; on commit verify its
    /// integrity and that this device's renderer can run it.
    fn handle_package(&mut self, packet: Packet) -> Result<(), SimError> {
        let payload = decode_package_payload(&packet.payload)
            .map_err(|e| SimError::Protocol(e.to_string()))?;
        match payload {
            PackagePayload::Begin { .. } => {
                self.package_assembler = Some(mdc_model::PackageAssembler::begin());
                Ok(())
            }
            PackagePayload::Chunk { data, .. } => {
                let Some(assembler) = self.package_assembler.as_mut() else {
                    return self.reply_error(packet.request_id, "E_PACKAGE", "chunk before begin");
                };
                if let Err(error) = assembler.push_chunk(&data) {
                    return self.reply_error(packet.request_id, "E_PACKAGE", &error.to_string());
                }
                Ok(())
            }
            PackagePayload::Commit => {
                let Some(assembler) = self.package_assembler.take() else {
                    return self.reply_error(packet.request_id, "E_PACKAGE", "commit before begin");
                };
                let package = match assembler.finish() {
                    Ok(package) => package,
                    Err(error) => {
                        return self.reply_error(packet.request_id, "E_PACKAGE", &error.to_string())
                    }
                };
                let caps = self.capabilities();
                let renderer = mdc_model::RendererCapability::from_wire(caps.renderer.as_ref());
                if let Err(error) = package.accepted_by(&renderer) {
                    return self.reply_error(packet.request_id, "E_PACKAGE", &error.to_string());
                }
                self.package_ready = true;
                if let Some(atlas) = &package.package.atlas {
                    self.atlas_frames = atlas.frames;
                    self.atlas_sprite = (atlas.sprite_width, atlas.sprite_height);
                }
                self.atlas = package.package.atlas.clone();
                self.texture = package.package.texture.clone();
                // N195: keep the whole package for multi-mesh device rendering.
                self.mesh_scene = Some(package.package.clone());
                // N169: prefer full geometry when the package carries it; project
                // it into the framebuffer so the device renders the whole mesh.
                let mesh = package.package.meshes.first();
                self.preview_color = mesh
                    .and_then(|m| m.material)
                    .and_then(|mi| package.package.materials.get(mi))
                    .and_then(|material| material.color_rgb565())
                    .unwrap_or(0xF800);
                self.preview_masked = mesh
                    .and_then(|m| m.material)
                    .and_then(|mi| package.package.materials.get(mi))
                    .is_some_and(|material| material.is_masked());
                self.preview_uv = mesh
                    .and_then(|m| m.geometry.as_ref())
                    .filter(|g| g.uv.len() >= g.positions.len() && !g.positions.is_empty())
                    .map(|g| g.uv.clone())
                    .unwrap_or_default();
                let geometry_preview = mesh
                    .and_then(|m| m.geometry.as_ref())
                    .filter(|g| !g.positions.is_empty())
                    .map(|geometry| {
                        let points = fit_preview(
                            &geometry.positions,
                            u32::from(self.profile.width),
                            u32::from(self.profile.height),
                        );
                        let indices: Vec<u16> = geometry
                            .indices
                            .iter()
                            .take(usize::from(u16::MAX))
                            .map(|i| *i as u16)
                            .collect();
                        (points, indices)
                    });
                if let Some((points, indices)) = geometry_preview {
                    self.preview_xy = points;
                    self.preview_indices = indices;
                } else {
                    self.preview_xy = mesh.map(|m| m.preview_xy.clone()).unwrap_or_default();
                    self.preview_indices =
                        mesh.map(|m| m.preview_indices.clone()).unwrap_or_default();
                }
                self.stop_animation();
                self.metrics.packages_received = self.metrics.packages_received.saturating_add(1);
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
        }
    }
    /// N23: true once a runtime package has been accepted and is render-ready.
    pub fn package_ready(&self) -> bool {
        self.package_ready
    }
    /// N41: read a managed configuration value (test / inspector helper).
    pub fn config_value(&self, key: &str) -> Option<&str> {
        self.config.get(key).map(String::as_str)
    }
    /// N54: node count of the last accepted scene (0 with `has_scene()` false).
    pub fn scene_node_count(&self) -> u32 {
        self.scene_nodes
    }
    pub fn has_scene(&self) -> bool {
        self.scene_present
    }
    /// N61: frames in the last accepted baked atlas (0 if none).
    pub fn atlas_frames(&self) -> u32 {
        self.atlas_frames
    }
    pub fn atlas_sprite(&self) -> (u32, u32) {
        self.atlas_sprite
    }
    /// N71: the atlas frame currently selected by the running clip, if any.
    pub fn current_atlas_frame(&self) -> Option<u32> {
        let atlas = self.atlas.as_ref()?;
        let clip = atlas.clip(self.anim_clip.as_deref()?)?;
        if clip.frames == 0 {
            return Some(clip.start);
        }
        let elapsed_ms = self.clock.tick.saturating_sub(self.anim_start_tick);
        let local = (elapsed_ms.saturating_mul(u64::from(clip.fps)) / 1000) as u32;
        let within = if clip.looping {
            local % clip.frames
        } else {
            local.min(clip.frames - 1)
        };
        Some(clip.start + within)
    }
    pub fn active_clip(&self) -> Option<&str> {
        self.anim_clip.as_deref()
    }
    /// N129: completed loops of the running clip (0 when not looping).
    pub fn animation_loops(&self) -> u32 {
        let Some(atlas) = self.atlas.as_ref() else {
            return 0;
        };
        let Some(clip) = atlas.clip(self.anim_clip.as_deref().unwrap_or("")) else {
            return 0;
        };
        if !clip.looping || clip.frames == 0 || clip.fps == 0 {
            return 0;
        }
        let elapsed_ms = self.clock.tick.saturating_sub(self.anim_start_tick);
        let local = elapsed_ms.saturating_mul(u64::from(clip.fps)) / 1000;
        (local / u64::from(clip.frames)) as u32
    }
    /// N80: rasterize the accepted scene into the local framebuffer (rect fills,
    /// parent then children so children draw on top; bounds clipped). N137: the
    /// framebuffer is cleared first so repeated renders are deterministic.
    /// Returns true when at least one primitive was drawn.
    pub fn render_scene(&mut self) -> bool {
        let Some(mut scene) = self.scene.clone() else {
            return false;
        };
        // N463: sample keyframed scene animation on the device clock.
        mdc_protocol::apply_scene_animation(&mut scene, self.clock.tick);
        let width = u32::from(self.profile.width);
        let height = u32::from(self.profile.height);
        self.pixels.fill(0);
        let mut drew = false;
        fill_scene(
            &scene,
            &mut self.pixels,
            width,
            height,
            self.atlas.as_ref(),
            SceneTransform::identity(),
            &mut drew,
        );
        drew
    }
    /// Draw the accepted mesh preview as a red triangle fan. Returns true when
    /// at least three points were available.
    pub fn render_preview(&mut self) -> bool {
        let width = u32::from(self.profile.width);
        let height = u32::from(self.profile.height);
        // N196: a mesh package is rendered as a whole scene at the device clock.
        if let Some(scene) = self.mesh_scene.clone() {
            if scene.meshes.iter().any(|m| m.geometry.is_some()) {
                let seconds = self.clock.tick as f32 / 1000.0;
                if let Ok(frame) = mdc_bake::render_package_scene_clip_filtered(
                    &scene,
                    self.mesh_clip,
                    seconds,
                    (width, height),
                    self.texture_filter,
                ) {
                    let len = frame.rgb565.len().min(self.pixels.len());
                    self.pixels[..len].copy_from_slice(&frame.rgb565[..len]);
                    // N330: cross-fade the frame frozen at the last clip change.
                    if self.mesh_crossfade_ms > 0 {
                        let elapsed = self.clock.tick.saturating_sub(self.mesh_blend_start);
                        if elapsed >= u64::from(self.mesh_crossfade_ms) {
                            self.mesh_crossfade_ms = 0;
                            self.mesh_prev_pixels.clear();
                        } else if self.mesh_prev_pixels.len() >= len {
                            let progress = (elapsed.saturating_mul(4)
                                / u64::from(self.mesh_crossfade_ms))
                            .min(4) as u8;
                            let quarters = 4u8.saturating_sub(progress);
                            let previous = std::mem::take(&mut self.mesh_prev_pixels);
                            mix_rgb565(&mut self.pixels[..len], &previous[..len], quarters);
                            self.mesh_prev_pixels = previous;
                        }
                    }
                    return true;
                }
            }
        }
        if self.preview_xy.len() < 3 {
            return false;
        }
        let points = self.preview_xy.clone();
        let indices = self.preview_indices.clone();
        // N178: sample the package texture when the geometry carries UVs.
        let textured = self
            .texture
            .clone()
            .filter(|t| t.validate() && self.preview_uv.len() >= points.len());
        let color = self.preview_color;
        if indices.len() >= 3 {
            for tri in indices.chunks_exact(3) {
                let three = (
                    points.get(tri[0] as usize).copied(),
                    points.get(tri[1] as usize).copied(),
                    points.get(tri[2] as usize).copied(),
                );
                let (Some(a), Some(b), Some(c)) = three else {
                    continue;
                };
                match (&textured, self.preview_uv.len() >= points.len()) {
                    (Some(texture), true) => fill_textured_triangle(
                        &mut self.pixels,
                        width,
                        height,
                        a,
                        self.preview_uv[tri[0] as usize],
                        b,
                        self.preview_uv[tri[1] as usize],
                        c,
                        self.preview_uv[tri[2] as usize],
                        texture,
                        color,
                        self.preview_masked,
                    ),
                    _ => fill_triangle(&mut self.pixels, width, height, a, b, c, color),
                }
            }
        } else {
            fill_fan(&mut self.pixels, width, height, &points, color);
        }
        true
    }
    /// N80: RGB565 pixel at (x, y) of the local framebuffer.
    pub fn pixel_rgb565(&self, x: u16, y: u16) -> Option<u16> {
        let width = usize::from(self.profile.width);
        let index = (usize::from(y) * width + usize::from(x)) * 2;
        let slice = self.pixels.get(index..index + 2)?;
        Some(u16::from_le_bytes([slice[0], slice[1]]))
    }
    /// N82: the local framebuffer (RGB565 LE, tightly packed).
    pub fn framebuffer(&self) -> &[u8] {
        &self.pixels
    }
    /// N72: draw the current atlas frame into the local framebuffer (top-left).
    /// Returns true when a frame was drawn.
    pub fn render_atlas_frame(&mut self) -> bool {
        let Some(index) = self.current_atlas_frame() else {
            return false;
        };
        let Some(atlas) = self.atlas.clone() else {
            return false;
        };
        let Some(offset) = atlas.frame_offset(index) else {
            return false;
        };
        let sprite_w = atlas.sprite_width as usize;
        let sprite_h = atlas.sprite_height as usize;
        let row_bytes = sprite_w * 2;
        let atlas_row_bytes = (atlas.columns as usize) * sprite_w * 2;
        let dst_row_bytes = self.profile.width as usize * 2;
        if sprite_w == 0 || sprite_h == 0 || dst_row_bytes == 0 {
            return false;
        }
        let rows = sprite_h.min(self.pixels.len() / dst_row_bytes);
        for y in 0..rows {
            let src = offset + y * atlas_row_bytes;
            let dst = y * dst_row_bytes;
            if src + row_bytes > atlas.rgb565.len() || dst + row_bytes > self.pixels.len() {
                break;
            }
            self.pixels[dst..dst + row_bytes].copy_from_slice(&atlas.rgb565[src..src + row_bytes]);
        }
        if let Some(clip) = atlas.clip(self.anim_clip.as_deref().unwrap_or("")) {
            let elapsed = self.clock.tick.saturating_sub(self.anim_start_tick);
            if clip.fade_in_ms > 0 && elapsed < u64::from(clip.fade_in_ms) {
                let quarters =
                    ((elapsed.saturating_mul(4) / u64::from(clip.fade_in_ms)) as u8).min(3);
                fade_toward_black(&mut self.pixels, quarters);
            }
            if !clip.looping && clip.fade_out_ms > 0 && clip.fps > 0 && clip.frames > 0 {
                let duration = u64::from(clip.frames).saturating_mul(1000) / u64::from(clip.fps);
                let fade = u64::from(clip.fade_out_ms);
                if elapsed + fade >= duration && elapsed < duration {
                    let left = duration.saturating_sub(elapsed);
                    let quarters = ((left.saturating_mul(4) / fade) as u8).min(3);
                    fade_toward_black(&mut self.pixels, quarters);
                }
            }
        }
        // N154: cross-fade the frozen previous frame out over the new clip.
        if self.anim_crossfade_ms > 0 {
            let blend_elapsed = self.clock.tick.saturating_sub(self.anim_blend_start);
            if blend_elapsed >= u64::from(self.anim_crossfade_ms) {
                self.anim_prev_frame = None;
                self.anim_crossfade_ms = 0;
            } else if let Some(previous) = self.anim_prev_frame {
                let progress = (blend_elapsed.saturating_mul(4) / u64::from(self.anim_crossfade_ms))
                    .min(4) as u8;
                let quarters = 4u8.saturating_sub(progress);
                mix_atlas_frame(&mut self.pixels, &atlas, previous, quarters, dst_row_bytes);
            }
        }
        true
    }
    /// N54: accept a vector scene within the advertised limits.
    fn handle_scene(&mut self, packet: Packet) -> Result<(), SimError> {
        let command: mdc_protocol::SceneCommand =
            decode_control(&packet.payload).map_err(|e| SimError::Protocol(e.to_string()))?;
        self.metrics.scene_commands = self.metrics.scene_commands.saturating_add(1);
        match command.action.as_str() {
            "clear" => {
                self.scene_nodes = 0;
                self.scene_present = false;
                self.scene = None;
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
            "set" => {
                let Some(scene) = command.scene else {
                    return self.reply_error(packet.request_id, "E_SCENE", "set without scene");
                };
                let caps = mdc_protocol::SceneCapability::enabled(64, 4);
                if scene.count() > caps.max_nodes || scene.depth() > caps.max_depth {
                    return self.reply_error(packet.request_id, "E_SCENE", "scene over limits");
                }
                self.scene_nodes = scene.count();
                self.scene_present = true;
                self.scene = Some(scene);
                // N83: setting a scene takes over rendering (last-wins).
                self.stop_animation();
                self.ack(packet.request_id, self.current_frame_id, true, false)
            }
            other => self.reply_error(
                packet.request_id,
                "E_SCENE",
                &format!("unknown action {other}"),
            ),
        }
    }
    /// N41: apply a configuration get/set command and reply with ConfigReply.
    fn handle_config(&mut self, packet: Packet) -> Result<(), SimError> {
        let command: ConfigCommand =
            decode_control(&packet.payload).map_err(|e| SimError::Protocol(e.to_string()))?;
        self.metrics.config_commands = self.metrics.config_commands.saturating_add(1);
        // N71: "animation" is handled by the atlas runtime, not the config map.
        if command.key == "animation" {
            return self.handle_animation_config(&packet, &command);
        }
        // N347: "filtering" selects the device texture sampler.
        if command.key == "filtering" {
            return self.handle_filtering_config(&packet, &command);
        }
        if !self.profile.config_keys.iter().any(|k| k == &command.key) {
            return self.reply_config(&command.key, false, None, Some("E_UNSUPPORTED"));
        }
        match command.action.as_str() {
            "get" => {
                let value = self
                    .config
                    .get(&command.key)
                    .cloned()
                    .unwrap_or_else(|| "".to_string());
                let shown = if is_secret_key(&command.key) {
                    "***".to_string()
                } else {
                    value
                };
                self.reply_config(&command.key, true, Some(shown), None)
            }
            "set" => {
                let value = command.value.clone().unwrap_or_default();
                if let Err(error) = validate_config(&command.key, &value) {
                    return self.reply_config(&command.key, false, None, Some(error));
                }
                self.config.insert(command.key.clone(), value.clone());
                let shown = if is_secret_key(&command.key) {
                    "***".to_string()
                } else {
                    value
                };
                self.reply_config(&command.key, true, Some(shown), None)
            }
            _ => self.reply_config(&command.key, false, None, Some("E_ACTION")),
        }
    }
    fn reply_config(
        &mut self,
        key: &str,
        ok: bool,
        value: Option<String>,
        error: Option<&str>,
    ) -> Result<(), SimError> {
        let reply = ConfigReply {
            key: key.to_string(),
            ok,
            value,
            error: error.map(str::to_string),
        };
        self.write_control(MessageType::Config, 0, &reply)
    }
    /// N71: `config animation <clip|off>` starts/stops the atlas runtime.
    /// N315: for an installed mesh package it selects one of the package's own
    /// animations (`<index>|<name>|default|off`) instead.
    fn handle_animation_config(
        &mut self,
        packet: &Packet,
        command: &ConfigCommand,
    ) -> Result<(), SimError> {
        if self
            .mesh_scene
            .as_ref()
            .is_some_and(|scene| scene.meshes.iter().any(|mesh| mesh.geometry.is_some()))
        {
            return self.handle_mesh_clip_config(packet, command);
        }
        match command.action.as_str() {
            "get" => {
                let value = self.anim_clip.clone().unwrap_or_else(|| "off".to_string());
                self.reply_config("animation", true, Some(value), None)
            }
            "set" => {
                if self.atlas.is_none() {
                    return self.reply_error(
                        packet.request_id,
                        "E_UNSUPPORTED",
                        "no baked atlas installed",
                    );
                }
                let value = command.value.clone().unwrap_or_default();
                let crossfade_ms = self
                    .atlas
                    .as_ref()
                    .and_then(|atlas| atlas.clip(&value))
                    .map(|clip| clip.fade_in_ms)
                    .unwrap_or(0);
                let has_clip = self
                    .atlas
                    .as_ref()
                    .map(|atlas| atlas.clip(&value).is_some())
                    .unwrap_or(false);
                if value.is_empty() || value == "off" {
                    self.stop_animation();
                    return self.reply_config("animation", true, Some("off".into()), None);
                }
                if !has_clip {
                    return self.reply_config("animation", false, None, Some("E_VALUE"));
                }
                // N154: cross-fade from the frame frozen when the clip changes.
                let previous = self.current_atlas_frame();
                if self.anim_clip.as_deref() != Some(value.as_str()) {
                    self.anim_prev_frame = previous;
                    self.anim_crossfade_ms = crossfade_ms.min(2000);
                    self.anim_blend_start = self.clock.tick;
                }
                self.anim_clip = Some(value.clone());
                self.anim_start_tick = self.clock.tick;
                self.reply_config("animation", true, Some(value), None)
            }
            _ => self.reply_config("animation", false, None, Some("E_ACTION")),
        }
    }
    /// N154: stop the running clip and clear any cross-fade state.
    pub fn stop_animation(&mut self) {
        self.anim_clip = None;
        self.anim_prev_frame = None;
        self.anim_crossfade_ms = 0;
        // N315: a new package resets the mesh clip selection to the default.
        self.mesh_clip = mdc_bake::SceneClip::First;
        // N330: and clears any in-flight mesh cross-fade.
        self.mesh_prev_pixels.clear();
        self.mesh_crossfade_ms = 0;
    }
    /// Form B: true once a host MirrorCtrl start has been accepted.
    pub fn mirror_active(&self) -> bool {
        self.mirror_active
    }
    /// N315: the mesh package animation currently selected.
    pub fn mesh_clip(&self) -> mdc_bake::SceneClip {
        self.mesh_clip
    }
    /// N347: `config filtering <nearest|linear>` selects the device sampler.
    fn handle_filtering_config(
        &mut self,
        _packet: &Packet,
        command: &ConfigCommand,
    ) -> Result<(), SimError> {
        match command.action.as_str() {
            "get" => {
                let value = if self.texture_filter == mdc_bake::Filter::Linear {
                    "linear"
                } else {
                    "nearest"
                };
                self.reply_config("filtering", true, Some(value.into()), None)
            }
            "set" => {
                let value = command
                    .value
                    .clone()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let filter = match value.as_str() {
                    "linear" => mdc_bake::Filter::Linear,
                    "nearest" => mdc_bake::Filter::Nearest,
                    _ => return self.reply_config("filtering", false, None, Some("E_VALUE")),
                };
                self.texture_filter = filter;
                self.reply_config("filtering", true, Some(value), None)
            }
            _ => self.reply_config("filtering", false, None, Some("E_ACTION")),
        }
    }

    /// N315: select a mesh package animation by index/name; `off` renders static.
    fn handle_mesh_clip_config(
        &mut self,
        _packet: &Packet,
        command: &ConfigCommand,
    ) -> Result<(), SimError> {
        let names: Vec<String> = self
            .mesh_scene
            .as_ref()
            .map(|scene| scene.animations.iter().map(|a| a.name.clone()).collect())
            .unwrap_or_default();
        match command.action.as_str() {
            "get" => {
                let value = match self.mesh_clip {
                    mdc_bake::SceneClip::Static => "off".to_string(),
                    mdc_bake::SceneClip::First => {
                        names.first().cloned().unwrap_or_else(|| "default".into())
                    }
                    mdc_bake::SceneClip::Index(index) => names
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| index.to_string()),
                };
                self.reply_config("animation", true, Some(value), None)
            }
            "set" => {
                let value = command.value.clone().unwrap_or_default();
                let new_clip = if value.is_empty() || value == "off" {
                    Some(mdc_bake::SceneClip::Static)
                } else if value == "default" {
                    Some(mdc_bake::SceneClip::First)
                } else if let Ok(index) = value.parse::<usize>() {
                    (index < names.len()).then_some(mdc_bake::SceneClip::Index(index))
                } else {
                    names
                        .iter()
                        .position(|name| name == &value)
                        .map(mdc_bake::SceneClip::Index)
                };
                let Some(new_clip) = new_clip else {
                    return self.reply_config("animation", false, None, Some("E_VALUE"));
                };
                if new_clip != self.mesh_clip {
                    // N330: freeze the current frame and cross-fade it out.
                    self.begin_mesh_crossfade();
                }
                self.mesh_clip = new_clip;
                let shown = if value.is_empty() {
                    "off".to_string()
                } else {
                    value
                };
                self.reply_config("animation", true, Some(shown), None)
            }
            _ => self.reply_config("animation", false, None, Some("E_ACTION")),
        }
    }
    /// N330: freeze the current framebuffer for a short cross-fade window.
    fn begin_mesh_crossfade(&mut self) {
        self.mesh_prev_pixels = self.pixels.clone();
        self.mesh_crossfade_ms = MESH_CROSSFADE_MS;
        self.mesh_blend_start = self.clock.tick;
    }
    /// N263: the negotiated mirror mode ("full-frame" | "dirty-region").
    pub fn mirror_mode(&self) -> &str {
        &self.mirror_mode
    }
    /// Form B: device pushes the current rendered `main` pixels back to the host.
    /// N264: in `dirty-region` mode only the changed byte ranges are chunked.
    pub fn emit_mirror_frame(&mut self) -> Result<(), SimError> {
        if !self.profile.mirror {
            return Err(SimError::Protocol("mirror capability disabled".into()));
        }
        // N196: a mesh package renders as a scene; else the clip; else the scene.
        if self
            .mesh_scene
            .as_ref()
            .is_some_and(|s| s.meshes.iter().any(|m| m.geometry.is_some()))
        {
            let _ = self.render_preview();
        } else if self.anim_clip.is_some() {
            let _ = self.render_atlas_frame();
        } else if self.scene.is_some() {
            let _ = self.render_scene();
        }
        self.metrics.mirror_frames = self.metrics.mirror_frames.saturating_add(1);
        let frame_id = self.current_frame_id;
        if self.mirror_mode == "dirty-region" && self.mirror_prev.len() == self.pixels.len() {
            let ranges = changed_ranges(&self.mirror_prev, &self.pixels, 16);
            let begin = mdc_protocol::encode_frame_begin(
                "main",
                frame_id,
                self.profile.width,
                self.profile.height,
                self.pixels.len() as u32,
            )
            .map_err(|e| SimError::Protocol(e.to_string()))?;
            self.write_packet(MessageType::MirrorFrame, 0, begin)?;
            for (offset, end) in ranges {
                let mut start = offset;
                while start < end {
                    let stop = (start + self.max_chunk).min(end);
                    let chunk = mdc_protocol::encode_frame_chunk(
                        frame_id,
                        start as u32,
                        &self.pixels[start..stop],
                    );
                    self.write_packet(MessageType::MirrorFrame, 0, chunk)?;
                    start = stop;
                }
            }
            let commit = mdc_protocol::encode_frame_commit(frame_id);
            self.write_packet(MessageType::MirrorFrame, 0, commit)?;
        } else {
            let payload = mdc_protocol::encode_frame_payload(
                "main",
                frame_id,
                self.profile.width,
                self.profile.height,
                &self.pixels,
            )
            .map_err(|e| SimError::Protocol(e.to_string()))?;
            self.write_packet(MessageType::MirrorFrame, 0, payload)?;
        }
        self.mirror_prev.clear();
        self.mirror_prev.extend_from_slice(&self.pixels);
        Ok(())
    }
    fn note_refresh(&mut self) {
        if self.profile.color_fps {
            return;
        }
        self.busy_until = self.clock.tick.saturating_add(100);
    }
    fn handle_frame(&mut self, packet: Packet) -> Result<(), SimError> {
        if self.ota_active {
            return self.reply_error(packet.request_id, "E_FROZEN", "render frozen during ota");
        }
        if !self.profile.color_fps && self.busy_until > self.clock.tick {
            return self.reply_error(packet.request_id, "E_BUSY", "panel is refreshing");
        }
        let decoded =
            decode_frame_payload(&packet.payload).map_err(|e| SimError::Protocol(e.to_string()))?;
        match decoded {
            FramePayload::Legacy {
                surface_id,
                frame_id,
                width,
                height,
                bytes,
            } => {
                if self
                    .store_surface_pixels(&surface_id, width, height, bytes)
                    .is_err()
                {
                    return self.reply_error(packet.request_id, "E_DIM", "geometry mismatch");
                }
                self.current_frame_id = frame_id;
                self.assembler = None;
                self.note_refresh();
                self.metrics.frames_received = self.metrics.frames_received.saturating_add(1);
                self.ack(packet.request_id, frame_id, true, true)?;
                self.flush_scripted_input()
            }
            FramePayload::Begin {
                surface_id,
                frame_id,
                width,
                height,
                total_bytes,
            } => {
                if self.surface_size(&surface_id) != Some((width, height)) {
                    return self.reply_error(packet.request_id, "E_DIM", "geometry mismatch");
                }
                let asm = FrameAssembler::begin(frame_id, total_bytes as usize, self.max_chunk)
                    .map_err(|e| SimError::Protocol(e.to_string()))?;
                self.assembler = Some((surface_id, width, height, asm));
                Ok(())
            }
            FramePayload::Chunk {
                frame_id,
                offset,
                data,
            } => {
                let Some((_, _, _, asm)) = self.assembler.as_mut() else {
                    return self.reply_error(packet.request_id, "E_CHUNK", "no begin");
                };
                if asm.frame_id != frame_id {
                    return self.reply_error(packet.request_id, "E_CHUNK", "frame_id mismatch");
                }
                if let Err(nack) = asm.chunk(offset as usize, &data) {
                    return self.reply_error(packet.request_id, "E_CHUNK", &format!("{nack:?}"));
                }
                Ok(())
            }
            FramePayload::Commit { frame_id } => {
                let Some((_sid, _w, _h, asm)) = self.assembler.take() else {
                    return self.reply_error(packet.request_id, "E_COMMIT", "no begin");
                };
                if asm.frame_id != frame_id {
                    return self.reply_error(packet.request_id, "E_COMMIT", "frame_id mismatch");
                }
                match asm.commit() {
                    Ok(bytes) => {
                        if self.store_surface_pixels(&_sid, _w, _h, bytes).is_err() {
                            return self.reply_error(
                                packet.request_id,
                                "E_DIM",
                                "geometry mismatch",
                            );
                        }
                        self.current_frame_id = frame_id;
                        self.note_refresh();
                        self.metrics.frames_received =
                            self.metrics.frames_received.saturating_add(1);
                        self.ack(packet.request_id, frame_id, true, true)?;
                        self.flush_scripted_input()
                    }
                    Err(TransactionNack::Incomplete) => {
                        self.reply_error(packet.request_id, "E_INCOMPLETE", "incomplete frame")
                    }
                    Err(other) => {
                        self.reply_error(packet.request_id, "E_COMMIT", &format!("{other:?}"))
                    }
                }
            }
        }
    }
    fn handle_tile(&mut self, packet: Packet) -> Result<(), SimError> {
        if self.ota_active {
            return self.reply_error(packet.request_id, "E_FROZEN", "render frozen during ota");
        }
        let tile =
            decode_tile_payload(&packet.payload).map_err(|e| SimError::Protocol(e.to_string()))?;
        let core_tile = Tile {
            surface_id: tile.surface_id,
            base_frame_id: tile.base_frame_id,
            x: tile.x,
            y: tile.y,
            width: tile.w,
            height: tile.h,
            bytes: tile.bytes,
        };
        if let Err(TransactionNack::BaseFrameMismatch) =
            validate_tile_base(&core_tile, self.current_frame_id)
        {
            return self.reply_error(packet.request_id, "E_BASE", "base frame mismatch");
        }
        let (buffer_width, overlay) = if core_tile.surface_id == "overlay" {
            if self.overlay_pixels.is_empty() {
                return self.reply_error(packet.request_id, "E_DIM", "unknown surface");
            }
            (self.profile.overlay_width, true)
        } else if core_tile.surface_id == "main" {
            (self.profile.width, false)
        } else {
            return self.reply_error(packet.request_id, "E_DIM", "unknown surface");
        };
        let buffer_height = if overlay {
            self.profile.overlay_height
        } else {
            self.profile.height
        };
        if u32::from(core_tile.x) + u32::from(core_tile.width) > u32::from(buffer_width)
            || u32::from(core_tile.y) + u32::from(core_tile.height) > u32::from(buffer_height)
        {
            return self.reply_error(packet.request_id, "E_DIM", "tile outside surface");
        }
        let stride = usize::from(buffer_width) * 2;
        let row_bytes = usize::from(core_tile.width) * 2;
        let pixels = if overlay {
            &mut self.overlay_pixels
        } else {
            &mut self.pixels
        };
        // N489: an e-ink partial refresh leaves a faint ghost of the old pixel.
        let ghost = !self.profile.color_fps && !overlay;
        for row in 0..usize::from(core_tile.height) {
            let dst_y = usize::from(core_tile.y) + row;
            let dst_off = dst_y * stride + usize::from(core_tile.x) * 2;
            let src_off = row * row_bytes;
            let incoming = &core_tile.bytes[src_off..src_off + row_bytes];
            if ghost {
                for col in (0..row_bytes).step_by(2) {
                    let old =
                        u16::from_le_bytes([pixels[dst_off + col], pixels[dst_off + col + 1]]);
                    let new = u16::from_le_bytes([incoming[col], incoming[col + 1]]);
                    let mixed = ghost_mix(old, new);
                    pixels[dst_off + col..dst_off + col + 2].copy_from_slice(&mixed.to_le_bytes());
                }
            } else {
                pixels[dst_off..dst_off + row_bytes].copy_from_slice(incoming);
            }
        }
        self.metrics.tiles_received = self.metrics.tiles_received.saturating_add(1);
        self.ack(packet.request_id, self.current_frame_id, true, true)
    }
    fn ack(
        &mut self,
        request_id: u32,
        frame_id: u64,
        received: bool,
        displayed: bool,
    ) -> Result<(), SimError> {
        self.metrics.acks_sent = self.metrics.acks_sent.saturating_add(1);
        let ack = Ack {
            received,
            displayed,
            frame_id,
            error: None,
            run_id: self.run_id,
            event_seq: self.metrics.acks_sent,
        };
        self.write_control(MessageType::Ack, request_id, &ack)
    }
    fn reply_error(&mut self, request_id: u32, code: &str, message: &str) -> Result<(), SimError> {
        self.metrics.errors_sent = self.metrics.errors_sent.saturating_add(1);
        let err = ErrorPayload {
            code: code.into(),
            message: message.into(),
        };
        self.write_control(MessageType::Error, request_id, &err)
    }
    fn write_control<T: serde::Serialize>(
        &mut self,
        kind: MessageType,
        request_id: u32,
        value: &T,
    ) -> Result<(), SimError> {
        let payload = encode_control(value).map_err(|e| SimError::Protocol(e.to_string()))?;
        self.write_packet(kind, request_id, payload)
    }
    fn write_packet(
        &mut self,
        kind: MessageType,
        request_id: u32,
        payload: Vec<u8>,
    ) -> Result<(), SimError> {
        if !self.connected {
            return Err(SimError::Disconnected);
        }
        let packet = Packet {
            version: VERSION,
            kind,
            flags: 0,
            request_id,
            payload,
        };
        let bytes = packet
            .encode(self.max_message)
            .map_err(|e| SimError::Protocol(e.to_string()))?;
        match self.link.write(&bytes) {
            Ok(()) => Ok(()),
            Err(TransportError::Closed) => {
                self.connected = false;
                Err(SimError::Disconnected)
            }
            Err(e) => Err(SimError::Transport(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdc_core::{ConnectionState, Frame, Session, Tile};
    use mdc_protocol::OtaCommand;
    use mdc_transport::MemoryLink;

    #[test]
    fn config_set_get_validate_and_redact() {
        let (host, mut device) = FakeDevice::pair(BoardProfile::default());
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        session.set_config("brightness", "200").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(session.config("brightness"), Some("200"));
        assert_eq!(device.config_value("brightness"), Some("200"));

        // An invalid value is rejected by the device and keeps the old value.
        session.set_config("brightness", "999").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(session.config("brightness"), Some("200"));

        // A secret is never echoed back.
        session.get_config("wifi_password").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(session.config("wifi_password"), Some("***"));
        assert_eq!(device.config_value("wifi_password"), Some("mdc-sim-secret"));
        assert!(device.metrics.config_commands >= 3);
    }

    #[test]
    fn scene_set_and_clear_are_capability_gated() {
        use mdc_protocol::SceneNode;
        let (host, mut device) = FakeDevice::pair(BoardProfile::default());
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        let leaf = |name: &str, kind: &str, value: Option<&str>| SceneNode {
            name: name.into(),
            kind: kind.into(),
            rect: None,
            children: Vec::new(),
            value: value.map(str::to_string),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        let mut root = leaf("root", "rect", None);
        root.children.push(leaf("label", "text", Some("hi")));
        session.set_scene(root).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.has_scene());
        assert_eq!(device.scene_node_count(), 2);

        session.clear_scene().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(!device.has_scene());
        assert!(device.metrics.scene_commands >= 2);
    }

    #[test]
    fn accepts_a_baked_sprites_package_and_records_the_grid() {
        let (host, mut device) = FakeDevice::pair(BoardProfile::default());
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        let atlas = mdc_model::AtlasBlob {
            sprite_width: 2,
            sprite_height: 2,
            frames: 2,
            columns: 2,
            rows: 1,
            rgb565: vec![0u8; 2 * 2 * 2 * 2],
            clips: Vec::new(),
            mips: Vec::new(),
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
            estimated_bytes: 16,
            atlas: Some(atlas),
            texture: None,
            texture_blobs: Vec::new(),
        };
        let sealed = mdc_model::MdcPackage::seal(package).unwrap();
        session
            .install_package(&sealed.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.package_ready());
        assert_eq!(device.atlas_frames(), 2);
        assert_eq!(device.atlas_sprite(), (2, 2));
    }

    #[test]
    fn atlas_animation_runtime_indexes_frames_by_the_clock() {
        let (host, mut device) = FakeDevice::pair(BoardProfile::default());
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        // Install a baked atlas with a 4-frame looping clip.
        let atlas = mdc_model::AtlasBlob {
            sprite_width: 2,
            sprite_height: 2,
            frames: 4,
            columns: 2,
            rows: 2,
            rgb565: vec![0u8; 2 * 2 * 4 * 2],
            clips: vec![mdc_model::AtlasClip {
                name: "idle".into(),
                start: 0,
                frames: 4,
                fps: 30,
                looping: true,
                fade_in_ms: 0,
                fade_out_ms: 0,
            }],

            mips: Vec::new(),
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
        let sealed = mdc_model::MdcPackage::seal(package).unwrap();
        session
            .install_package(&sealed.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.package_ready());
        assert_eq!(device.current_atlas_frame(), None, "no clip running yet");

        session.set_config("animation", "idle").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.active_clip(), Some("idle"));
        assert_eq!(device.current_atlas_frame(), Some(0));
        assert_eq!(device.animation_loops(), 0);

        // 40ms at 30fps -> frame 1; 4 frames loop.
        device.clock.advance(40);
        assert_eq!(device.current_atlas_frame(), Some(1));
        assert_eq!(device.animation_loops(), 0);
        device.clock.advance(4 * 1000);
        assert_eq!(device.current_atlas_frame(), Some(1));
        assert!(
            device.animation_loops() >= 1,
            "the clip wrapped at least once"
        );

        session.set_config("animation", "off").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.current_atlas_frame(), None);
    }

    #[test]
    fn animation_crossfades_between_clips() {
        let (host, mut device) = FakeDevice::pair(BoardProfile::default());
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        // A 2-frame atlas: frame 0 red, frame 1 green. "wave" cross-fades in 1s.
        let atlas = mdc_model::AtlasBlob {
            sprite_width: 1,
            sprite_height: 1,
            frames: 2,
            columns: 2,
            rows: 1,
            rgb565: vec![0x00, 0xF8, 0xE0, 0x07],
            clips: vec![
                mdc_model::AtlasClip {
                    name: "idle".into(),
                    start: 0,
                    frames: 1,
                    fps: 1,
                    looping: true,
                    fade_in_ms: 0,
                    fade_out_ms: 0,
                },
                mdc_model::AtlasClip {
                    name: "wave".into(),
                    start: 1,
                    frames: 1,
                    fps: 1,
                    looping: true,
                    fade_in_ms: 1000,
                    fade_out_ms: 0,
                },
            ],

            mips: Vec::new(),
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
            estimated_bytes: 8,
            atlas: Some(atlas),
            texture: None,
            texture_blobs: Vec::new(),
        };
        session
            .install_package(
                &mdc_model::MdcPackage::seal(package)
                    .unwrap()
                    .to_bytes()
                    .unwrap(),
            )
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();

        session.set_config("animation", "idle").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_atlas_frame());
        assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800), "idle is red");

        // Switching to wave: at t=0 the frozen idle frame still wins.
        session.set_config("animation", "wave").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_atlas_frame());
        assert_eq!(
            device.pixel_rgb565(0, 0),
            Some(0xF800),
            "old frame covers at t=0"
        );

        // After the 1s window the cross-fade has finished: pure green.
        device.clock.advance(1000);
        assert!(device.render_atlas_frame());
        assert_eq!(
            device.pixel_rgb565(0, 0),
            Some(0x07E0),
            "new frame after blend"
        );
    }

    #[test]
    fn device_renders_the_running_atlas_frame_into_the_mirror() {
        let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        // A 4x2 atlas: row = [frame0, frame0, frame1, frame1] (red, green).
        let mut pixels = Vec::new();
        for _row in 0..2 {
            for value in [0xF800u16, 0xF800u16, 0x07E0u16, 0x07E0u16] {
                pixels.push((value & 0xff) as u8);
                pixels.push((value >> 8) as u8);
            }
        }
        let atlas = mdc_model::AtlasBlob {
            sprite_width: 2,
            sprite_height: 2,
            frames: 2,
            columns: 2,
            rows: 1,
            rgb565: pixels,
            clips: vec![mdc_model::AtlasClip {
                name: "idle".into(),
                start: 0,
                frames: 2,
                fps: 30,
                looping: false,
                fade_in_ms: 0,
                fade_out_ms: 0,
            }],

            mips: Vec::new(),
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
        let sealed = mdc_model::MdcPackage::seal(package).unwrap();
        session
            .install_package(&sealed.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();

        session.start_mirror("main", "full-frame", 15).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session.set_config("animation", "idle").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();

        // 40ms at 30fps -> frame 1 (green).
        device.clock.advance(40);
        assert_eq!(device.current_atlas_frame(), Some(1));
        device.emit_mirror_frame().unwrap();
        session.poll().unwrap();
        let frame = session.take_mirror_frame().unwrap();
        assert_eq!(&frame.bytes[0..2], &[0xE0, 0x07], "second frame is green");
    }

    #[test]
    fn scene_renders_rects_with_bounds_and_z_order() {
        use mdc_protocol::SceneNode;
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        let rect = |name: &str, geom: [u16; 4], color: &str| SceneNode {
            name: name.into(),
            kind: "rect".into(),
            rect: Some(geom),
            children: Vec::new(),
            value: Some(color.into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        // Background red covers all; a green child on top; a third clipped.
        let scene = SceneNode {
            name: "root".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 4, 4]),
            children: vec![
                rect("green", [1, 1, 1, 1], "#00FF00"),
                rect("clip", [3, 3, 10, 10], "#0000FF"),
            ],
            value: Some("#FF0000".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        session.set_scene(scene).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800), "red background");
        assert_eq!(device.pixel_rgb565(1, 1), Some(0x07E0), "green on top");
        assert_eq!(
            device.pixel_rgb565(3, 3),
            Some(0x001F),
            "clipped blue corner"
        );
    }

    #[test]
    fn scene_draws_text_and_image_pixels() {
        use mdc_protocol::SceneNode;
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 16,
            height: 8,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        let scene = SceneNode {
            name: "root".into(),
            kind: "group".into(),
            rect: None,
            value: None,
            children: vec![
                SceneNode {
                    name: "label".into(),
                    kind: "text".into(),
                    rect: Some([0, 0, 0, 0]),
                    value: Some("H".into()),
                    children: Vec::new(),
                    opacity: None,
                    visible: None,
                    anim: None,
                    rotate: None,
                    blend: None,
                    align: None,
                    z: None,
                    wrap: None,
                    mode: None,
                    focus: None,
                    value2: None,
                },
                SceneNode {
                    name: "swatch".into(),
                    kind: "image".into(),
                    rect: Some([12, 0, 1, 1]),
                    value: Some("07e0".into()),
                    children: Vec::new(),
                    opacity: None,
                    visible: None,
                    anim: None,
                    rotate: None,
                    blend: None,
                    align: None,
                    z: None,
                    wrap: None,
                    mode: None,
                    focus: None,
                    value2: None,
                },
            ],
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        session.set_scene(scene).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        assert_ne!(device.pixel_rgb565(0, 0), Some(0));
        assert_eq!(device.pixel_rgb565(12, 0), Some(0x07e0));
    }

    #[test]
    fn image_node_scales_with_nearest_and_bilinear() {
        // N556: a 2x1 source (red, blue) scaled to a 4x1 rect.
        use mdc_protocol::SceneNode;
        let render = |mode: Option<&str>| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width: 4,
                height: 1,
                ..BoardProfile::default()
            });
            device.scene = Some(SceneNode {
                name: "pic".into(),
                kind: "image".into(),
                rect: Some([0, 0, 4, 1]),
                value: Some("f800001f".into()),
                value2: Some("2,1".into()),
                children: Vec::new(),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: mode.map(str::to_string),
                focus: None,
            });
            assert!(device.render_scene());
            (0..4)
                .map(|x| device.pixel_rgb565(x, 0).unwrap())
                .collect::<Vec<_>>()
        };
        // Nearest: two columns of red then two of blue.
        let nearest = render(None);
        assert_eq!(nearest, vec![0xF800, 0xF800, 0x001F, 0x001F]);
        // Bilinear: the interior column is a blend, not an exact endpoint colour.
        let linear = render(Some("linear"));
        assert_ne!(linear[1], 0xF800, "bilinear blends");
        assert_ne!(linear[1], 0x001F, "bilinear blends");
        assert_eq!(linear[0], 0xF800, "the first column is still red");
        assert_eq!(linear[3], 0x001F, "the last column is still blue");
    }

    #[test]
    fn image_node_supports_flip_and_rotate() {
        // N565: `mode` flip tokens and `rotate` transform the sampled image.
        use mdc_protocol::SceneNode;
        let render = |mode: Option<&str>, rotate: Option<u16>| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width: 4,
                height: 1,
                ..BoardProfile::default()
            });
            device.scene = Some(SceneNode {
                name: "pic".into(),
                kind: "image".into(),
                rect: Some([0, 0, 4, 1]),
                value: Some("f800001f".into()),
                value2: Some("2,1".into()),
                children: Vec::new(),
                opacity: None,
                visible: None,
                anim: None,
                rotate,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: mode.map(str::to_string),
                focus: None,
            });
            assert!(device.render_scene());
            (0..4)
                .map(|x| device.pixel_rgb565(x, 0).unwrap())
                .collect::<Vec<_>>()
        };
        // Baseline: red then blue.
        assert_eq!(render(None, None), vec![0xF800, 0xF800, 0x001F, 0x001F]);
        // Horizontal flip swaps the halves.
        assert_eq!(
            render(Some("flip-h"), None),
            vec![0x001F, 0x001F, 0xF800, 0xF800]
        );
        // A 180° rotation on a 1-row image matches the horizontal flip.
        assert_eq!(
            render(None, Some(180)),
            vec![0x001F, 0x001F, 0xF800, 0xF800]
        );
    }

    #[test]
    fn image_fit_preserves_aspect_ratio() {
        // N575: `fit` letterboxes a 1x1 source into a 4x1 rect (only the centre
        // column is drawn); stretching fills every column.
        use mdc_protocol::SceneNode;
        let render = |mode: &str, align: Option<&str>| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width: 4,
                height: 1,
                ..BoardProfile::default()
            });
            device.scene = Some(SceneNode {
                name: "pic".into(),
                kind: "image".into(),
                rect: Some([0, 0, 4, 1]),
                value: Some("f800".into()),
                value2: Some("1,1".into()),
                children: Vec::new(),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: align.map(str::to_string),
                z: None,
                wrap: None,
                mode: Some(mode.into()),
                focus: None,
            });
            assert!(device.render_scene());
            (0..4)
                .map(|x| device.pixel_rgb565(x, 0).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(render("", None), vec![0xF800, 0xF800, 0xF800, 0xF800]);
        assert_eq!(render("fit", None), vec![0, 0xF800, 0, 0]);
        // N585: `align` places the letterbox left/right.
        assert_eq!(render("fit", Some("left")), vec![0xF800, 0, 0, 0]);
        assert_eq!(render("fit", Some("right")), vec![0, 0, 0, 0xF800]);
    }

    #[test]
    fn glyphs_cover_digits_letters_and_punctuation() {
        for ch in ".,:!?-+/()=*#".bytes() {
            assert!(glyph_columns(ch).is_some(), "missing {}", ch as char);
        }
        // N476: lowercase has its own glyph (not the uppercase one).
        assert_ne!(glyph_columns(b'a'), glyph_columns(b'A'));
        assert!(glyph_columns(b'z').is_some());
        assert_eq!(glyph_columns(b' '), Some([0, 0, 0, 0, 0]));
        // N494: the extended punctuation renders too.
        for ch in "\"'%&@;<>~".bytes() {
            assert!(glyph_columns(ch).is_some(), "missing {}", ch as char);
        }
    }

    #[test]
    fn scene_text_scales_with_the_rect() {
        use mdc_protocol::SceneNode;
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 24,
            height: 16,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        let text = |rect: [u16; 4]| SceneNode {
            name: "label".into(),
            kind: "text".into(),
            rect: Some(rect),
            value: Some("H".into()),
            children: Vec::new(),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        // rect 0×0 -> scale 1, so (1,1) stays clear on the fresh framebuffer.
        session.set_scene(text([0, 0, 0, 0])).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        assert_eq!(
            device.pixel_rgb565(1, 1),
            Some(0),
            "scale 1 leaves (1,1) clear"
        );

        // rect 12×14 -> 2× glyph scale, so the scaled pixel lights up.
        session.set_scene(text([0, 0, 12, 14])).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        assert_ne!(device.pixel_rgb565(1, 1), Some(0), "scale 2 fills (1,1)");
    }

    #[test]
    fn text_wraps_to_the_rect_width() {
        assert_eq!(
            wrap_text("ab\ncd", 10),
            vec!["ab".to_string(), "cd".to_string()]
        );
        assert_eq!(
            wrap_text("hello world", 7),
            vec!["hello".to_string(), "world".to_string()]
        );
        // A word longer than the line is hard-broken.
        assert_eq!(
            wrap_text("abcdef", 3),
            vec!["abc".to_string(), "def".to_string()]
        );
        // A blank paragraph keeps a blank line.
        assert_eq!(
            wrap_text("a\n\nb", 5),
            vec!["a".to_string(), "".to_string(), "b".to_string()]
        );
        // Zero width still makes progress one char per line.
        assert_eq!(wrap_text("ab", 0), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn scene_text_draws_wrapped_lines() {
        use mdc_protocol::SceneNode;
        // 40×30 box, 30-wide text rect -> scale 1, 5 columns; two lines.
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 40,
            height: 30,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        let scene = SceneNode {
            name: "label".into(),
            kind: "text".into(),
            rect: Some([0, 0, 30, 7]),
            value: Some("hello world".into()),
            children: Vec::new(),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        session.set_scene(scene).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        // Line 1 at y=0, line 2 at y=8 (line height 8 at scale 1).
        // N476: pixel positions depend on the glyph, so scan each line's band.
        let line_lit = |y0: u16, y1: u16| {
            (y0..y1).any(|y| (0..30).any(|x| device.pixel_rgb565(x, y) != Some(0)))
        };
        assert!(line_lit(0, 7), "first line drawn");
        assert!(line_lit(8, 15), "wrapped line drawn");
        // Row 7 is the gap between the two lines and stays clear.
        assert_eq!(device.pixel_rgb565(0, 7), Some(0), "line gap is clear");
    }

    #[test]
    fn scene_rect_opacity_blends_over_the_background() {
        use mdc_protocol::SceneNode;
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        let rect = |name: &str, color: &str, opacity: Option<u8>| SceneNode {
            name: name.into(),
            kind: "rect".into(),
            rect: Some([0, 0, 4, 4]),
            value: Some(color.into()),
            opacity,
            value2: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        };
        device.scene = Some(SceneNode {
            name: "root".into(),
            kind: "group".into(),
            rect: None,
            value: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: vec![
                rect("bg", "#FFFFFF", None),
                rect("fg", "#FF0000", Some(128)),
            ],
            value2: None,
        });
        assert!(device.render_scene());
        let px = device.pixel_rgb565(1, 1).unwrap();
        // White blended half-way to red: not pure red, not white.
        assert_ne!(px, 0xF800);
        assert_ne!(px, 0xFFFF);
        assert!((px >> 11) > 20, "red channel is high: {px:#06x}");
    }

    #[test]
    fn device_filtering_config_sets_the_sampler() {
        let (host, mut device) = FakeDevice::pair(BoardProfile::default());
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.texture_filter, mdc_bake::Filter::Nearest);
        session.set_config("filtering", "linear").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.texture_filter, mdc_bake::Filter::Linear);
        session.set_config("filtering", "bogus").unwrap();
        device.poll().unwrap();
        let _ = session.poll();
        assert_eq!(
            device.texture_filter,
            mdc_bake::Filter::Linear,
            "an unknown filter leaves the sampler unchanged"
        );
    }

    #[test]
    fn scene_gradient_interpolates_top_to_bottom() {
        use mdc_protocol::SceneNode;
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 1,
            height: 8,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "g".into(),
            kind: "gradient".into(),
            rect: Some([0, 0, 1, 8]),
            value: Some("#FF0000".into()),
            value2: Some("#0000FF".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        });
        assert!(device.render_scene());
        assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800), "top is red");
        assert_eq!(device.pixel_rgb565(0, 7), Some(0x001F), "bottom is blue");
        let mid = device.pixel_rgb565(0, 4).unwrap();
        assert!(
            (mid >> 11) > 0 && (mid & 0x1f) > 0,
            "mid blended: {mid:#06x}"
        );
    }

    #[test]
    fn eink_full_refresh_clears_ghosting() {
        // N499: repeated partial refreshes fade the ghost; a full frame is exact.
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 2,
            height: 2,
            color_fps: false,
            supports_partial_update: true,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        let frame = |bytes: Vec<u8>| mdc_core::Frame {
            surface_id: "main".into(),
            width: 2,
            height: 2,
            bytes,
        };
        // Start from a clean black panel.
        session.send_frame(frame(vec![0u8; 8])).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixel_rgb565(0, 0).unwrap(), 0);
        let white_tile = |base: u64| mdc_core::Tile {
            surface_id: "main".into(),
            base_frame_id: base,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0xff, 0xff],
        };
        // A white partial refresh leaves a ghost...
        device.advance(200).unwrap();
        let base = device.current_frame_id();
        session.send_tile(white_tile(base)).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let after_first = device.pixel_rgb565(0, 0).unwrap();
        assert!(
            after_first != 0 && after_first != 0xffff,
            "partial refresh ghosts: {after_first:#06x}"
        );
        // ...that fades with a second identical refresh.
        device.advance(200).unwrap();
        session.send_tile(white_tile(base)).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let after_second = device.pixel_rgb565(0, 0).unwrap();
        assert!(
            after_second > after_first,
            "the ghost fades toward the target ({after_first:#06x} -> {after_second:#06x})"
        );
        // A full white frame clears it exactly.
        device.advance(200).unwrap();
        session.send_frame(frame(vec![0xff; 8])).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(
            device.pixel_rgb565(0, 0).unwrap(),
            0xffff,
            "a full refresh is exact"
        );
    }

    #[test]
    fn eink_tiles_leave_a_ghost() {
        // N489: a white tile over black is not fully white on e-ink.
        let apply = |board: &str| {
            let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id(board));
            let mut session = Session::new(host);
            session.connect().unwrap();
            device.poll().unwrap();
            assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
            let _ = session.send_tile(mdc_core::Tile {
                surface_id: "main".into(),
                base_frame_id: device.current_frame_id(),
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0xff, 0xff],
            });
            device.poll().unwrap();
            session.poll().unwrap();
            device.pixel_rgb565(0, 0).unwrap()
        };
        let color = apply("linux-virt");
        assert_eq!(color, 0xffff, "a colour panel replaces the pixel");
        let eink = apply("waveshare-eink");
        assert!(
            eink != 0 && eink != 0xffff,
            "e-ink keeps a ghost: {eink:#06x}"
        );
    }

    #[test]
    fn z_order_overrides_document_order() {
        // N485: a higher `z` draws on top regardless of document order.
        use mdc_protocol::SceneNode;
        let render = |green_z: Option<i32>| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width: 4,
                height: 4,
                ..BoardProfile::default()
            });
            let rect = |name: &str, color: &str, z: Option<i32>| SceneNode {
                name: name.into(),
                kind: "rect".into(),
                rect: Some([0, 0, 4, 4]),
                value: Some(color.into()),
                value2: None,
                opacity: Some(255),
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z,
                wrap: None,
                mode: None,
                focus: None,
                children: Vec::new(),
            };
            device.scene = Some(SceneNode {
                name: "root".into(),
                kind: "group".into(),
                rect: None,
                value: None,
                value2: None,
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                children: vec![
                    rect("green", "#00FF00", green_z),
                    rect("red", "#FF0000", None),
                ],
            });
            assert!(device.render_scene());
            device.pixel_rgb565(1, 1).unwrap()
        };
        // Document order: red (last) wins.
        let red = render(None);
        assert!(
            red >> 11 > 8 && ((red >> 5) & 0x3f) < 8,
            "red on top: {red:#06x}"
        );
        // z=1 lifts green above red, and hit-testing agrees.
        let green = render(Some(1));
        assert!(
            ((green >> 5) & 0x3f) > 8 && green >> 11 < 8,
            "green on top: {green:#06x}"
        );
    }

    #[test]
    fn radial_focus_moves_the_bright_centre() {
        // N535: `focus` places the radial centre at a fraction of the rect.
        use mdc_protocol::SceneNode;
        let render = |focus: Option<&str>| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width: 9,
                height: 9,
                ..BoardProfile::default()
            });
            device.scene = Some(SceneNode {
                name: "spot".into(),
                kind: "gradient".into(),
                rect: Some([0, 0, 9, 9]),
                value: Some("#FFFFFF".into()),
                value2: Some("#000000".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: Some("radial".into()),
                focus: focus.map(str::to_string),
                children: Vec::new(),
            });
            assert!(device.render_scene());
            let at = |x: u16, y: u16| device.pixel_rgb565(x, y).unwrap();
            (at(0, 0), at(4, 4), at(8, 8))
        };
        // Default: the middle is brightest.
        let (top_left, centre, bottom_right) = render(None);
        assert!(centre > top_left && centre > bottom_right, "centred");
        // focus 0,0: the top-left corner is the bright one.
        let (top_left, centre, _) = render(Some("0,0"));
        assert!(
            top_left > centre,
            "focused top-left: {top_left:#06x} vs {centre:#06x}"
        );
        // focus 100,100: the bottom-right corner is the bright one.
        let (_, centre, bottom_right) = render(Some("100,100"));
        assert!(bottom_right > centre, "focused bottom-right");
        // An unparsable focus falls back to centred.
        let (top_left, centre, _) = render(Some("nope"));
        assert!(centre > top_left, "fallback is centred");
    }

    #[test]
    fn focus_animation_moves_the_radial_centre_over_time() {
        // N546: a focus keyframe track animates the radial centre on the clock.
        use mdc_protocol::{SceneAnim, SceneNode};
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 9,
            height: 9,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "spot".into(),
            kind: "gradient".into(),
            rect: Some([0, 0, 9, 9]),
            value: Some("#FFFFFF".into()),
            value2: Some("#000000".into()),
            opacity: None,
            visible: None,
            anim: Some(SceneAnim {
                duration_ms: 100,
                looping: false,
                opacity: Vec::new(),
                rect: Vec::new(),
                // Bright focus starts top-left, ends bottom-right.
                focus: vec![[0, 0, 0], [100, 100, 100]],
            }),
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: Some("radial".into()),
            focus: Some("0,0".into()),
            children: Vec::new(),
        });
        let top_left_bright = |device: &FakeDevice| {
            let at = |x: u16, y: u16| device.pixel_rgb565(x, y).unwrap();
            at(0, 0) > at(8, 8)
        };
        device.clock.tick = 0;
        assert!(device.render_scene());
        assert!(top_left_bright(&device), "t=0 focuses the top-left");
        device.clock.tick = 100;
        assert!(device.render_scene());
        assert!(!top_left_bright(&device), "t=100 focuses the bottom-right");
    }

    #[test]
    fn gradient_angle_controls_the_direction() {
        // N525: `rotate` sets the ramp direction; 180/270 reverse it and other
        // angles give a diagonal.
        use mdc_protocol::SceneNode;
        let render = |rotate: Option<u16>| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width: 8,
                height: 8,
                ..BoardProfile::default()
            });
            device.scene = Some(SceneNode {
                name: "ramp".into(),
                kind: "gradient".into(),
                rect: Some([0, 0, 8, 8]),
                value: Some("#FFFFFF".into()),
                value2: Some("#000000".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                children: Vec::new(),
            });
            assert!(device.render_scene());
            let mut pixels = Vec::new();
            for y in 0..8u16 {
                for x in 0..8u16 {
                    pixels.push(device.pixel_rgb565(x, y).unwrap());
                }
            }
            pixels
        };
        let at = |pixels: &[u16], x: usize, y: usize| pixels[y * 8 + x];
        // 0: top bright, bottom dark.
        let up = render(None);
        assert!(at(&up, 0, 0) > at(&up, 0, 7));
        assert_eq!(at(&up, 0, 0), 0xFFFF);
        // 180: reversed.
        let down = render(Some(180));
        assert!(at(&down, 0, 7) > at(&down, 0, 0));
        assert_eq!(at(&down, 0, 7), 0xFFFF);
        // 90: left bright, right dark; 270 reverses it.
        let right = render(Some(90));
        assert!(at(&right, 0, 0) > at(&right, 7, 0));
        let left = render(Some(270));
        assert!(at(&left, 7, 0) > at(&left, 0, 0));
        // 45: the diagonal runs from the top-left to the bottom-right.
        let diagonal = render(Some(45));
        assert!(at(&diagonal, 0, 0) > at(&diagonal, 7, 7));
        assert!(at(&diagonal, 0, 0) > at(&diagonal, 7, 0), "corner vs side");
        // 135: the other diagonal (bright at the bottom-left).
        let other = render(Some(135));
        assert!(at(&other, 0, 7) > at(&other, 7, 0));
    }

    #[test]
    fn radial_gradients_are_brighter_at_the_centre() {
        // N515: a radial gradient is brightest (the start colour) at the centre;
        // linear mode still ramps top-to-bottom.
        use mdc_protocol::SceneNode;
        let render = |width: u16, height: u16, mode: Option<&str>| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width,
                height,
                ..BoardProfile::default()
            });
            device.scene = Some(SceneNode {
                name: "glow".into(),
                kind: "gradient".into(),
                rect: Some([0, 0, width, height]),
                value: Some("#FFFFFF".into()),
                value2: Some("#000000".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: mode.map(str::to_string),
                focus: None,
                children: Vec::new(),
            });
            assert!(device.render_scene());
            let mut pixels = Vec::new();
            for y in 0..height {
                for x in 0..width {
                    pixels.push(device.pixel_rgb565(x, y).unwrap());
                }
            }
            pixels
        };
        // Radial: the centre is the start colour, the corner much darker.
        let radial = render(9, 9, Some("radial"));
        let centre = radial[4 * 9 + 4];
        let corner = radial[0];
        assert!(centre > 0xC000, "bright centre: {centre:#06x}");
        assert!(centre > corner, "centre brighter than corner");
        // Linear: the top is bright and the bottom dark.
        let linear = render(1, 9, None);
        assert!(linear[0] > linear[8], "linear ramps down");
        assert_eq!(linear[0], 0xFFFF);
    }

    #[test]
    fn multi_stop_gradients_blend_through_each_stop() {
        // N505: `value2` as `offset:#colour` stops.
        use mdc_protocol::SceneNode;
        let render = |value2: &str| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width: 1,
                height: 11,
                ..BoardProfile::default()
            });
            device.scene = Some(SceneNode {
                name: "ramp".into(),
                kind: "gradient".into(),
                rect: Some([0, 0, 1, 11]),
                value: Some("#FF0000".into()),
                value2: Some(value2.into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                children: Vec::new(),
            });
            assert!(device.render_scene());
            (0..11u16)
                .map(|y| device.pixel_rgb565(0, y).unwrap())
                .collect::<Vec<_>>()
        };
        let stops = render("0:#FF0000,50:#00FF00,100:#0000FF");
        let channel = |value: u16, shift: u32, mask: u16| (value >> shift) & mask;
        // Top is red, the middle is green, and the bottom is blue.
        assert!(
            channel(stops[0], 11, 0x1f) > 24,
            "red top: {:#06x}",
            stops[0]
        );
        assert!(channel(stops[0], 5, 0x3f) < 8, "no green at the top");
        assert!(
            channel(stops[5], 5, 0x3f) > 40,
            "green middle: {:#06x}",
            stops[5]
        );
        assert!(channel(stops[5], 11, 0x1f) < 12 && channel(stops[5], 0, 0x1f) < 12);
        assert!(
            channel(stops[10], 0, 0x1f) > 24,
            "blue bottom: {:#06x}",
            stops[10]
        );
        // A two-stop `value`/`value2` ramp is unchanged (red -> blue).
        let ramp = render("#0000FF");
        assert_eq!(ramp[0], mdc_protocol::parse_color("#FF0000"));
        assert_eq!(ramp[10], mdc_protocol::parse_color("#0000FF"));
    }

    #[test]
    fn dashed_lines_leave_gaps() {
        // N497: a dash length paints the first pixels of each period.
        use mdc_protocol::SceneNode;
        let lit = |dash: Option<&str>| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width: 16,
                height: 1,
                ..BoardProfile::default()
            });
            device.scene = Some(SceneNode {
                name: "rule".into(),
                kind: "line".into(),
                rect: Some([0, 0, 15, 0]),
                value: Some("#FFFFFF".into()),
                value2: dash.map(str::to_string),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                children: Vec::new(),
            });
            assert!(device.render_scene());
            (0..16u16)
                .map(|x| device.pixel_rgb565(x, 0).unwrap() != 0)
                .collect::<Vec<_>>()
        };
        let solid = lit(None);
        assert!(solid.iter().all(|v| *v), "a solid line paints every pixel");
        let dashed = lit(Some("2"));
        assert_eq!(
            dashed,
            vec![
                true, true, false, false, true, true, false, false, true, true, false, false, true,
                true, false, false
            ],
            "dash length 2 leaves 2-pixel gaps"
        );
    }

    #[test]
    fn text_wrap_mode_selects_line_breaking() {
        // N495: `word`/`char` wrap to the rect width; `none` keeps one line.
        use mdc_protocol::SceneNode;
        let lit_rows = |wrap: Option<&str>| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width: 18,
                height: 16,
                ..BoardProfile::default()
            });
            device.scene = Some(SceneNode {
                name: "label".into(),
                kind: "text".into(),
                rect: Some([0, 0, 18, 7]),
                value: Some("ab cd".into()),
                value2: None,
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: wrap.map(str::to_string),
                mode: None,
                focus: None,
                children: Vec::new(),
            });
            assert!(device.render_scene());
            let mut rows = Vec::new();
            for y in 0..16u16 {
                let lit = (0..18u16).any(|x| device.pixel_rgb565(x, y).unwrap() != 0);
                rows.push(lit);
            }
            rows
        };
        // The default (word) and char modes break the phrase onto two lines.
        for mode in [None, Some("word"), Some("char")] {
            let rows = lit_rows(mode);
            assert!(rows[0], "first line drawn ({mode:?})");
            assert!(
                rows.iter().skip(8).any(|v| *v),
                "wrapped onto a second line ({mode:?})"
            );
        }
        // `none` draws a single line, so nothing lands on the second line.
        let none = lit_rows(Some("none"));
        assert!(none[0]);
        assert!(
            !none.iter().skip(8).any(|v| *v),
            "no wrap keeps a single line"
        );
    }

    #[test]
    fn text_align_shifts_lines_within_the_rect() {
        // N484: centre alignment moves the glyphs right of the rect origin.
        use mdc_protocol::SceneNode;
        let render = |align: Option<&str>| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width: 24,
                height: 8,
                ..BoardProfile::default()
            });
            device.scene = Some(SceneNode {
                name: "label".into(),
                kind: "text".into(),
                rect: Some([0, 0, 24, 7]),
                value: Some("hi".into()),
                value2: None,
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: align.map(str::to_string),
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                children: Vec::new(),
            });
            assert!(device.render_scene());
            let mut row = Vec::new();
            for x in 0..24u16 {
                row.push(device.pixel_rgb565(x, 0).unwrap());
            }
            row
        };
        let left = render(None);
        let center = render(Some("center"));
        assert_ne!(left, center, "centred text differs");
        let first_lit = |row: &[u16]| row.iter().position(|v| *v != 0).unwrap_or(usize::MAX);
        assert!(
            first_lit(&center) > first_lit(&left),
            "centre alignment starts further right"
        );
    }

    #[test]
    fn additive_blend_brightens_an_overlap() {
        // N480: an additive overlay adds to the base instead of covering it.
        use mdc_protocol::SceneNode;
        let render = |blend: Option<&str>| {
            let (_host, mut device) = FakeDevice::pair(BoardProfile {
                width: 4,
                height: 4,
                ..BoardProfile::default()
            });
            let rect = |name: &str, color: &str, blend: Option<&str>| SceneNode {
                name: name.into(),
                kind: "rect".into(),
                rect: Some([0, 0, 4, 4]),
                value: Some(color.into()),
                value2: None,
                opacity: Some(128),
                visible: None,
                anim: None,
                rotate: None,
                blend: blend.map(str::to_string),
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                children: Vec::new(),
            };
            device.scene = Some(SceneNode {
                name: "root".into(),
                kind: "group".into(),
                rect: None,
                value: None,
                value2: None,
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                children: vec![
                    rect("base", "#00FF00", None),
                    rect("glow", "#FF0000", blend),
                ],
            });
            assert!(device.render_scene());
            device.pixel_rgb565(1, 1).unwrap()
        };
        let over = render(None);
        let additive = render(Some("additive"));
        // Source-over fades green toward red; additive keeps the green and adds red.
        assert!(
            ((additive >> 5) & 0x3f) > ((over >> 5) & 0x3f),
            "green is preserved: over={over:#06x} additive={additive:#06x}"
        );
        // Additive keeps the base green and adds red, so it is strictly brighter.
        let brightness = |value: u16| {
            u32::from((value >> 11) & 0x1f)
                + u32::from((value >> 5) & 0x3f)
                + u32::from(value & 0x1f)
        };
        assert!(
            brightness(additive) > brightness(over),
            "additive is brighter: over={over:#06x} additive={additive:#06x}"
        );
    }

    #[test]
    fn lowercase_glyphs_are_distinct() {
        // N476: `a` and `A` are different glyphs.
        let lower = glyph_columns(b'a').unwrap();
        let upper = glyph_columns(b'A').unwrap();
        assert_ne!(lower, upper);
        // Every letter renders (no gaps).
        for ch in b'a'..=b'z' {
            assert!(glyph_columns(ch).is_some(), "missing {ch}");
        }
        // Rendering "abc" differs from the uppercase form.
        let (mut a, mut b) = (vec![0u8; 32 * 2], vec![0u8; 32 * 2]);
        draw_text(&mut a, 16, 8, 0, 0, "abc", 0xFFFF, 1, 1, 255);
        draw_text(&mut b, 16, 8, 0, 0, "ABC", 0xFFFF, 1, 1, 255);
        assert_ne!(a, b);
        assert!(a.iter().any(|byte| *byte != 0), "lowercase text draws");
    }

    #[test]
    fn scene_rotate_swaps_the_rect_axes() {
        // N475: rotating a wide rect by 90 degrees draws it tall.
        use mdc_protocol::SceneNode;
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 8,
            height: 8,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "wide".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 8, 2]),
            value: Some("#FFFFFF".into()),
            value2: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: Some(90),
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        });
        assert!(device.render_scene());
        assert_eq!(device.pixel_rgb565(0, 0), Some(0xFFFF));
        // Rotated: 2 wide x 8 tall, so (1,7) is filled and (7,1) is not.
        assert_eq!(device.pixel_rgb565(1, 7), Some(0xFFFF), "tall after rotate");
        assert_eq!(device.pixel_rgb565(7, 1), Some(0), "not wide after rotate");
        assert_eq!(device.hit_test(1, 7).as_deref(), Some("wide"));
    }

    #[test]
    fn overlay_surface_renders_independently() {
        // N465: the overlay has its own buffer and rasterises the scene there.
        use mdc_protocol::SceneNode;
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            overlay_width: 2,
            overlay_height: 2,
            ..BoardProfile::default()
        });
        // No scene: a white placeholder fills the overlay, main stays untouched.
        assert!(device.render_overlay());
        assert_eq!(device.overlay_pixel_rgb565(0, 0), Some(0xFFFF));
        assert_eq!(device.pixel_rgb565(0, 0), Some(0), "main untouched");

        // A scene renders into the overlay at its own size.
        device.scene = Some(SceneNode {
            name: "o".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 2, 2]),
            value: Some("#00FF00".into()),
            value2: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        });
        assert!(device.render_overlay());
        assert_eq!(device.overlay_pixel_rgb565(1, 1), Some(0x07E0));
    }

    #[test]
    fn scene_animation_samples_on_the_device_clock() {
        // N463: opacity keyframes drive the rendered alpha over time.
        use mdc_protocol::{SceneAnim, SceneNode};
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "fade".into(),
            kind: "rect".into(),
            rect: Some([0, 0, 4, 4]),
            value: Some("#FFFFFF".into()),
            value2: None,
            opacity: Some(255),
            visible: None,
            anim: Some(SceneAnim {
                duration_ms: 100,
                looping: false,
                opacity: vec![[0, 0], [100, 255]],
                rect: Vec::new(),
                focus: Vec::new(),
            }),
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        });
        // t=0: fully transparent, so the black background remains.
        assert!(device.render_scene());
        assert_eq!(device.pixel_rgb565(0, 0), Some(0), "transparent at t=0");
        // t=duration: fully opaque white.
        device.clock.advance(100);
        assert!(device.render_scene());
        assert_eq!(device.pixel_rgb565(0, 0), Some(0xFFFF), "opaque at the end");
        // Midway: a blend between the two.
        device.clock.tick = 50;
        assert!(device.render_scene());
        let mid = device.pixel_rgb565(0, 0).unwrap();
        assert!(mid != 0 && mid != 0xFFFF, "blended midway: {mid:#06x}");
    }

    #[test]
    fn hidden_scene_nodes_are_skipped() {
        use mdc_protocol::SceneNode;
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 8,
            height: 8,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "root".into(),
            kind: "group".into(),
            rect: None,
            value: None,
            value2: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: vec![
                SceneNode {
                    name: "hidden".into(),
                    kind: "rect".into(),
                    rect: Some([0, 0, 8, 8]),
                    value: Some("#FFFFFF".into()),
                    value2: None,
                    opacity: None,
                    visible: Some(false),
                    anim: None,
                    rotate: None,
                    blend: None,
                    align: None,
                    z: None,
                    wrap: None,
                    mode: None,
                    focus: None,
                    children: Vec::new(),
                },
                SceneNode {
                    name: "shown".into(),
                    kind: "rect".into(),
                    rect: Some([0, 0, 4, 4]),
                    value: Some("#00FF00".into()),
                    value2: None,
                    opacity: None,
                    visible: None,
                    anim: None,
                    rotate: None,
                    blend: None,
                    align: None,
                    z: None,
                    wrap: None,
                    mode: None,
                    focus: None,
                    children: Vec::new(),
                },
            ],
        });
        assert!(device.render_scene());
        // The hidden full-surface rect is skipped; the shown 4x4 rect is drawn.
        assert_eq!(device.pixel_rgb565(0, 0), Some(0x07E0), "shown rect drawn");
        assert_eq!(device.pixel_rgb565(7, 7), Some(0), "hidden rect skipped");
        assert_eq!(device.hit_test(7, 7), None, "hidden node not hit");
        assert_eq!(device.hit_test(1, 1).as_deref(), Some("shown"));
    }

    #[test]
    fn mirror_refuses_a_non_primary_surface() {
        // N457: only `main` is mirrored; `overlay` is refused explicitly.
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            overlay_width: 4,
            overlay_height: 2,
            mirror: true,
            ..BoardProfile::default()
        });
        let command = |surface: &str| mdc_protocol::MirrorCommand {
            action: "start".into(),
            surface_id: surface.into(),
            mode: "full-frame".into(),
            max_fps: 15,
            format: "rgb565".into(),
        };
        let packet = |surface: &str| Packet {
            version: mdc_protocol::VERSION,
            kind: MessageType::MirrorCtrl,
            flags: 0,
            request_id: 1,
            payload: mdc_protocol::encode_control(&command(surface)).unwrap(),
        };
        assert!(device.handle_mirror_ctrl(packet("overlay")).is_ok());
        assert!(!device.mirror_active, "overlay is refused");
        assert!(device.handle_mirror_ctrl(packet("main")).is_ok());
        assert!(device.mirror_active, "main is mirrored");
    }

    #[test]
    fn hit_test_finds_the_topmost_node() {
        use mdc_protocol::SceneNode;
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 16,
            height: 16,
            ..BoardProfile::default()
        });
        let rect = |name: &str, r: [u16; 4]| SceneNode {
            name: name.into(),
            kind: "rect".into(),
            rect: Some(r),
            value: Some("#FFFFFF".into()),
            value2: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        };
        device.scene = Some(SceneNode {
            name: "root".into(),
            kind: "group".into(),
            rect: None,
            value: None,
            value2: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: vec![rect("back", [0, 0, 8, 8]), rect("front", [2, 2, 4, 4])],
        });
        // The overlapping topmost child wins.
        assert_eq!(device.hit_test(3, 3).as_deref(), Some("front"));
        // The lower child is hit outside the front rect.
        assert_eq!(device.hit_test(7, 7).as_deref(), Some("back"));
        // Outside every rect.
        assert_eq!(device.hit_test(12, 12), None);
    }

    #[test]
    fn scene_clip_bounds_children() {
        use mdc_protocol::SceneNode;
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 8,
            height: 8,
            ..BoardProfile::default()
        });
        let fill = |rect: [u16; 4]| SceneNode {
            name: "f".into(),
            kind: "rect".into(),
            rect: Some(rect),
            value: Some("#FFFFFF".into()),
            value2: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        };
        device.scene = Some(SceneNode {
            name: "c".into(),
            kind: "clip".into(),
            rect: Some([2, 2, 4, 4]),
            value: None,
            value2: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            // The child fills 8x8 relative to the clip origin, so it would cover
            // everything without the clip rect.
            children: vec![fill([0, 0, 8, 8])],
        });
        assert!(device.render_scene());
        assert_eq!(device.pixel_rgb565(3, 3), Some(0xFFFF), "inside clip");
        assert_eq!(device.pixel_rgb565(0, 0), Some(0), "outside clip top-left");
        assert_eq!(
            device.pixel_rgb565(7, 7),
            Some(0),
            "outside clip bottom-right"
        );
        assert_eq!(
            device.pixel_rgb565(5, 5),
            Some(0xFFFF),
            "clip edge included"
        );
        assert_eq!(device.pixel_rgb565(6, 6), Some(0), "just past clip");
    }

    #[test]
    fn scene_rounded_rect_clips_the_corners() {
        use mdc_protocol::SceneNode;
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 8,
            height: 8,
            ..BoardProfile::default()
        });
        device.scene = Some(SceneNode {
            name: "r".into(),
            kind: "rounded-rect".into(),
            rect: Some([0, 0, 8, 8]),
            value: Some("#FFFFFF".into()),
            value2: Some("4".into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            children: Vec::new(),
        });
        assert!(device.render_scene());
        assert_eq!(device.pixel_rgb565(0, 0), Some(0), "corner clipped");
        assert_ne!(device.pixel_rgb565(0, 4), Some(0), "edge middle filled");
        assert_ne!(device.pixel_rgb565(4, 4), Some(0), "center filled");
    }

    #[test]
    fn mesh_clip_cross_fade_blends_and_expires() {
        let (_host, mut device) = FakeDevice::pair(BoardProfile {
            width: 4,
            height: 4,
            ..BoardProfile::default()
        });
        device.begin_mesh_crossfade();
        assert_eq!(device.mesh_crossfade_ms, MESH_CROSSFADE_MS);
        assert_eq!(device.mesh_prev_pixels.len(), device.pixels.len());

        // Full weight copies; zero keeps.
        let src = [0xE0u8, 0x07, 0x00, 0xF8];
        let mut dst = vec![0x00u8, 0xF8, 0xE0, 0x07];
        mix_rgb565(&mut dst, &src, 4);
        assert_eq!(dst, src);
        let mut dst = vec![0x00u8, 0xF8, 0xE0, 0x07];
        mix_rgb565(&mut dst, &src, 0);
        assert_eq!(dst, vec![0x00, 0xF8, 0xE0, 0x07]);

        // Half weight blends red toward green (r channel strictly between).
        let mut dst = vec![0x00u8, 0xF8, 0x00, 0xF8];
        mix_rgb565(&mut dst, &[0xE0u8, 0x07, 0xE0, 0x07], 2);
        let red = (u16::from_le_bytes([dst[0], dst[1]]) >> 11) & 0x1f;
        assert!(red > 0 && red < 0x1f, "red channel blended: {red}");
    }

    #[test]
    fn scene_draws_circles_and_lines() {
        use mdc_protocol::SceneNode;
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 16,
            height: 16,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        let node = |name: &str, kind: &str, rect: [u16; 4], value: &str| SceneNode {
            name: name.into(),
            kind: kind.into(),
            rect: Some(rect),
            children: Vec::new(),
            value: Some(value.into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        let scene = SceneNode {
            name: "root".into(),
            kind: "group".into(),
            rect: None,
            value: None,
            children: vec![
                node("disc", "circle", [0, 0, 8, 8], "#FF0000"),
                node("diag", "line", [0, 15, 15, 0], "#00FF00"),
            ],
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        session.set_scene(scene).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        assert_eq!(device.pixel_rgb565(4, 4), Some(0xF800), "circle centre");
        assert_eq!(
            device.pixel_rgb565(0, 0),
            Some(0),
            "ellipse corner is clear"
        );
        assert_eq!(device.pixel_rgb565(8, 7), Some(0x07E0), "line pixel");
    }

    #[test]
    fn scene_group_offsets_and_scales_children() {
        use mdc_protocol::SceneNode;
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 16,
            height: 16,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        let rect = |name: &str, geometry: [u16; 4], color: &str| SceneNode {
            name: name.into(),
            kind: "rect".into(),
            rect: Some(geometry),
            children: Vec::new(),
            value: Some(color.into()),
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        // Group offset (4, 2) and 2× scale: [0,0,2,2] red -> [4,2,4,4].
        let scene = SceneNode {
            name: "root".into(),
            kind: "group".into(),
            rect: Some([4, 2, 2000, 2000]),
            children: vec![rect("bg", [0, 0, 2, 2], "#FF0000")],
            value: None,
            opacity: None,
            visible: None,
            anim: None,
            rotate: None,
            blend: None,
            align: None,
            z: None,
            wrap: None,
            mode: None,
            focus: None,
            value2: None,
        };
        session.set_scene(scene).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        assert_eq!(device.pixel_rgb565(4, 2), Some(0xF800), "offset applied");
        assert_eq!(device.pixel_rgb565(7, 5), Some(0xF800), "2x scale applied");
        assert_eq!(device.pixel_rgb565(0, 0), Some(0), "outside the group");
    }

    #[test]
    fn setting_a_scene_stops_the_running_animation() {
        use mdc_protocol::SceneNode;
        let (host, mut device) = FakeDevice::pair(BoardProfile::default());
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        // Install an atlas and play a clip.
        let atlas = mdc_model::AtlasBlob {
            sprite_width: 2,
            sprite_height: 2,
            frames: 2,
            columns: 2,
            rows: 1,
            rgb565: vec![0u8; 16],
            clips: vec![mdc_model::AtlasClip {
                name: "idle".into(),
                start: 0,
                frames: 2,
                fps: 30,
                looping: true,
                fade_in_ms: 0,
                fade_out_ms: 0,
            }],

            mips: Vec::new(),
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
            estimated_bytes: 16,
            atlas: Some(atlas),
            texture: None,
            texture_blobs: Vec::new(),
        };
        let sealed = mdc_model::MdcPackage::seal(package).unwrap();
        session
            .install_package(&sealed.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session.set_config("animation", "idle").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.current_atlas_frame().is_some());

        session
            .set_scene(SceneNode {
                name: "bg".into(),
                kind: "rect".into(),
                rect: Some([0, 0, 2, 2]),
                children: Vec::new(),
                value: Some("#FFFFFF".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                value2: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.current_atlas_frame(), None, "scene took over");
        assert!(device.render_scene());
    }

    #[test]
    fn mesh_preview_rasterizes_after_install() {
        let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        let package = mdc_model::DevicePackage {
            target: "linux-mesh".into(),
            mode: mdc_model::PackageMode::Mesh,
            nodes: Vec::new(),
            meshes: vec![mdc_model::Mesh {
                name: "tri".into(),
                vertices: 3,
                indices: 0,
                material: None,
                preview_xy: vec![(1, 1), (6, 1), (1, 6)],
                preview_indices: Vec::new(),
                ..mdc_model::Mesh::default()
            }],
            textures: Vec::new(),
            materials: Vec::new(),
            lights: Vec::new(),
            skeletons: Vec::new(),
            animations: Vec::new(),
            sprites: Vec::new(),
            layers: Vec::new(),
            estimated_bytes: 36,
            atlas: None,
            texture: None,
            texture_blobs: Vec::new(),
        };
        let sealed = mdc_model::MdcPackage::seal(package).unwrap();
        session
            .install_package(&sealed.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_preview());
        assert_ne!(device.pixel_rgb565(1, 1), Some(0));
    }

    #[test]
    fn indexed_preview_does_not_draw_the_unindexed_corner() {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 8,
            height: 8,
            renderer_mesh2d: true,
            renderer_max_vertices: 16,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        let package = mdc_model::DevicePackage {
            target: "linux-mesh".into(),
            mode: mdc_model::PackageMode::Mesh,
            nodes: Vec::new(),
            meshes: vec![mdc_model::Mesh {
                name: "tri".into(),
                vertices: 4,
                indices: 3,
                material: None,
                preview_xy: vec![(0, 0), (4, 0), (0, 4), (7, 7)],
                preview_indices: vec![0, 1, 2],
                ..mdc_model::Mesh::default()
            }],
            textures: Vec::new(),
            materials: Vec::new(),
            lights: Vec::new(),
            skeletons: Vec::new(),
            animations: Vec::new(),
            sprites: Vec::new(),
            layers: Vec::new(),
            estimated_bytes: 48,
            atlas: None,
            texture: None,
            texture_blobs: Vec::new(),
        };
        let sealed = mdc_model::MdcPackage::seal(package).unwrap();
        session
            .install_package(&sealed.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_preview());
        assert_ne!(device.pixel_rgb565(1, 1), Some(0));
        assert_eq!(device.pixel_rgb565(7, 7), Some(0));
    }

    #[test]
    fn scene_sprite_blits_an_atlas_frame() {
        use mdc_protocol::SceneNode;
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
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
            estimated_bytes: 8,
            atlas: Some(mdc_model::AtlasBlob {
                sprite_width: 1,
                sprite_height: 1,
                frames: 1,
                columns: 1,
                rows: 1,
                rgb565: vec![0x00, 0xF8],
                clips: Vec::new(),
                mips: Vec::new(),
            }),
            texture: None,
            texture_blobs: Vec::new(),
        };
        let sealed = mdc_model::MdcPackage::seal(package).unwrap();
        session
            .install_package(&sealed.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .set_scene(SceneNode {
                name: "icon".into(),
                kind: "sprite".into(),
                rect: Some([0, 0, 1, 1]),
                children: Vec::new(),
                value: Some("0".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                value2: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_scene());
        assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800));
    }

    #[test]
    fn atlas_fade_in_darkens_the_first_frame() {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 1,
            height: 1,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
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
            estimated_bytes: 2,
            atlas: Some(mdc_model::AtlasBlob {
                sprite_width: 1,
                sprite_height: 1,
                frames: 1,
                columns: 1,
                rows: 1,
                rgb565: vec![0x00, 0xF8],
                clips: vec![mdc_model::AtlasClip {
                    name: "idle".into(),
                    start: 0,
                    frames: 1,
                    fps: 30,
                    looping: true,
                    fade_in_ms: 100,
                    fade_out_ms: 0,
                }],

                mips: Vec::new(),
            }),
            texture: None,
            texture_blobs: Vec::new(),
        };
        let sealed = mdc_model::MdcPackage::seal(package).unwrap();
        session
            .install_package(&sealed.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session.set_config("animation", "idle").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_atlas_frame());
        assert_eq!(device.pixel_rgb565(0, 0), Some(0));
        device.clock.advance(100);
        assert!(device.render_atlas_frame());
        assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800));
    }

    #[test]
    fn atlas_fade_out_darkens_the_last_moment() {
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 1,
            height: 1,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
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
            estimated_bytes: 2,
            atlas: Some(mdc_model::AtlasBlob {
                sprite_width: 1,
                sprite_height: 1,
                frames: 1,
                columns: 1,
                rows: 1,
                rgb565: vec![0x00, 0xF8],
                clips: vec![mdc_model::AtlasClip {
                    name: "once".into(),
                    start: 0,
                    frames: 1,
                    fps: 1,
                    looping: false,
                    fade_in_ms: 0,
                    fade_out_ms: 400,
                }],

                mips: Vec::new(),
            }),
            texture: None,
            texture_blobs: Vec::new(),
        };
        let sealed = mdc_model::MdcPackage::seal(package).unwrap();
        session
            .install_package(&sealed.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session.set_config("animation", "once").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_atlas_frame());
        assert_eq!(device.pixel_rgb565(0, 0), Some(0xF800));
        device.clock.advance(800);
        assert!(device.render_atlas_frame());
        assert_ne!(device.pixel_rgb565(0, 0), Some(0xF800));
    }

    #[test]
    fn device_renders_the_scene_into_the_mirror() {
        use mdc_protocol::SceneNode;
        let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        session
            .set_scene(SceneNode {
                name: "bg".into(),
                kind: "rect".into(),
                rect: Some([0, 0, 4, 4]),
                children: Vec::new(),
                value: Some("#0000FF".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                value2: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();

        session.start_mirror("main", "full-frame", 15).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device.emit_mirror_frame().unwrap();
        session.poll().unwrap();
        let frame = session.take_mirror_frame().unwrap();
        assert_eq!(&frame.bytes[0..2], &[0x1F, 0x00], "blue scene pixel");
    }

    #[test]
    fn renderer_capability_follows_the_board_profile() {
        let (_, esp) = FakeDevice::with_default_profile();
        let renderer = esp.capabilities().renderer.unwrap();
        assert!(renderer.sprite_2d && !renderer.mesh2d && !renderer.skeleton);
        assert_eq!(renderer.max_texture, 512);

        let (_, gpu) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
        let renderer = gpu.capabilities().renderer.unwrap();
        assert!(renderer.mesh2d && renderer.mesh3d && renderer.skeleton);
        assert_eq!(renderer.max_texture, 2048);
        assert_eq!(renderer.max_vertices, 200_000);
    }

    #[test]
    fn accepts_a_runtime_package_and_reports_ready() {
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        let gltf = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root","mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"count":4,"type":"VEC3"}]}"#;
        let model = mdc_model::GltfImporter
            .import(gltf, &mdc_model::ImportLimits::default())
            .unwrap();
        let package =
            mdc_model::compile_to_package(&model, &mdc_model::RendererCapability::sprite_only())
                .unwrap();
        session
            .install_package(&package.to_bytes().unwrap())
            .unwrap();
        device.poll().unwrap();
        assert!(device.package_ready());
        assert_eq!(device.metrics.packages_received, 1);
    }

    #[test]
    fn hello_capabilities_and_legacy_frame() {
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        assert_eq!(
            session.device.as_ref().unwrap().capabilities.device_id,
            "sim-esp32-jc3248w535"
        );
        let pixels = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: pixels.clone(),
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device.assert_pixels_eq(&pixels);
        assert_eq!(session.pending_request_count(), 0);
    }

    #[test]
    fn accepts_chunked_frame_and_tile_with_base_check() {
        let profile = BoardProfile {
            width: 4,
            height: 2,
            device_id: "chunk-sim".into(),
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        // Force small chunk on device side via Hello negotiation: Session sends DEFAULT_MAX_CHUNK;
        // override after handshake by setting device.max_chunk before frame — Session uses caps.
        let mut session = Session::new(host);
        session.connect().unwrap();
        // Shrink device max_chunk before answering so capabilities advertise 8.
        device.max_chunk = 8;
        device.poll().unwrap();
        session.poll().unwrap();

        let pixels: Vec<u8> = (0..16).collect();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 4,
                height: 2,
                bytes: pixels.clone(),
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device.assert_pixels_eq(&pixels);
        assert_eq!(device.current_frame_id(), 1);

        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 1,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0xaa, 0xbb],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(&device.pixels()[2..4], &[0xaa, 0xbb]);

        // Bad base is rejected (Error); session surfaces protocol error on poll.
        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 99,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0, 0],
            })
            .unwrap();
        device.poll().unwrap();
        assert!(session.poll().is_err());
    }

    #[test]
    fn fault_injector_drop_delay_disconnect() {
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.faults.drop_next();
        device.poll().unwrap(); // HELLO dropped
        assert_ne!(session.poll().unwrap(), ConnectionState::Ready);

        // Re-send path: new session pair for delay + disconnect
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.faults.delay(5);
        device.poll().unwrap(); // HELLO delayed
        assert_ne!(session.poll().unwrap(), ConnectionState::Ready);
        device.advance(5).unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);

        device.faults.request_disconnect();
        assert_eq!(device.poll(), Err(SimError::Disconnected));
    }

    #[test]
    fn linux_virt_profile_matches_board_yaml() {
        let p = BoardProfile::from_board_id("linux-virt");
        assert_eq!(p.device_id, "sim-linux-virt");
        assert_eq!(p.width, 800);
        assert_eq!(p.height, 480);
        assert!(!p.touch);
        assert!(!p.ota);
        assert!(p.color_fps);
        let pi = BoardProfile::from_board_id("pi4-hdmi");
        assert_eq!((pi.width, pi.height), (640, 480));
        assert!(pi.color_fps);
        let eink = BoardProfile::from_board_id("waveshare-eink");
        assert!(!eink.color_fps, "e-ink must not claim color FPS");
    }

    #[test]
    fn mirror_capability_follows_profile_and_gates_control() {
        // ESP32-S3 class: Form A only, no mirror capability.
        let (_, esp) = FakeDevice::pair(BoardProfile::default());
        assert!(!esp.capabilities().mirror.supports());
        assert_eq!(esp.capabilities().mirror_fps, 0);

        // A GPU board advertises mirror; local display and mirror FPS differ.
        let profile = BoardProfile {
            width: 4,
            height: 2,
            mirror: true,
            mirror_fps: 5,
            local_display_fps: 30,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let caps = device.capabilities();
        assert!(caps.mirror.supports());
        assert_eq!(caps.local_display_fps, 30);
        assert_eq!(caps.mirror_fps, 5);
        assert_ne!(caps.local_display_fps, caps.mirror_fps);
        assert!(caps.mirror_bandwidth > 0);

        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(session.supports_mirror());
        session.start_mirror("main", "full-frame", 30).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.mirror_active());

        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 4,
                height: 2,
                bytes: vec![7; 16],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        // Device mirrors its rendered frame back to the host.
        device.emit_mirror_frame().unwrap();
        session.poll().unwrap();
        let mirrored = session.take_mirror_frame().unwrap();
        assert_eq!(mirrored.frame_id, 1);
        assert_eq!((mirrored.width, mirrored.height), (4, 2));
        assert_eq!(mirrored.bytes, vec![7; 16]);

        session.stop_mirror("main").unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(!device.mirror_active());
    }

    #[test]
    fn device_masks_transparent_texels() {
        use mdc_model::{Material, Mesh, MeshGeometry, Node, TextureBlob};
        let mesh = Mesh {
            name: "quad".into(),
            vertices: 4,
            indices: 6,
            material: Some(0),
            geometry: Some(MeshGeometry {
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
            }),
            ..Mesh::default()
        };
        let package = mdc_model::DevicePackage {
            target: "linux-mesh".into(),
            mode: mdc_model::PackageMode::Mesh,
            nodes: vec![Node {
                name: "root".into(),
                mesh: Some(0),
                ..Node::default()
            }],
            meshes: vec![mesh],
            textures: Vec::new(),
            materials: vec![Material {
                name: "cut".into(),
                texture: None,
                color: Some([255, 255, 255]),
                alpha_mode: Some("MASK".into()),
                alpha_cutoff: Some(0.5),
                alpha: None,
                metallic: None,
                roughness: None,
                emissive: None,
                unlit: None,
                uv_transform: None,
                emissive_strength: None,
                occlusion_texture: None,
                occlusion_strength: None,
                normal_texture: None,
                normal_scale: None,
                uv_set: None,
                uv_set2: None,
                emissive_texture: None,
                emissive_uv_transform: None,
                normal_uv_transform: None,
                occlusion_uv_transform: None,
                roughness_texture: None,
                roughness_uv_transform: None,
                specular_factor: None,
                specular_color: None,
                ior: None,
                clearcoat_factor: None,
                clearcoat_roughness: None,
                sheen_color: None,
                sheen_roughness: None,
                transmission: None,
                volume_thickness: None,
                volume_attenuation_distance: None,
                iridescence_factor: None,
                iridescence_ior: None,
                anisotropy_strength: None,
                anisotropy_rotation: None,
                dispersion: None,
            }],
            lights: Vec::new(),
            skeletons: Vec::new(),
            animations: Vec::new(),
            sprites: Vec::new(),
            layers: Vec::new(),
            estimated_bytes: 48,
            atlas: None,
            texture: Some(TextureBlob {
                width: 2,
                height: 1,
                rgb565: vec![0x00, 0xF8, 0xE0, 0x07],
                alpha: vec![0b01],
                palette: Vec::new(),
                indices: Vec::new(),
                mips: Vec::new(),
            }),
            texture_blobs: Vec::new(),
        };
        let (host, mut device) = FakeDevice::pair(BoardProfile {
            width: 16,
            height: 16,
            renderer_mesh2d: true,
            renderer_max_vertices: 4096,
            ..BoardProfile::default()
        });
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        session
            .install_package(
                &mdc_model::MdcPackage::seal(package)
                    .unwrap()
                    .to_bytes()
                    .unwrap(),
            )
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert!(device.render_preview());
        assert_eq!(device.pixel_rgb565(2, 8), Some(0xF800), "opaque left drawn");
        assert_eq!(
            device.pixel_rgb565(13, 8),
            Some(0),
            "transparent right clear"
        );
    }

    #[test]
    fn dirty_region_mirror_sends_only_changed_ranges() {
        use mdc_protocol::SceneNode;
        let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        assert_eq!(session.poll().unwrap(), ConnectionState::Ready);
        assert!(device.capabilities().mirror.supports_mode("dirty-region"));

        session
            .set_scene(SceneNode {
                name: "bg".into(),
                kind: "rect".into(),
                rect: Some([0, 0, 4, 4]),
                children: Vec::new(),
                value: Some("#000000".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                value2: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session.start_mirror("main", "dirty-region", 10).unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.mirror_mode(), "dirty-region");

        // First frame: no previous, sent in full.
        device.emit_mirror_frame().unwrap();
        session.poll().unwrap();
        let first = session.take_mirror_frame().expect("first frame");
        assert_eq!((first.width, first.height), (800, 480));

        // Change one small rect; the device chunks only the changed rows.
        session
            .set_scene(SceneNode {
                name: "bg".into(),
                kind: "rect".into(),
                rect: Some([0, 0, 4, 4]),
                children: Vec::new(),
                value: Some("#FF0000".into()),
                opacity: None,
                visible: None,
                anim: None,
                rotate: None,
                blend: None,
                align: None,
                z: None,
                wrap: None,
                mode: None,
                focus: None,
                value2: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device.emit_mirror_frame().unwrap();
        session.poll().unwrap();
        let second = session.take_mirror_frame().expect("patched frame");
        assert_eq!((second.width, second.height), (800, 480));
        assert_ne!(second.bytes, first.bytes, "the patch changed pixels");
    }

    #[test]
    fn device_without_mirror_rejects_mirror_control() {
        let (host, mut esp) = FakeDevice::pair(BoardProfile::default());
        let mut session = Session::new(host);
        session.connect().unwrap();
        esp.poll().unwrap();
        session.poll().unwrap();
        assert!(!session.supports_mirror());
        assert_eq!(
            session.start_mirror("main", "full-frame", 30),
            Err(mdc_core::CoreError::Unsupported)
        );
        assert_eq!(
            session.stop_mirror("main"),
            Err(mdc_core::CoreError::Unsupported)
        );
        assert!(esp.emit_mirror_frame().is_err());
        assert!(!esp.mirror_active());
        // Nothing but the handshake reply ever reached the device.
        assert_eq!(esp.metrics.mirror_commands, 0);
    }

    #[test]
    fn eink_busy_keeps_the_previous_frame() {
        let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("waveshare-eink"));
        assert_eq!(device.capabilities().max_fps, 0);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let first = vec![0x11; 800 * 480 * 2];
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 800,
                height: 480,
                bytes: first,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels()[0], 0x11);
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 800,
                height: 480,
                bytes: vec![0x22; 800 * 480 * 2],
            })
            .unwrap();
        device.poll().unwrap();
        assert_eq!(device.pixels()[0], 0x11);
        let err = session.poll().unwrap_err();
        assert!(err.to_string().contains("E_BUSY"), "{err}");
        device.advance(100).unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 800,
                height: 480,
                bytes: vec![0x22; 800 * 480 * 2],
            })
            .unwrap();
        device.poll().unwrap();
        assert_eq!(device.pixels()[0], 0x22);
    }

    #[test]
    fn default_profile_is_480x320() {
        let (_, device) = FakeDevice::with_default_profile();
        assert_eq!((device.profile.width, device.profile.height), (480, 320));
        assert_eq!(device.pixels().len(), 480 * 320 * 2);
    }

    #[test]
    fn emit_input_reaches_session_manager() {
        use mdc_core::{DeviceManager, ManagerEvent};
        use std::sync::{Arc, Mutex};
        let profile = BoardProfile {
            width: 2,
            height: 2,
            touch: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 0,
                phase: "down".into(),
                x: 1,
                y: 1,

                node: None,
            })
            .unwrap();
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        assert!(events.iter().any(|e| matches!(
            e,
            ManagerEvent::Input {
                event,
                ..
            } if event.phase == "down"
        )));
    }

    #[test]
    fn scripted_input_follows_displayed_frame() {
        use mdc_core::{DeviceManager, Frame, ManagerEvent, Session};
        use std::sync::{Arc, Mutex};
        let profile = BoardProfile {
            width: 2,
            height: 2,
            touch: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        device.emit_input_after_next_frame(InputEvent {
            surface_id: "main".into(),
            pointer_id: 1,
            phase: "down".into(),
            x: 1,
            y: 1,

            node: None,
        });
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            ManagerEvent::Input { event, .. } if event.x == 1 && event.y == 1 && event.phase == "down"
        )));
    }

    #[test]
    fn ota_gated_by_capability() {
        use mdc_protocol::OtaCommand;
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ota: false,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(
            session.send_ota(OtaCommand {
                action: "begin".into(),
                version: None,
                size: None,
                sha256: None,
            }),
            Err(mdc_core::CoreError::Unsupported)
        );

        let profile = BoardProfile {
            width: 2,
            height: 2,
            ota: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_ota(OtaCommand {
                action: "begin".into(),
                version: Some("1.0.0".into()),
                size: Some(10),
                sha256: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.metrics.ota_commands, 1);
    }

    fn ota(
        session: &mut Session<MemoryLink>,
        device: &mut FakeDevice,
        action: &str,
        version: Option<&str>,
        size: Option<u64>,
        sha: Option<&str>,
        expect_error: bool,
    ) {
        session
            .send_ota(OtaCommand {
                action: action.into(),
                version: version.map(str::to_string),
                size,
                sha256: sha.map(str::to_string),
            })
            .unwrap();
        device.poll().unwrap();
        let polled = session.poll();
        if expect_error {
            assert!(polled.is_err());
        } else {
            polled.unwrap();
        }
    }

    #[test]
    fn ota_failure_keeps_installed_firmware() {
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ota: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.firmware(), "1.0.0");

        ota(
            &mut session,
            &mut device,
            "begin",
            Some("9.9.9"),
            Some(2 * 1024 * 1024),
            None,
            true,
        );
        assert_eq!(device.firmware(), "1.0.0");
        assert!(device.pending_version().is_none());

        ota(
            &mut session,
            &mut device,
            "begin",
            Some("2.0.0"),
            Some(32),
            None,
            false,
        );
        assert_eq!(device.firmware(), "1.0.0");
        ota(
            &mut session,
            &mut device,
            "chunk",
            None,
            None,
            Some("bad"),
            true,
        );
        assert_eq!(device.firmware(), "1.0.0");
        assert!(device.metrics.ota_rollbacks >= 1);

        ota(
            &mut session,
            &mut device,
            "begin",
            Some("2.0.0"),
            Some(32),
            None,
            false,
        );
        ota(&mut session, &mut device, "abort", None, None, None, false);
        assert_eq!(device.firmware(), "1.0.0");

        ota(
            &mut session,
            &mut device,
            "begin",
            Some("2.0.0"),
            Some(32),
            None,
            false,
        );
        ota(&mut session, &mut device, "timeout", None, None, None, true);
        assert_eq!(device.firmware(), "1.0.0");

        ota(
            &mut session,
            &mut device,
            "begin",
            Some("2.0.0"),
            Some(32),
            None,
            false,
        );
        ota(
            &mut session,
            &mut device,
            "confirm",
            None,
            None,
            None,
            false,
        );
        assert_eq!(device.firmware(), "2.0.0");
    }

    #[test]
    fn ota_begin_freezes_pixels_until_abort() {
        use mdc_core::{Frame, Session};
        use mdc_protocol::OtaCommand;
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ota: true,
            touch: false,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let painted = vec![0x11u8; 8];
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: painted.clone(),
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), painted.as_slice());
        session
            .send_ota(OtaCommand {
                action: "begin".into(),
                version: Some("2.0.0".into()),
                size: Some(32),
                sha256: Some("0".repeat(64)),
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x22; 8],
            })
            .unwrap();
        device.poll().unwrap();
        assert!(session.poll().is_err());
        assert_eq!(device.pixels(), painted.as_slice());
        session
            .send_ota(OtaCommand {
                action: "abort".into(),
                version: None,
                size: None,
                sha256: None,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x33; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x33; 8]);
    }

    #[test]
    fn dual_surface_accepts_overlay_and_rejects_unknown() {
        use mdc_core::{Frame, Session};
        let profile = BoardProfile::from_board_id("dual-sim");
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let surfaces = &session.device.as_ref().unwrap().capabilities.surfaces;
        assert_eq!(surfaces.len(), 2);
        assert_eq!(surfaces[1].id, "overlay");
        let main_before = device.pixels().to_vec();
        session
            .send_frame(Frame {
                surface_id: "overlay".into(),
                width: 4,
                height: 2,
                bytes: vec![0xAB; 16],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.overlay_pixels(), &[0xAB; 16]);
        assert_eq!(device.pixels(), main_before.as_slice());
        let missing = session.send_frame(Frame {
            surface_id: "nope".into(),
            width: 4,
            height: 2,
            bytes: vec![0; 16],
        });
        assert!(missing.is_err());
    }

    #[test]
    fn overlay_tile_updates_overlay_and_rejects_out_of_bounds() {
        use mdc_core::{Frame, Session, Tile};
        let profile = BoardProfile::from_board_id("dual-sim");
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let main_before = device.pixels().to_vec();
        session
            .send_frame(Frame {
                surface_id: "overlay".into(),
                width: 4,
                height: 2,
                bytes: vec![0xAB; 16],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_tile(Tile {
                surface_id: "overlay".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0x11, 0x22],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(&device.overlay_pixels()[..2], &[0x11, 0x22]);
        assert_eq!(&device.overlay_pixels()[2..], &[0xAB; 14]);
        assert_eq!(device.pixels(), main_before.as_slice());
        let overlay_before = device.overlay_pixels().to_vec();
        let rejected = session.send_tile(Tile {
            surface_id: "overlay".into(),
            base_frame_id: 1,
            x: 4,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![0, 0],
        });
        assert_eq!(rejected, Err(mdc_core::CoreError::TileBounds));
        let payload = mdc_protocol::encode_tile_payload("overlay", 1, 4, 0, 1, 1, &[0, 0]).unwrap();
        device
            .handle_tile(mdc_protocol::Packet {
                version: mdc_protocol::VERSION,
                kind: mdc_protocol::MessageType::Tile,
                flags: 0,
                request_id: 99,
                payload,
            })
            .unwrap();
        let err = session.poll().unwrap_err();
        assert!(err.to_string().contains("E_DIM"), "{err}");
        assert_eq!(device.overlay_pixels(), overlay_before.as_slice());
        assert_eq!(device.pixels(), main_before.as_slice());
    }

    #[test]
    fn corners_and_two_pointers_cancel_only_the_ones_still_down() {
        use mdc_core::{DeviceManager, ManagerEvent, Session};
        use std::sync::{Arc, Mutex};
        let profile = BoardProfile {
            width: 4,
            height: 2,
            touch: true,
            rotation: 90,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let corners = [(0u16, 0u16), (3, 0), (0, 1), (3, 1)];
        for (x, y) in corners {
            let (tx, ty) = mdc_core::transform_touch(4, 2, 90, x, y).unwrap();
            device
                .emit_input(InputEvent {
                    surface_id: "main".into(),
                    pointer_id: 1,
                    phase: "down".into(),
                    x,
                    y,

                    node: None,
                })
                .unwrap();
            session.poll().unwrap();
            let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    ManagerEvent::Input { event, .. } if event.x == tx && event.y == ty
                )),
                "corner ({x},{y}) -> ({tx},{ty})"
            );
            device
                .emit_input(InputEvent {
                    surface_id: "main".into(),
                    pointer_id: 1,
                    phase: "up".into(),
                    x,
                    y,

                    node: None,
                })
                .unwrap();
            session.poll().unwrap();
            let _ = manager.lock().unwrap().drain_events().count();
        }
        assert!(mdc_core::transform_touch(4, 2, 90, 4, 0).is_err());
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "down".into(),
                x: 0,
                y: 0,

                node: None,
            })
            .unwrap();
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 2,
                phase: "down".into(),
                x: 1,
                y: 0,

                node: None,
            })
            .unwrap();
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 2,
                phase: "up".into(),
                x: 1,
                y: 0,

                node: None,
            })
            .unwrap();
        device.faults.request_disconnect();
        assert!(device.poll().is_err());
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        let cancels: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ManagerEvent::Input { event, .. } if event.phase == "cancel" => {
                    Some(event.pointer_id)
                }
                _ => None,
            })
            .collect();
        assert_eq!(cancels, vec![1]);
    }

    #[test]
    fn rotation_transforms_touch_and_drops_out_of_bounds() {
        use mdc_core::{DeviceManager, ManagerEvent, Session};
        use std::sync::{Arc, Mutex};
        let profile = BoardProfile {
            width: 4,
            height: 2,
            touch: true,
            rotation: 90,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "down".into(),
                x: 0,
                y: 0,

                node: None,
            })
            .unwrap();
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            ManagerEvent::Input { event, .. } if event.x == 1 && event.y == 0
        )));
        assert!(device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "down".into(),
                x: 4,
                y: 0,

                node: None,
            })
            .is_err());
    }

    #[test]
    fn drop_next_keeps_previous_pixels() {
        use mdc_core::{Frame, Session};
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x11; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device.faults.drop_next();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x22; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x11; 8]);
        assert_eq!(session.pending_request_count(), 1);
    }

    #[test]
    fn linux_virt_is_800x480_and_rejects_early_tile() {
        use mdc_core::{Frame, Session, Tile};
        let (host, mut device) = FakeDevice::pair(BoardProfile::from_board_id("linux-virt"));
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let surface = &session.device.as_ref().unwrap().capabilities.surfaces[0];
        assert_eq!((surface.width, surface.height), (800, 480));
        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0, 0],
            })
            .unwrap();
        device.poll().unwrap();
        assert!(session.poll().is_err());
        let bytes = vec![0x44u8; 800 * 480 * 2];
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 800,
                height: 480,
                bytes,
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0x44, 0x44],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(session.pending_request_count(), 0);
    }

    #[test]
    fn pending_frame_is_painted_only_after_the_inflight_ack() {
        use mdc_core::Frame;
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let send = |session: &mut Session<MemoryLink>, value: u8| {
            session
                .send_frame(Frame {
                    surface_id: "main".into(),
                    width: 2,
                    height: 2,
                    bytes: vec![value; 8],
                })
                .unwrap()
        };
        assert_ne!(send(&mut session, 0x11), 0);
        assert_eq!(send(&mut session, 0x22), 0);
        assert_eq!(send(&mut session, 0x33), 0);
        device.poll().unwrap();
        assert_eq!(device.pixels(), &[0x11; 8]);
        session.poll().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x33; 8]);
    }

    #[test]
    fn saturated_queue_rejects_a_tile() {
        use mdc_core::{CoreError, Tile};
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![1; 8],
            })
            .unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![2; 8],
            })
            .unwrap();
        assert_eq!(
            session.send_tile(Tile {
                surface_id: "main".into(),
                base_frame_id: 1,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bytes: vec![0, 0],
            }),
            Err(CoreError::QueueFull)
        );
    }

    #[test]
    fn disconnect_cancels_only_the_pointer_still_down() {
        use mdc_core::{DeviceManager, ManagerEvent};
        use std::sync::{Arc, Mutex};
        let profile = BoardProfile {
            width: 4,
            height: 2,
            touch: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let manager = Arc::new(Mutex::new(DeviceManager::new()));
        let mut session = Session::new(host);
        session.set_manager(manager.clone());
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        let down = |pointer_id, x, y| InputEvent {
            surface_id: "main".into(),
            pointer_id,
            phase: "down".into(),
            x,
            y,

            node: None,
        };
        device.emit_input(down(1, 0, 0)).unwrap();
        device.emit_input(down(2, 1, 0)).unwrap();
        device
            .emit_input(InputEvent {
                surface_id: "main".into(),
                pointer_id: 1,
                phase: "up".into(),
                x: 0,
                y: 0,

                node: None,
            })
            .unwrap();
        session.poll().unwrap();
        device.faults.request_disconnect();
        assert_eq!(device.poll(), Err(SimError::Disconnected));
        session.poll().unwrap();
        let events: Vec<_> = manager.lock().unwrap().drain_events().collect();
        let cancels: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ManagerEvent::Input { event, .. } if event.phase == "cancel" => {
                    Some((event.pointer_id, event.x, event.y))
                }
                _ => None,
            })
            .collect();
        assert_eq!(cancels, vec![(2, 1, 0)]);
    }

    #[test]
    fn ota_virtual_timeout_unfreezes_without_changing_firmware() {
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ota: true,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_ota(OtaCommand {
                action: "begin".into(),
                version: Some("2.0.0".into()),
                size: Some(32),
                sha256: Some("0".repeat(64)),
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        device.clock.advance(5_000);
        device.poll().unwrap();
        assert_eq!(device.firmware(), "1.0.0");
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x55; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x55; 8]);
    }

    #[test]
    fn dropped_commit_keeps_pixels_and_the_next_frame_displays() {
        let profile = BoardProfile {
            width: 2,
            height: 2,
            ..BoardProfile::default()
        };
        let (host, mut device) = FakeDevice::pair(profile);
        let mut session = Session::new(host);
        session.connect().unwrap();
        device.max_chunk = 8;
        device.poll().unwrap();
        session.poll().unwrap();
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x11; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x11; 8]);
        // begin + chunk 放行，丢掉 commit。
        device.faults.drop_one_after(2);
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x22; 8],
            })
            .unwrap();
        device.poll().unwrap();
        let _ = session.poll();
        assert_eq!(device.pixels(), &[0x11; 8]);
        session
            .send_frame(Frame {
                surface_id: "main".into(),
                width: 2,
                height: 2,
                bytes: vec![0x33; 8],
            })
            .unwrap();
        device.poll().unwrap();
        session.poll().unwrap();
        assert_eq!(device.pixels(), &[0x33; 8]);
    }
}
