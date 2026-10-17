//! N336: a dependency-free baseline JPEG encoder.
//!
//! Straightforward 4:4:4 (no chroma subsampling) baseline JPEG with the standard
//! Annex K quantisation and Huffman tables, scaled by a quality factor. Used to
//! serve an OBS-friendly `/frame.jpg` that is far smaller than a stored-deflate
//! PNG. Not a general-purpose image library: no restart markers, no progressive.

/// Natural-order quantisation tables (Annex K), scaled by quality.
const QUANT_LUMA: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const QUANT_CHROMA: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// Zig-zag scan order (natural index for each zig-zag position).
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_LUMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_CHROMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

const AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const AC_LUMA_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];
const AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const AC_CHROMA_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

/// Precomputed Huffman codes/sizes for a table.
struct Huff {
    codes: [u16; 256],
    sizes: [u8; 256],
}

impl Huff {
    fn new(bits: &[u8; 16], values: &[u8]) -> Self {
        let mut huff = Huff {
            codes: [0; 256],
            sizes: [0; 256],
        };
        let mut code: u32 = 0;
        let mut k = 0usize;
        for (i, &count) in bits.iter().enumerate() {
            let len = (i + 1) as u8;
            for _ in 0..count {
                let value = values[k] as usize;
                k += 1;
                huff.codes[value] = code as u16;
                huff.sizes[value] = len;
                code += 1;
            }
            code <<= 1;
        }
        huff
    }
}

/// MSB-first bit writer with JPEG 0xFF byte stuffing and 1-bit padding.
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    nbits: u8,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter {
            out: Vec::new(),
            acc: 0,
            nbits: 0,
        }
    }
    fn put(&mut self, code: u16, size: u8) {
        for i in (0..size).rev() {
            self.push((code >> i) & 1 == 1);
        }
    }
    fn push(&mut self, bit: bool) {
        self.acc = (self.acc << 1) | u32::from(bit);
        self.nbits += 1;
        if self.nbits == 8 {
            let byte = self.acc as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0x00);
            }
            self.acc = 0;
            self.nbits = 0;
        }
    }
    /// Pad the last byte with 1 bits (spec) and precompute-induce no stuffing.
    fn flush(&mut self) {
        while self.nbits != 0 {
            self.push(true);
        }
    }
}

/// N336: encode a BGRA8888 frame as a baseline JPEG at `quality` (1..100).
/// Returns an empty vector for degenerate geometry.
pub fn encode_jpeg(width: u16, height: u16, bgra: &[u8], quality: u8) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let quality = quality.clamp(1, 100) as u32;
    // Annex K quality scaling.
    let scale = if quality < 50 {
        5000 / quality
    } else {
        200 - quality * 2
    };
    let scale_table = |table: &[u16; 64]| -> [u16; 64] {
        let mut out = [0u16; 64];
        for (i, &value) in table.iter().enumerate() {
            let scaled = (u32::from(value) * scale + 50) / 100;
            out[i] = scaled.clamp(1, 255) as u16;
        }
        out
    };
    let q_luma = scale_table(&QUANT_LUMA);
    let q_chroma = scale_table(&QUANT_CHROMA);

    let dc_luma = Huff::new(&DC_LUMA_BITS, &DC_LUMA_VALUES);
    let ac_luma = Huff::new(&AC_LUMA_BITS, &AC_LUMA_VALUES);
    let dc_chroma = Huff::new(&DC_CHROMA_BITS, &DC_CHROMA_VALUES);
    let ac_chroma = Huff::new(&AC_CHROMA_BITS, &AC_CHROMA_VALUES);

    // Basis for the separable 8x8 DCT: basis[u][x] = C(u)/2 * cos((2x+1)u pi/16).
    let mut basis = [[0.0f32; 8]; 8];
    for (u, row) in basis.iter_mut().enumerate() {
        let cu = if u == 0 {
            std::f32::consts::FRAC_1_SQRT_2
        } else {
            1.0
        };
        for (x, slot) in row.iter_mut().enumerate() {
            *slot =
                cu / 2.0 * (((2 * x + 1) as f32) * (u as f32) * std::f32::consts::PI / 16.0).cos();
        }
    }

    let mut out = Vec::with_capacity(w * h);
    // SOI
    out.extend_from_slice(&[0xFF, 0xD8]);
    // APP0 JFIF
    out.extend_from_slice(&[
        0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00,
        0x01, 0x00, 0x00,
    ]);
    // DQT (both tables, zig-zag order)
    out.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]);
    for &i in &ZIGZAG {
        out.push(q_luma[i] as u8);
    }
    out.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x01]);
    for &i in &ZIGZAG {
        out.push(q_chroma[i] as u8);
    }
    // SOF0 (baseline, 3 components, 4:4:4)
    out.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&[0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
    // DHT: DC luma, AC luma, DC chroma, AC chroma
    push_dht(&mut out, 0x00, &DC_LUMA_BITS, &DC_LUMA_VALUES);
    push_dht(&mut out, 0x10, &AC_LUMA_BITS, &AC_LUMA_VALUES);
    push_dht(&mut out, 0x01, &DC_CHROMA_BITS, &DC_CHROMA_VALUES);
    push_dht(&mut out, 0x11, &AC_CHROMA_BITS, &AC_CHROMA_VALUES);
    // SOS
    out.extend_from_slice(&[
        0xFF, 0xDA, 0x00, 0x0C, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00,
    ]);

    let mut writer = BitWriter::new();
    let mut prev_dc = [0i32; 3];
    let blocks_x = w.div_ceil(8);
    let blocks_y = h.div_ceil(8);
    let mut block = [0f32; 64];
    let mut quantised = [0i32; 64];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            for (component, prev) in prev_dc.iter_mut().enumerate() {
                // Gather the 8x8 sample block, clamping to the frame edge.
                for y in 0..8 {
                    for x in 0..8 {
                        let px = (bx * 8 + x).min(w - 1);
                        let py = (by * 8 + y).min(h - 1);
                        let src = (py * w + px) * 4;
                        let (r, g, b) = match bgra.get(src..src + 4) {
                            Some(p) => (f32::from(p[2]), f32::from(p[1]), f32::from(p[0])),
                            None => (0.0, 0.0, 0.0),
                        };
                        // BT.601 full-range YCbCr.
                        let value = match component {
                            0 => 0.299 * r + 0.587 * g + 0.114 * b,
                            1 => -0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0,
                            _ => 0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0,
                        };
                        block[y * 8 + x] = value - 128.0;
                    }
                }
                dct(&mut block, &basis);
                let table = if component == 0 { &q_luma } else { &q_chroma };
                for i in 0..64 {
                    quantised[i] = (block[i] / f32::from(table[i])).round() as i32;
                }
                let (dc, ac) = if component == 0 {
                    (&dc_luma, &ac_luma)
                } else {
                    (&dc_chroma, &ac_chroma)
                };
                encode_block(&mut writer, &quantised, prev, dc, ac);
            }
        }
    }
    writer.flush();
    out.extend_from_slice(&writer.out);
    out.extend_from_slice(&[0xFF, 0xD9]); // EOI
    out
}

fn push_dht(out: &mut Vec<u8>, table_class: u8, bits: &[u8; 16], values: &[u8]) {
    let len = 2 + 1 + 16 + values.len();
    out.extend_from_slice(&[0xFF, 0xC4]);
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.push(table_class);
    out.extend_from_slice(bits);
    out.extend_from_slice(values);
}

/// In-place separable 2D DCT of an 8x8 block.
fn dct(block: &mut [f32; 64], basis: &[[f32; 8]; 8]) {
    let mut tmp = [0f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            let mut sum = 0.0;
            for x in 0..8 {
                sum += block[y * 8 + x] * basis[u][x];
            }
            tmp[y * 8 + u] = sum;
        }
    }
    for x in 0..8 {
        for v in 0..8 {
            let mut sum = 0.0;
            for y in 0..8 {
                sum += tmp[y * 8 + x] * basis[v][y];
            }
            block[v * 8 + x] = sum;
        }
    }
}

fn encode_block(
    writer: &mut BitWriter,
    quantised: &[i32; 64],
    prev_dc: &mut i32,
    dc: &Huff,
    ac: &Huff,
) {
    let diff = quantised[0] - *prev_dc;
    *prev_dc = quantised[0];
    let (size, bits) = magnitude(diff);
    writer.put(dc.codes[size as usize], dc.sizes[size as usize]);
    if size > 0 {
        writer.put(bits, size);
    }
    let mut run = 0u32;
    for k in 1..64 {
        let coeff = quantised[ZIGZAG[k]];
        if coeff == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            writer.put(ac.codes[0xF0], ac.sizes[0xF0]);
            run -= 16;
        }
        let (size, bits) = magnitude(coeff);
        let symbol = ((run as u16) << 4) | u16::from(size);
        writer.put(ac.codes[symbol as usize], ac.sizes[symbol as usize]);
        writer.put(bits, size);
        run = 0;
    }
    if run > 0 {
        writer.put(ac.codes[0x00], ac.sizes[0x00]); // EOB
    }
}

/// JPEG amplitude category and its value bits.
fn magnitude(value: i32) -> (u8, u16) {
    let abs = value.unsigned_abs();
    let mut size = 0u8;
    while (abs >> size) != 0 {
        size += 1;
    }
    if size == 0 {
        return (0, 0);
    }
    let bits = if value < 0 {
        (value + (1 << size) - 1) as u16
    } else {
        value as u16
    };
    (size, bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magnitude_matches_the_jpeg_amplitude_categories() {
        assert_eq!(magnitude(0), (0, 0));
        assert_eq!(magnitude(1), (1, 1));
        assert_eq!(magnitude(-1), (1, 0));
        assert_eq!(magnitude(3), (2, 3));
        assert_eq!(magnitude(-3), (2, 0));
        assert_eq!(magnitude(7), (3, 7));
    }

    #[test]
    fn jpeg_has_soi_markers_and_an_eoi() {
        let bgra = vec![0u8, 0, 255, 255]; // 1x1 red
        let jpeg = encode_jpeg(1, 1, &bgra, 80);
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "SOI");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "EOI");
        assert!(jpeg.windows(2).any(|w| w == [0xFF, 0xDB]), "DQT");
        assert!(jpeg.windows(2).any(|w| w == [0xFF, 0xC0]), "SOF0");
        assert!(jpeg.windows(2).any(|w| w == [0xFF, 0xC4]), "DHT");
        assert!(jpeg.windows(2).any(|w| w == [0xFF, 0xDA]), "SOS");
        assert!(encode_jpeg(0, 4, &[], 80).is_empty());
    }
}
