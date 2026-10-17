//! N114/N115: texture sampling and textured triangle rasterisation for the
//! skinning bake.
//!
//! A [`TextureMap`] holds RGB565 little-endian pixels. Sampling wraps UVs
//! outside `[0, 1]` (glTF `REPEAT`) with either nearest or bilinear filtering
//! (N203). A textured triangle interpolates UVs barycentrically and samples the
//! texel per pixel, multiplying it by an optional tint (N204, the glTF
//! baseColorFactor); when no texture is supplied the triangle is filled with a
//! solid fallback colour.
use crate::BakeError;

/// N203: texture magnification filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Nearest,
    Linear,
}

/// N334: texture wrap mode outside `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wrap {
    #[default]
    Repeat,
    Clamp,
}

/// N204: multiply two RGB565 colours channel-wise (each channel scaled to 8 bits
/// and back, so `0xFFFF` is the identity).
pub fn rgb565_mul(a: u16, b: u16) -> u16 {
    let channel = |shift: u32, mask: u32| {
        let av = u32::from((a >> shift) & mask as u16);
        let bv = u32::from((b >> shift) & mask as u16);
        (((av * bv + mask / 2) / mask) as u16 & mask as u16) << shift
    };
    channel(11, 0x1f) | channel(5, 0x3f) | channel(0, 0x1f)
}

/// An RGB565 little-endian texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureMap {
    pub width: u32,
    pub height: u32,
    pub rgb565: Vec<u8>,
    /// N203: sampling filter (defaults to nearest).
    pub filter: Filter,
    /// N334: wrap mode outside `[0, 1]` (defaults to repeat).
    pub wrap: Wrap,
    /// N285: optional bit-packed alpha mask (1 = opaque). `None` = fully opaque.
    pub alpha: Option<Vec<u8>>,
    /// N344: optional 8-bit alpha channel (one byte per texel). Takes precedence
    /// over `alpha` for soft `BLEND`.
    pub alpha8: Option<Vec<u8>>,
    /// N407: average bilinear samples in linear light (sRGB-correct).
    pub srgb: bool,
}

impl TextureMap {
    /// Wrap already-packed RGB565 pixels, validating the length.
    pub fn from_rgb565(width: u32, height: u32, rgb565: Vec<u8>) -> Result<Self, BakeError> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|p| p.checked_mul(2))
            .ok_or(BakeError::Budget(width, height))?;
        if width == 0 || height == 0 || rgb565.len() != expected {
            return Err(BakeError::Budget(width, height));
        }
        Ok(Self {
            width,
            height,
            rgb565,
            filter: Filter::Nearest,
            wrap: Wrap::Repeat,
            alpha: None,
            alpha8: None,
            srgb: false,
        })
    }

    /// N415: expand a palette-indexed image (one index byte per pixel) into an
    /// RGB565 texture.
    pub fn from_indexed(
        width: u32,
        height: u32,
        palette: &[u16],
        indices: &[u8],
    ) -> Result<Self, BakeError> {
        if width == 0 || height == 0 || indices.len() != (width as usize) * (height as usize) {
            return Err(BakeError::Budget(width, height));
        }
        let mut rgb565 = Vec::with_capacity(indices.len() * 2);
        for index in indices {
            let color = palette.get(*index as usize).copied().unwrap_or(0);
            rgb565.extend_from_slice(&color.to_le_bytes());
        }
        Self::from_rgb565(width, height, rgb565)
    }

    /// Build from a decoded texture (the output of `decode_resize_rgb565`).
    pub fn from_baked(baked: &crate::BakedTexture) -> Result<Self, BakeError> {
        Self::from_rgb565(baked.width, baked.height, baked.rgb565.clone())
    }

    /// N203: set the sampling filter.
    pub fn with_filter(mut self, filter: Filter) -> Self {
        self.filter = filter;
        self
    }

    /// N334: set the wrap mode.
    pub fn with_wrap(mut self, wrap: Wrap) -> Self {
        self.wrap = wrap;
        self
    }

    /// N334: wrap a UV coordinate into `[0, dimension)` (repeat) or clamp it.
    fn wrap_u(&self, value: f32) -> u32 {
        self.wrap_axis(value, self.width)
    }
    fn wrap_v(&self, value: f32) -> u32 {
        self.wrap_axis(value, self.height)
    }
    fn wrap_axis(&self, value: f32, dimension: u32) -> u32 {
        if dimension == 0 {
            return 0;
        }
        match self.wrap {
            Wrap::Repeat => wrap(value, dimension),
            Wrap::Clamp => clamp_coord(value, dimension),
        }
    }
    /// N334: wrap an already-floored integer neighbour index.
    fn wrap_index(&self, index: i64, dimension: u32) -> u32 {
        if dimension == 0 {
            return 0;
        }
        match self.wrap {
            Wrap::Repeat => index.rem_euclid(dimension as i64) as u32,
            Wrap::Clamp => index.clamp(0, dimension as i64 - 1) as u32,
        }
    }

    /// N285: attach a bit-packed alpha mask (1 = opaque).
    pub fn with_alpha(mut self, alpha: Vec<u8>) -> Self {
        self.alpha = Some(alpha);
        self
    }

    /// N344: attach a per-texel 8-bit alpha channel.
    pub fn with_alpha8(mut self, alpha8: Vec<u8>) -> Self {
        self.alpha8 = Some(alpha8);
        self
    }

    /// N407: sample bilinear in linear light (sRGB-correct averaging).
    pub fn with_srgb(mut self, srgb: bool) -> Self {
        self.srgb = srgb;
        self
    }

    /// N344: the source alpha at `uv` (0..=255). Falls back to the 1-bit mask,
    /// then to opaque.
    pub fn alpha_value_at(&self, u: f32, v: f32) -> u8 {
        if self.width == 0 || self.height == 0 {
            return 255;
        }
        let x = self.wrap_u(u * self.width as f32);
        let y = self.wrap_v(v * self.height as f32);
        let index = y as usize * self.width as usize + x as usize;
        if let Some(alpha8) = &self.alpha8 {
            return alpha8.get(index).copied().unwrap_or(255);
        }
        if let Some(mask) = &self.alpha {
            return match mask.get(index / 8) {
                Some(byte) if byte & (1 << (index % 8)) != 0 => 255,
                _ => 0,
            };
        }
        255
    }

    /// N285: whether the texel at `uv` is opaque (`true` when no mask).
    pub fn alpha_at(&self, u: f32, v: f32) -> bool {
        let Some(alpha) = &self.alpha else {
            return true;
        };
        if self.width == 0 || self.height == 0 {
            return true;
        }
        let x = self.wrap_u(u * self.width as f32);
        let y = self.wrap_v(v * self.height as f32);
        let index = y as usize * self.width as usize + x as usize;
        match alpha.get(index / 8) {
            Some(byte) => byte & (1 << (index % 8)) != 0,
            None => true,
        }
    }

    /// The texel at integer `(x, y)`, wrapping.
    fn texel(&self, x: u32, y: u32) -> u16 {
        let x = (x % self.width.max(1)) as usize;
        let y = (y % self.height.max(1)) as usize;
        let index = (y * self.width as usize + x) * 2;
        match self.rgb565.get(index..index + 2) {
            Some(bytes) => u16::from_le_bytes([bytes[0], bytes[1]]),
            None => 0,
        }
    }

    /// Sample `uv` with the configured filter, wrapping outside `[0, 1]`.
    pub fn sample(&self, u: f32, v: f32) -> u16 {
        match self.filter {
            Filter::Nearest => self.sample_nearest(u, v),
            Filter::Linear => self.sample_linear(u, v),
        }
    }

    /// Nearest texel for `uv`, wrapping outside `[0, 1]`.
    pub fn sample_nearest(&self, u: f32, v: f32) -> u16 {
        if self.width == 0 || self.height == 0 {
            return 0;
        }
        let x = self.wrap_u(u * self.width as f32);
        let y = self.wrap_v(v * self.height as f32);
        self.texel(x, y)
    }

    /// N203: bilinear sample for `uv`, wrapping outside `[0, 1]`.
    pub fn sample_linear(&self, u: f32, v: f32) -> u16 {
        if self.width == 0 || self.height == 0 {
            return 0;
        }
        let fx = u * self.width as f32 - 0.5;
        let fy = v * self.height as f32 - 0.5;
        let x0 = fx.floor();
        let y0 = fy.floor();
        let tx = fx - x0;
        let ty = fy - y0;
        let x0 = self.wrap_index(fx.floor() as i64, self.width);
        let y0 = self.wrap_index(fy.floor() as i64, self.height);
        let x1 = self.wrap_index(i64::from(x0) + 1, self.width);
        let y1 = self.wrap_index(i64::from(y0) + 1, self.height);
        let samples = [
            (self.texel(x0, y0), (1.0 - tx) * (1.0 - ty)),
            (self.texel(x1, y0), tx * (1.0 - ty)),
            (self.texel(x0, y1), (1.0 - tx) * ty),
            (self.texel(x1, y1), tx * ty),
        ];
        let channel = |shift: u32, mask: u32| {
            let acc: f32 = samples
                .iter()
                .map(|(value, weight)| {
                    let raw = f32::from((value >> shift) & mask as u16);
                    let raw = if self.srgb {
                        // N407: average in linear light, then return to sRGB.
                        srgb_to_linear(raw / mask as f32)
                    } else {
                        raw
                    };
                    raw * weight
                })
                .sum();
            let acc = if self.srgb {
                linear_to_srgb(acc) * mask as f32
            } else {
                acc
            };
            ((acc.round() as u32).min(mask) as u16) << shift
        };
        channel(11, 0x1f) | channel(5, 0x3f) | channel(0, 0x1f)
    }
}

/// N407: sRGB transfer functions (per-channel 0..1).
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// N364: convert an RGBA (0..1) vertex colour to RGB565.
pub(crate) fn rgba_to_rgb565(color: [f32; 4]) -> u16 {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u16;
    ((c(color[0]) >> 3) << 11) | ((c(color[1]) >> 2) << 5) | (c(color[2]) >> 3)
}

/// N333: source-over blend of `src` into `dst` with source alpha `a` (0..=255).
/// `a == 255` returns `src`; `a == 0` returns `dst`.
pub(crate) fn blend_rgb565(dst: u16, src: u16, a: u8) -> u16 {
    if a == u8::MAX {
        return src;
    }
    if a == 0 {
        return dst;
    }
    let af = u32::from(a);
    let kf = 255 - af;
    let channel = |shift: u32, mask: u16| -> u16 {
        let d = u32::from((dst >> shift) & mask);
        let s = u32::from((src >> shift) & mask);
        ((d * kf + s * af) / 255) as u16
    };
    (channel(11, 0x1f) << 11) | (channel(5, 0x3f) << 5) | channel(0, 0x1f)
}

/// N333: write `value` at `index`, blending over the existing pixel when `blend`
/// is translucent (255 replaces).
pub(crate) fn write_pixel(pixels: &mut [u8], index: usize, value: u16, blend: u8) {
    if index + 1 >= pixels.len() {
        return;
    }
    let out = if blend == u8::MAX {
        value
    } else {
        blend_rgb565(
            u16::from_le_bytes([pixels[index], pixels[index + 1]]),
            value,
            blend,
        )
    };
    let bytes = out.to_le_bytes();
    pixels[index] = bytes[0];
    pixels[index + 1] = bytes[1];
}

/// N327: scale an RGB565 colour's channels by `factor` (0..=1) and repack.
pub(crate) fn rgb565_scale(color: u16, factor: f32) -> u16 {
    let f = factor.clamp(0.0, 1.0);
    let r = ((color >> 11) & 0x1f) as f32 * f;
    let g = ((color >> 5) & 0x3f) as f32 * f;
    let b = (color & 0x1f) as f32 * f;
    (((r.round() as u16) & 0x1f) << 11)
        | (((g.round() as u16) & 0x3f) << 5)
        | ((b.round() as u16) & 0x1f)
}

/// N334: clamp a UV coordinate into `[0, dimension)`.
fn clamp_coord(value: f32, dimension: u32) -> u32 {
    if !value.is_finite() || dimension == 0 {
        return 0;
    }
    (value.floor().max(0.0) as u32).min(dimension - 1)
}

/// Map a possibly out-of-range coordinate onto `[0, dimension)` with repeat.
fn wrap(value: f32, dimension: u32) -> u32 {
    if !value.is_finite() || dimension == 0 {
        return 0;
    }
    let dimension = dimension as i64;
    let floored = value.floor() as i64;
    floored.rem_euclid(dimension) as u32
}

fn edge(a: (i16, i16), b: (i16, i16), p: (i16, i16)) -> i32 {
    (i32::from(p.0) - i32::from(a.0)) * (i32::from(b.1) - i32::from(a.1))
        - (i32::from(p.1) - i32::from(a.1)) * (i32::from(b.0) - i32::from(a.0))
}

/// Draw `points` into a fresh framebuffer, sampling `texture` through `uvs`
/// (barycentric). With no texture or no UVs, fills `fallback` colour instead.
#[allow(clippy::too_many_arguments)]
pub(crate) fn raster_textured(
    points: &[(i16, i16)],
    uvs: Option<&[[f32; 2]]>,
    indices: &[u16],
    width: u32,
    height: u32,
    texture: Option<&TextureMap>,
    tint: Option<u16>,
    mask: bool,
    fallback: u16,
) -> Result<Vec<u8>, BakeError> {
    let indices: Vec<u32> = indices.iter().map(|&i| u32::from(i)).collect();
    raster_indexed(
        points, uvs, &indices, width, height, texture, tint, mask, fallback, 64,
    )
}

/// N165: raster a geometry's triangles (u32 indices) into a fresh framebuffer,
/// bounded by `max_points` vertices.
#[allow(clippy::too_many_arguments)]
pub(crate) fn raster_indexed(
    points: &[(i16, i16)],
    uvs: Option<&[[f32; 2]]>,
    indices: &[u32],
    width: u32,
    height: u32,
    texture: Option<&TextureMap>,
    tint: Option<u16>,
    mask: bool,
    fallback: u16,
    max_points: usize,
) -> Result<Vec<u8>, BakeError> {
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return Err(BakeError::Budget(width, height));
    }
    let mut pixels = vec![0u8; (width as usize) * (height as usize) * 2];
    draw_indexed(
        &mut pixels,
        points,
        uvs,
        None,
        indices,
        width,
        height,
        texture,
        tint,
        mask,
        255,
        fallback,
        None,
        None,
        mdc_model::UvTransform::default(),
        None,
        None,
        max_points,
    )?;
    Ok(pixels)
}

/// N183: draw a geometry's triangles into an existing framebuffer (so a scene of
/// several meshes can share one target). Bounded by `max_points` vertices.
/// N379: per-pixel tangent-space normal mapping inputs for a triangle.
pub(crate) struct NormalMap<'a> {
    /// Per-vertex normals, parallel to the points.
    pub normals: &'a [[f32; 3]],
    /// Per-vertex tangents (xyzw; `w` is the handedness sign).
    pub tangents: &'a [[f32; 4]],
    /// The tangent-space normal map.
    pub map: &'a TextureMap,
    /// `normalTexture.scale`.
    pub scale: f32,
    /// Directional key light.
    pub light: [f32; 3],
    /// Ambient floor for the Lambert term.
    pub ambient: f32,
    /// N387: Blinn-Phong specular strength (0 = no highlight).
    pub specular: f32,
    /// N387: Blinn-Phong shininess exponent.
    pub shininess: f32,
    /// N395: the normal texture's own UV transform.
    pub uv_transform: mdc_model::UvTransform,
    /// N394: metallicRoughness texture (G = roughness).
    pub roughness_map: Option<&'a TextureMap>,
    /// N394: the material's `roughnessFactor`.
    pub roughness_factor: f32,
    /// N395: the metallicRoughness texture's own UV transform.
    pub roughness_uv_transform: mdc_model::UvTransform,
    /// N406: occlusion texture (R = ambient occlusion).
    pub occlusion_map: Option<&'a TextureMap>,
    /// N406: the material's `occlusionStrength`.
    pub occlusion_strength: f32,
    /// N395: the occlusion texture's own UV transform.
    pub occlusion_uv_transform: mdc_model::UvTransform,
    /// N414: clearcoat factor (0 = none) and its shininess.
    pub clearcoat: f32,
    pub clearcoat_shininess: f32,
}

/// N393: an additive emissive texture layered on top of the shaded colour.
pub(crate) struct EmissiveMap<'a> {
    pub map: &'a TextureMap,
    pub strength: f32,
    /// N395: the emissive texture's own UV transform.
    pub uv_transform: mdc_model::UvTransform,
}

/// N387: saturating per-channel RGB565 add (for an additive specular term).
fn rgb565_add(a: u16, b: u16) -> u16 {
    let r = (((a >> 11) & 0x1f) + ((b >> 11) & 0x1f)).min(0x1f);
    let g = (((a >> 5) & 0x3f) + ((b >> 5) & 0x3f)).min(0x3f);
    let bl = ((a & 0x1f) + (b & 0x1f)).min(0x1f);
    (r << 11) | (g << 5) | bl
}

fn v3_dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn v3_normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-6 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn v3_cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// N386: a 1×1 normal map pointing straight out of the surface, used with a zero
/// scale to get per-pixel (Gouraud) Lambert shading without a real map.
pub(crate) fn flat_normal_map() -> TextureMap {
    // RGB(128, 128, 255) -> tangent-space normal (0, 0, 1).
    TextureMap::from_rgb565(1, 1, vec![0x1F, 0x84]).expect("1x1 texture")
}

/// N379: RGB565 -> linear (0..1) channels, for decoding a normal map texel.
fn rgb565_channels(value: u16) -> [f32; 3] {
    [
        f32::from((value >> 11) & 0x1f) / 31.0,
        f32::from((value >> 5) & 0x3f) / 63.0,
        f32::from(value & 0x1f) / 31.0,
    ]
}

/// N379/N387: apply per-pixel Lambert + specular from the normal bundle.
pub(crate) fn apply_normal_map(
    value: u16,
    nm: &NormalMap<'_>,
    tri: [usize; 3],
    weights: [f32; 3],
    uv: [f32; 2],
    uv_secondary: [f32; 2],
) -> u16 {
    let [w0, w1, w2] = weights;
    let interp = |k: usize| {
        w0 * nm.tangents[tri[0]][k] + w1 * nm.tangents[tri[1]][k] + w2 * nm.tangents[tri[2]][k]
    };
    let n = v3_normalize([
        w0 * nm.normals[tri[0]][0] + w1 * nm.normals[tri[1]][0] + w2 * nm.normals[tri[2]][0],
        w0 * nm.normals[tri[0]][1] + w1 * nm.normals[tri[1]][1] + w2 * nm.normals[tri[2]][1],
        w0 * nm.normals[tri[0]][2] + w1 * nm.normals[tri[1]][2] + w2 * nm.normals[tri[2]][2],
    ]);
    let t = v3_normalize([interp(0), interp(1), interp(2)]);
    let handed = interp(3).signum();
    let cross = v3_cross(n, t);
    let b = [cross[0] * handed, cross[1] * handed, cross[2] * handed];
    let texel = rgb565_channels(nm.map.sample(uv[0], uv[1]));
    let nt = v3_normalize([
        (texel[0] * 2.0 - 1.0) * nm.scale,
        (texel[1] * 2.0 - 1.0) * nm.scale,
        texel[2] * 2.0 - 1.0,
    ]);
    let world = v3_normalize([
        t[0] * nt[0] + b[0] * nt[1] + n[0] * nt[2],
        t[1] * nt[0] + b[1] * nt[1] + n[1] * nt[2],
        t[2] * nt[0] + b[2] * nt[1] + n[2] * nt[2],
    ]);
    // N394: a metallicRoughness texture raises the per-teixel ambient floor by
    // its green (roughness) channel, matching `material_ambient`.
    let ambient = match nm.roughness_map {
        Some(map) => {
            let [ru, rv] = nm.roughness_uv_transform.apply(uv_secondary);
            let texel = map.sample(ru, rv);
            let roughness =
                nm.roughness_factor.clamp(0.0, 1.0) * (f32::from((texel >> 5) & 0x3f) / 63.0);
            (nm.ambient + 0.35 * roughness).min(0.95)
        }
        None => nm.ambient,
    };
    let lux = v3_dot(world, nm.light).clamp(ambient, 1.0);
    let mut value = rgb565_scale(value, lux);
    if nm.specular > 0.0 {
        let half = v3_normalize([nm.light[0], nm.light[1], nm.light[2] + 1.0]);
        let spec = nm.specular * v3_dot(world, half).max(0.0).powf(nm.shininess);
        if spec > 0.0 {
            value = rgb565_add(value, rgb565_scale(0xFFFF, spec.clamp(0.0, 1.0)));
        }
    }
    // N414: a clearcoat second specular lobe from the smooth surface normal.
    if nm.clearcoat > 0.0 {
        let half = v3_normalize([nm.light[0], nm.light[1], nm.light[2] + 1.0]);
        let spec = nm.clearcoat * v3_dot(n, half).max(0.0).powf(nm.clearcoat_shininess);
        if spec > 0.0 {
            value = rgb565_add(value, rgb565_scale(0xFFFF, spec.clamp(0.0, 1.0)));
        }
    }
    // N406: per-pixel ambient occlusion (R channel), direction-independent.
    if let Some(map) = nm.occlusion_map {
        let [ou, ov] = nm.occlusion_uv_transform.apply(uv_secondary);
        let texel = map.sample(ou, ov);
        let red = f32::from((texel >> 11) & 0x1f) / 31.0;
        let ao = (1.0 - nm.occlusion_strength.clamp(0.0, 1.0) * (1.0 - red)).clamp(0.2, 1.0);
        value = rgb565_scale(value, ao);
    }
    value
}

/// N327: `shade` is an optional per-triangle factor (0..=1) applied to the
/// tint/fallback colour.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_indexed(
    pixels: &mut [u8],
    points: &[(i16, i16)],
    uvs: Option<&[[f32; 2]]>,
    uvs2: Option<&[[f32; 2]]>,
    indices: &[u32],
    width: u32,
    height: u32,
    texture: Option<&TextureMap>,
    tint: Option<u16>,
    mask: bool,
    blend: u8,
    fallback: u16,
    shade: Option<&[f32]>,
    colors: Option<&[[f32; 4]]>,
    uv_transform: mdc_model::UvTransform,
    normal_map: Option<NormalMap<'_>>,
    emissive: Option<EmissiveMap<'_>>,
    max_points: usize,
) -> Result<(), BakeError> {
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return Err(BakeError::Budget(width, height));
    }
    if points.len() > max_points {
        return Err(BakeError::FrameCount(max_points, points.len()));
    }
    let triangles: Vec<[usize; 3]> = if indices.len() >= 3 {
        indices
            .chunks_exact(3)
            .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
            .collect()
    } else if points.len() >= 3 {
        (1..points.len() - 1).map(|i| [0, i, i + 1]).collect()
    } else {
        Vec::new()
    };
    for (ti, triangle) in triangles.into_iter().enumerate() {
        let (Some(&a), Some(&b), Some(&c)) = (
            points.get(triangle[0]),
            points.get(triangle[1]),
            points.get(triangle[2]),
        ) else {
            continue;
        };
        // N327: shade the tint/fallback for this triangle.
        let factor = shade.and_then(|s| s.get(ti)).copied().unwrap_or(1.0);
        let tint = tint.map(|t| rgb565_scale(t, factor));
        let fill = rgb565_scale(fallback, factor);
        // N364: per-vertex colours for this triangle.
        let vc = colors
            .filter(|c| c.len() >= points.len())
            .map(|c| (c[triangle[0]], c[triangle[1]], c[triangle[2]]));
        let uvs = uvs.filter(|u| u.len() >= points.len());
        // N403: the secondary maps may sample a different UV set.
        let uv2 = uvs2
            .filter(|u| u.len() >= points.len())
            .map(|u| (u[triangle[0]], u[triangle[1]], u[triangle[2]]));
        match (texture, uvs) {
            (Some(texture), Some(uvs)) => fill_textured_triangle(
                pixels,
                width,
                height,
                (a, uvs[triangle[0]]),
                (b, uvs[triangle[1]]),
                (c, uvs[triangle[2]]),
                texture,
                tint,
                mask,
                blend,
                vc,
                uv_transform,
                uv2,
                normal_map
                    .as_ref()
                    .filter(|nm| {
                        nm.normals.len() >= points.len() && nm.tangents.len() >= points.len()
                    })
                    .map(|nm| (nm, triangle)),
                emissive.as_ref(),
            ),
            _ => crate::fill_triangle(
                pixels,
                width,
                height,
                a,
                b,
                c,
                fill,
                blend,
                vc,
                normal_map
                    .as_ref()
                    .filter(|nm| {
                        nm.normals.len() >= points.len() && nm.tangents.len() >= points.len()
                    })
                    .map(|nm| (nm, triangle)),
            ),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn fill_textured_triangle(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    a: ((i16, i16), [f32; 2]),
    b: ((i16, i16), [f32; 2]),
    c: ((i16, i16), [f32; 2]),
    texture: &TextureMap,
    tint: Option<u16>,
    mask: bool,
    blend: u8,
    vc: Option<([f32; 4], [f32; 4], [f32; 4])>,
    uv_transform: mdc_model::UvTransform,
    uv2: Option<([f32; 2], [f32; 2], [f32; 2])>,
    normal_map: Option<(&NormalMap<'_>, [usize; 3])>,
    emissive: Option<&EmissiveMap<'_>>,
) {
    let area = edge(a.0, b.0, c.0);
    if area == 0 {
        return;
    }
    let min_x = a.0 .0.min(b.0 .0).min(c.0 .0).max(0) as u32;
    let max_x = a.0 .0.max(b.0 .0).max(c.0 .0).max(0) as u32;
    let min_y = a.0 .1.min(b.0 .1).min(c.0 .1).max(0) as u32;
    let max_y = a.0 .1.max(b.0 .1).max(c.0 .1).max(0) as u32;
    let max_x = max_x.min(width.saturating_sub(1));
    let max_y = max_y.min(height.saturating_sub(1));
    let area_f = area as f32;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let p = (x as i16, y as i16);
            let e0 = edge(b.0, c.0, p);
            let e1 = edge(c.0, a.0, p);
            let e2 = edge(a.0, b.0, p);
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
            let u = w0 * a.1[0] + w1 * b.1[0] + w2 * c.1[0];
            let v = w0 * a.1[1] + w1 * b.1[1] + w2 * c.1[1];
            // N373/N395: each slot applies its own KHR_texture_transform.
            let raw = [u, v];
            // N403: the secondary maps interpolate their own UV set when present.
            let raw2 = match uv2 {
                Some((a2, b2, c2)) => [
                    w0 * a2[0] + w1 * b2[0] + w2 * c2[0],
                    w0 * a2[1] + w1 * b2[1] + w2 * c2[1],
                ],
                None => raw,
            };
            let [u, v] = uv_transform.apply(raw);
            if mask && !texture.alpha_at(u, v) {
                continue;
            }
            let mut value = texture.sample(u, v);
            if let Some(tint) = tint {
                value = rgb565_mul(value, tint);
            }
            // N364: multiply the interpolated vertex colour.
            if let Some((c0, c1, c2)) = vc {
                let interp = [
                    w0 * c0[0] + w1 * c1[0] + w2 * c2[0],
                    w0 * c0[1] + w1 * c1[1] + w2 * c2[1],
                    w0 * c0[2] + w1 * c1[2] + w2 * c2[2],
                    w0 * c0[3] + w1 * c1[3] + w2 * c2[3],
                ];
                value = rgb565_mul(value, rgba_to_rgb565(interp));
            }
            // N379: per-pixel Lambert using the tangent-space normal map.
            if let Some((nm, tri)) = normal_map {
                let [nu, nv] = nm.uv_transform.apply(raw2);
                value = apply_normal_map(value, nm, tri, [w0, w1, w2], [nu, nv], raw2);
            }
            // N393: add the emissive texture.
            if let Some(em) = emissive {
                let [eu, ev] = em.uv_transform.apply(raw2);
                let texel = em.map.sample(eu, ev);
                value = rgb565_add(value, rgb565_scale(texel, em.strength.clamp(0.0, 1.0)));
            }
            // N344: for a blended material, fold the per-texel alpha into the
            // material alpha so soft edges fade instead of cutting out.
            let effective = if blend == u8::MAX {
                u8::MAX
            } else {
                (u16::from(blend) * u16::from(texture.alpha_value_at(u, v)) / 255) as u8
            };
            let index = ((y * width + x) * 2) as usize;
            write_pixel(pixels, index, value, effective);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, value: u16) -> TextureMap {
        let mut rgb565 = Vec::with_capacity((width * height * 2) as usize);
        for _ in 0..width * height {
            rgb565.extend_from_slice(&value.to_le_bytes());
        }
        TextureMap::from_rgb565(width, height, rgb565).unwrap()
    }

    #[test]
    fn srgb_linear_filtering_is_brighter_for_black_white() {
        // N407: averaging black and white in linear light lands above mid-grey.
        let texture = TextureMap::from_rgb565(2, 1, vec![0x00, 0x00, 0xFF, 0xFF]).unwrap();
        let plain = texture.sample_linear(0.5, 0.5);
        let srgb = texture.with_srgb(true).sample_linear(0.5, 0.5);
        let red = |v: u16| v >> 11;
        assert!(red(srgb) > red(plain), "sRGB average is brighter");
    }

    #[test]
    fn alpha_value_prefers_the_8bit_channel() {
        let map = TextureMap::from_rgb565(2, 1, vec![0, 0, 0, 0])
            .unwrap()
            .with_alpha8(vec![0, 200]);
        assert_eq!(map.alpha_value_at(0.0, 0.0), 0);
        assert_eq!(map.alpha_value_at(0.75, 0.0), 200);
        // No alpha -> opaque.
        let opaque = TextureMap::from_rgb565(2, 1, vec![0, 0, 0, 0]).unwrap();
        assert_eq!(opaque.alpha_value_at(0.0, 0.0), 255);
    }

    #[test]
    fn clamp_wrap_repeats_the_edge_texel() {
        let mut rgb = Vec::new();
        rgb.extend_from_slice(&0xF800u16.to_le_bytes()); // red
        rgb.extend_from_slice(&0x001Fu16.to_le_bytes()); // blue
        let map = TextureMap::from_rgb565(2, 1, rgb).unwrap();
        assert_eq!(map.sample_nearest(0.0, 0.0), 0xF800);
        assert_eq!(
            map.clone().with_wrap(Wrap::Repeat).sample_nearest(2.0, 0.0),
            0xF800
        );
        assert_eq!(map.with_wrap(Wrap::Clamp).sample_nearest(2.0, 0.0), 0x001F);
    }

    #[test]
    fn blend_rgb565_is_source_over() {
        // Full alpha replaces, zero keeps, half is between.
        assert_eq!(blend_rgb565(0xF800, 0x001F, 255), 0x001F);
        assert_eq!(blend_rgb565(0xF800, 0x001F, 0), 0xF800);
        let mid = blend_rgb565(0xF800, 0x001F, 128); // red source over blue dest
        assert!(
            ((mid >> 11) & 0x1f) > 0 && (mid & 0x1f) > 0,
            "both channels"
        );
    }

    #[test]
    fn from_rgb565_validates_and_samples_nearest_texels() {
        assert!(matches!(
            TextureMap::from_rgb565(2, 2, vec![0, 0]),
            Err(BakeError::Budget(_, _))
        ));
        // A 2×1 texture: left red, right green.
        let texture = TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
        assert_eq!(texture.sample(0.1, 0.5), 0xF800);
        assert_eq!(texture.sample(0.6, 0.5), 0x07E0);
        // UVs wrap, so 1.0 maps back to the left texel.
        assert_eq!(texture.sample(1.0, 0.5), 0xF800);
    }

    #[test]
    fn textured_raster_samples_within_the_triangle() {
        // Left half red, right half green over a full-screen quad.
        let texture = TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
        let points = [(0i16, 0i16), (8, 0), (8, 8), (0, 8)];
        let uvs = [[0.0f32, 0.0f32], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let indices = [0u16, 1, 2, 0, 2, 3];
        let pixels = raster_textured(
            &points,
            Some(&uvs),
            &indices,
            8,
            8,
            Some(&texture),
            None,
            false,
            0,
        )
        .unwrap();
        let at = |x: usize, y: usize| {
            u16::from_le_bytes([pixels[(y * 8 + x) * 2], pixels[(y * 8 + x) * 2 + 1]])
        };
        assert_eq!(at(1, 4), 0xF800);
        assert_eq!(at(6, 4), 0x07E0);
    }

    #[test]
    fn bilinear_samples_blend_neighbouring_texels() {
        // 2×1 texture: left red, right green.
        let texture = TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07]).unwrap();
        assert_eq!(
            texture.sample_nearest(0.5, 0.5),
            0x07E0,
            "0.5 rounds to the right texel"
        );
        let mid = texture.sample_linear(0.5, 0.5);
        let linear = texture.with_filter(Filter::Linear);
        assert_eq!(linear.sample(0.5, 0.5), mid);
        let r = (mid >> 11) & 0x1f;
        let g = (mid >> 5) & 0x3f;
        assert!((10..=22).contains(&r), "red channel ~half ({r})");
        assert!((20..=44).contains(&g), "green channel ~half ({g})");
    }

    #[test]
    fn tint_multiplies_channels_and_identity_is_white() {
        assert_eq!(rgb565_mul(0xF800, 0xFFFF), 0xF800);
        assert_eq!(rgb565_mul(0x8410, 0xFFFF), 0x8410);
        // Half-intensity red stays red at half value; red × green is black.
        assert_eq!(rgb565_mul(0xF800, 0x8000), 0x8000);
        assert_eq!(rgb565_mul(0xF800, 0x07E0), 0x0000);
    }

    #[test]
    fn textured_raster_applies_the_tint() {
        let texture = TextureMap::from_rgb565(1, 1, vec![0x00, 0xF8]).unwrap(); // 1×1 red
        let points = [(0i16, 0i16), (4, 0), (0, 4)];
        let uvs = [[0.5f32, 0.5], [0.5, 0.5], [0.5, 0.5]];
        let pixels = raster_textured(
            &points,
            Some(&uvs),
            &[],
            4,
            4,
            Some(&texture),
            Some(0x8000),
            false,
            0,
        )
        .unwrap();
        assert_eq!(&pixels[0..2], &[0x00, 0x80], "red halved by the tint");
    }

    #[test]
    fn alpha_mask_cuts_out_transparent_texels() {
        // 2×1 texture: left red (opaque), right green (transparent).
        let texture = TextureMap::from_rgb565(2, 1, vec![0x00, 0xF8, 0xE0, 0x07])
            .unwrap()
            .with_alpha(vec![0b01]);
        assert!(texture.alpha_at(0.1, 0.5));
        assert!(!texture.alpha_at(0.6, 0.5));

        let points = [(0i16, 0i16), (8, 0), (8, 8), (0, 8)];
        let uvs = [[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let indices = [0u16, 1, 2, 0, 2, 3];
        let pixels = raster_textured(
            &points,
            Some(&uvs),
            &indices,
            8,
            8,
            Some(&texture),
            None,
            true,
            0x001F,
        )
        .unwrap();
        let at = |x: usize, y: usize| {
            u16::from_le_bytes([pixels[(y * 8 + x) * 2], pixels[(y * 8 + x) * 2 + 1]])
        };
        assert_eq!(at(1, 4), 0xF800, "opaque half drawn");
        assert_eq!(at(6, 4), 0, "transparent half left clear");
    }

    #[test]
    fn missing_texture_or_uv_falls_back_to_solid() {
        let points = [(0i16, 0i16), (4, 0), (0, 4)];
        let pixels = raster_textured(&points, None, &[], 8, 8, None, None, false, 0xF800).unwrap();
        assert_eq!(&pixels[0..2], &[0x00, 0xF8]);
        // A texture but no UVs also falls back.
        let texture = solid(2, 2, 0x07E0);
        let pixels = raster_textured(
            &points,
            None,
            &[],
            8,
            8,
            Some(&texture),
            None,
            false,
            0xF800,
        )
        .unwrap();
        assert_eq!(&pixels[0..2], &[0x00, 0xF8]);
    }
}
