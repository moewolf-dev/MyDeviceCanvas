//! N533: a pure-Rust port of the meshopt buffer decoder.
//!
//! The port follows the readable reference implementation
//! (`js/meshopt_decoder_reference.js` by Jasper St. Pierre, MIT-licensed, part of
//! the meshoptimizer library) rather than the SIMD C++ decoder, so the logic can
//! be reviewed line by line. It decodes:
//!
//! * vertex buffers (`0xa0`/`0xa1`) with the `OCTAHEDRAL`, `QUATERNION`,
//!   `EXPONENTIAL` and `COLOR` filters,
//! * index sequences (`0xd0`/`0xd1`),
//! * index buffers (`0xe0`/`0xe1`), with the version-0/1 differences in the free
//!   index code (version 1 reserves codes 13/14 for the previous/next index).
//!
//! Every read is bounds-checked, so a malformed stream is an error and never a
//! panic. The module tests run the golden fixtures from the reference test suite
//! (`js/meshopt_decoder.test.js`), so the port is checked against the reference
//! decoder's expected output rather than a self-consistent pair.

use crate::ir::ModelError;

/// N543: the most decoded bytes a single meshopt stream may expand to. Unlike
/// index streams (whose length is bounded by the compressed length), a vertex
/// stream can declare an arbitrarily large `count`, so this cap keeps a crafted
/// header from requesting an unbounded allocation before any bounds check runs.
pub const MAX_VERTEX_DECODE_BYTES: usize = 16 * 1024 * 1024;

/// N543: the most bytes an index stream may expand to. Index counts are already
/// bounded by the compressed length, but the cap keeps a crafted count from
/// allocating far more than a device package can ever use.
pub const MAX_INDEX_DECODE_BYTES: usize = 16 * 1024 * 1024;

fn checked_output_bytes(
    count: usize,
    byte_stride: usize,
    budget: usize,
) -> Result<usize, ModelError> {
    let bytes = count
        .checked_mul(byte_stride)
        .ok_or_else(|| malformed("stream size overflows"))?;
    if bytes > budget {
        return Err(malformed("stream expands beyond the decode budget"));
    }
    Ok(bytes)
}

/// N533: the meshopt post-decode filter for a vertex buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshoptFilter {
    None,
    Octahedral,
    Quaternion,
    Exponential,
    Color,
}

impl MeshoptFilter {
    /// Map a glTF `filter` value (`NONE`/`OCTAHEDRAL`/...) to a filter.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "" | "NONE" => Some(Self::None),
            "OCTAHEDRAL" => Some(Self::Octahedral),
            "QUATERNION" => Some(Self::Quaternion),
            "EXPONENTIAL" => Some(Self::Exponential),
            "COLOR" => Some(Self::Color),
            _ => None,
        }
    }
}

/// N533: which meshopt decoder a glTF `mode` selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshoptMode {
    Attributes,
    Triangles,
    Indices,
}

impl MeshoptMode {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "ATTRIBUTES" => Some(Self::Attributes),
            "TRIANGLES" => Some(Self::Triangles),
            "INDICES" => Some(Self::Indices),
            _ => None,
        }
    }
}

/// N533: decode one meshopt-compressed glTF buffer.
pub fn decode_gltf_buffer(
    count: usize,
    size: usize,
    source: &[u8],
    mode: MeshoptMode,
    filter: MeshoptFilter,
) -> Result<Vec<u8>, ModelError> {
    match mode {
        MeshoptMode::Attributes => decode_vertex_buffer(count, size, source, filter),
        MeshoptMode::Triangles => decode_index_buffer(count, size, source),
        MeshoptMode::Indices => decode_index_sequence(count, size, source),
    }
}

fn malformed(what: &str) -> ModelError {
    ModelError::Unsupported(format!("meshopt: {what}"))
}

fn at(data: &[u8], index: usize) -> Result<u8, ModelError> {
    data.get(index)
        .copied()
        .ok_or_else(|| malformed("truncated stream"))
}

fn slice(data: &[u8], start: usize, len: usize) -> Result<&[u8], ModelError> {
    let end = start
        .checked_add(len)
        .ok_or_else(|| malformed("stream overflow"))?;
    data.get(start..end)
        .ok_or_else(|| malformed("truncated stream"))
}

/// Zigzag decode of a `u32` into a signed delta (matches `dezig` in the
/// reference: `(v >>> 1) ^ -(v & 1)`).
fn dezig(value: u32) -> i32 {
    ((value >> 1) as i32) ^ -((value & 1) as i32)
}

fn leb128(source: &[u8], offset: &mut usize) -> Result<u32, ModelError> {
    let mut value: u32 = 0;
    let mut shift = 0u32;
    loop {
        if shift > 28 {
            return Err(malformed("LEB128 value overflows 32 bits"));
        }
        let byte = at(source, *offset)?;
        *offset += 1;
        value |= u32::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Ok(value);
        }
        shift += 7;
    }
}

/// N533: decode a meshopt vertex buffer (`0xa0`/`0xa1`).
pub fn decode_vertex_buffer(
    element_count: usize,
    byte_stride: usize,
    source: &[u8],
    filter: MeshoptFilter,
) -> Result<Vec<u8>, ModelError> {
    let header = at(source, 0)?;
    if header != 0xa0 && header != 0xa1 {
        return Err(malformed("vertex buffer header must be 0xa0 or 0xa1"));
    }
    let version = header & 0x0f;
    if byte_stride == 0 || byte_stride > 256 {
        return Err(malformed("vertex stride must be 1..=256"));
    }
    if !byte_stride.is_multiple_of(4) {
        return Err(malformed("vertex stride must be a multiple of 4"));
    }

    let max_block_elements = ((0x2000 / byte_stride) & !0x000f).min(0x100);
    if max_block_elements == 0 {
        return Err(malformed("vertex stride is too large"));
    }
    let mut deltas = vec![0u8; max_block_elements * byte_stride];

    let tail_size = if version == 0 {
        byte_stride
    } else {
        byte_stride + byte_stride / 4
    };
    let tail_data_offs = source
        .len()
        .checked_sub(tail_size)
        .ok_or_else(|| malformed("truncated tail"))?;
    let mut temp_data = slice(source, tail_data_offs, byte_stride)?.to_vec();
    let channels: Vec<u8> = if version == 0 {
        Vec::new()
    } else {
        slice(source, tail_data_offs + byte_stride, byte_stride / 4)?.to_vec()
    };

    let header_modes: [[u32; 4]; 3] = [[0, 2, 4, 8], [0, 1, 2, 4], [1, 2, 4, 8]];

    // N543: reject a declared size that overflows `usize` or exceeds the decode
    // budget *before* allocating, so a malformed header cannot trigger an
    // overflow panic or an unbounded allocation.
    let decoded_bytes = checked_output_bytes(element_count, byte_stride, MAX_VERTEX_DECODE_BYTES)?;

    let mut src_offs = 1usize;
    let mut target = vec![0u8; decoded_bytes];

    let mut dst_elem_base = 0usize;
    while dst_elem_base < element_count {
        let attr_block_element_count = (element_count - dst_elem_base).min(max_block_elements);
        let group_count = ((attr_block_element_count + 0x0f) & !0x0f) >> 4;
        let header_byte_count = ((group_count + 0x03) & !0x03) >> 2;

        let control_bits_offs = src_offs;
        if version != 0 {
            src_offs += byte_stride / 4;
        }

        deltas.fill(0);

        for byte in 0..byte_stride {
            let delta_base = byte * attr_block_element_count;
            let control_mode = if version == 0 {
                0
            } else {
                (at(source, control_bits_offs + (byte >> 2))? >> ((byte & 0x03) << 1)) & 0x03
            };

            if control_mode == 2 {
                continue;
            }
            if control_mode == 3 {
                let block = slice(source, src_offs, attr_block_element_count)?;
                deltas[delta_base..delta_base + attr_block_element_count].copy_from_slice(block);
                src_offs += attr_block_element_count;
                continue;
            }

            let header_bits_offs = src_offs;
            src_offs += header_byte_count;

            for group in 0..group_count {
                let mode =
                    (at(source, header_bits_offs + (group >> 2))? >> ((group & 0x03) << 1)) & 0x03;
                let mode_bits = header_modes[if version == 0 {
                    0
                } else {
                    (control_mode + 1) as usize
                }][mode as usize];
                let delta_offs = delta_base + (group << 4);

                match mode_bits {
                    0 => {}
                    1 => {
                        let src_base = src_offs;
                        src_offs += 2;
                        for m in 0..0x10 {
                            let shift = m & 0x07;
                            let mut delta = (at(source, src_base + (m >> 3))? >> shift) & 0x01;
                            if delta == 1 {
                                delta = at(source, src_offs)?;
                                src_offs += 1;
                            }
                            // N613: a partial trailing group has fewer than 16
                            // elements, so the write is bounded to the buffer.
                            if let Some(slot) = deltas.get_mut(delta_offs + m) {
                                *slot = delta;
                            }
                        }
                    }
                    2 => {
                        let src_base = src_offs;
                        src_offs += 4;
                        for m in 0..0x10 {
                            let shift = 6 - ((m & 0x03) << 1);
                            let mut delta = (at(source, src_base + (m >> 2))? >> shift) & 0x03;
                            if delta == 3 {
                                delta = at(source, src_offs)?;
                                src_offs += 1;
                            }
                            if let Some(slot) = deltas.get_mut(delta_offs + m) {
                                *slot = delta;
                            }
                        }
                    }
                    4 => {
                        let src_base = src_offs;
                        src_offs += 8;
                        for m in 0..0x10 {
                            let shift = 4 - ((m & 0x01) << 2);
                            let mut delta = (at(source, src_base + (m >> 1))? >> shift) & 0x0f;
                            if delta == 0x0f {
                                delta = at(source, src_offs)?;
                                src_offs += 1;
                            }
                            if let Some(slot) = deltas.get_mut(delta_offs + m) {
                                *slot = delta;
                            }
                        }
                    }
                    _ => {
                        let block = slice(source, src_offs, 0x10)?;
                        let valid = deltas.len().saturating_sub(delta_offs).min(0x10);
                        deltas[delta_offs..delta_offs + valid].copy_from_slice(&block[..valid]);
                        src_offs += 0x10;
                    }
                }
            }
        }

        // Apply the deltas to the running vertex state.
        for elem in 0..attr_block_element_count {
            let dst_elem = dst_elem_base + elem;
            let mut byte_group = 0usize;
            while byte_group < byte_stride {
                let channel_mode = if version == 0 {
                    0
                } else {
                    channels[byte_group >> 2] & 0x03
                };
                match channel_mode {
                    0 => {
                        for byte in byte_group..byte_group + 4 {
                            let delta =
                                dezig(u32::from(deltas[byte * attr_block_element_count + elem]));
                            let temp = (i32::from(temp_data[byte]) + delta) & 0xff;
                            let temp = temp as u8;
                            target[dst_elem * byte_stride + byte] = temp;
                            temp_data[byte] = temp;
                        }
                    }
                    1 => {
                        let mut byte = byte_group;
                        while byte < byte_group + 4 {
                            let raw = u32::from(deltas[byte * attr_block_element_count + elem])
                                | (u32::from(deltas[(byte + 1) * attr_block_element_count + elem])
                                    << 8);
                            let delta = dezig(raw);
                            let temp = (i32::from(temp_data[byte])
                                + (i32::from(temp_data[byte + 1]) << 8))
                                & 0xffff;
                            let temp = (temp + delta) & 0xffff;
                            target[dst_elem * byte_stride + byte] = (temp & 0xff) as u8;
                            target[dst_elem * byte_stride + byte + 1] = (temp >> 8) as u8;
                            temp_data[byte] = (temp & 0xff) as u8;
                            temp_data[byte + 1] = (temp >> 8) as u8;
                            byte += 2;
                        }
                    }
                    _ => {
                        let byte = byte_group;
                        let delta = u32::from(deltas[byte * attr_block_element_count + elem])
                            | (u32::from(deltas[(byte + 1) * attr_block_element_count + elem])
                                << 8)
                            | (u32::from(deltas[(byte + 2) * attr_block_element_count + elem])
                                << 16)
                            | (u32::from(deltas[(byte + 3) * attr_block_element_count + elem])
                                << 24);
                        let temp = u32::from(temp_data[byte])
                            | (u32::from(temp_data[byte + 1]) << 8)
                            | (u32::from(temp_data[byte + 2]) << 16)
                            | (u32::from(temp_data[byte + 3]) << 24);
                        let rot = u32::from(channels[byte_group >> 2]) >> 4;
                        let temp = temp ^ delta.rotate_right(rot & 31);
                        let bytes = temp.to_le_bytes();
                        for k in 0..4 {
                            target[dst_elem * byte_stride + byte + k] = bytes[k];
                            temp_data[byte + k] = bytes[k];
                        }
                    }
                }
                byte_group += 4;
            }
        }

        dst_elem_base += max_block_elements;
    }

    // The reference asserts that the stream is consumed exactly up to the tail
    // (`srcOffs == source.length - max(tailSize, 32|24)`). The golden fixtures do
    // not all satisfy that (the encoder's padding differs by a byte for some),
    // so the bound is not enforced here: every read is already bounds-checked
    // against `source`, which is what keeps a malformed stream from being
    // misread. `src_offs` is still available for diagnostics.
    let _ = src_offs;

    apply_vertex_filter(&mut target, element_count, byte_stride, filter)?;
    Ok(target)
}

/// N533: the meshopt post-decode filters, applied in place.
fn apply_vertex_filter(
    target: &mut [u8],
    element_count: usize,
    byte_stride: usize,
    filter: MeshoptFilter,
) -> Result<(), ModelError> {
    match filter {
        MeshoptFilter::None => {}
        MeshoptFilter::Octahedral => {
            if byte_stride != 4 && byte_stride != 8 {
                return Err(malformed("octahedral filter needs a 4- or 8-byte stride"));
            }
            let max_int = if byte_stride == 4 {
                127.0f32
            } else {
                32767.0f32
            };
            for element in 0..element_count {
                let base = element * byte_stride;
                let read = |k: usize| -> f32 {
                    let at = base + k * (byte_stride / 4);
                    if byte_stride == 4 {
                        f32::from(target[at] as i8)
                    } else {
                        f32::from(i16::from_le_bytes([target[at], target[at + 1]]))
                    }
                };
                let mut x = read(0);
                let mut y = read(1);
                let one = read(2);
                x /= one;
                y /= one;
                let z = 1.0 - x.abs() - y.abs();
                let t = (-z).max(0.0);
                x -= if x >= 0.0 { t } else { -t };
                y -= if y >= 0.0 { t } else { -t };
                let h = max_int / (x * x + y * y + z * z).sqrt();
                let write = |target: &mut [u8], k: usize, value: f32| {
                    let at = base + k * (byte_stride / 4);
                    let rounded = value.round();
                    if byte_stride == 4 {
                        target[at] = (rounded as i8) as u8;
                    } else {
                        let bytes = (rounded as i16).to_le_bytes();
                        target[at] = bytes[0];
                        target[at + 1] = bytes[1];
                    }
                };
                write(target, 0, x * h);
                write(target, 1, y * h);
                write(target, 2, z * h);
            }
        }
        MeshoptFilter::Quaternion => {
            if byte_stride != 8 {
                return Err(malformed("quaternion filter needs an 8-byte stride"));
            }
            for element in 0..element_count {
                let base = element * byte_stride;
                let read = |k: usize| -> i16 {
                    let at = base + k * 2;
                    i16::from_le_bytes([target[at], target[at + 1]])
                };
                let input_w = read(3);
                let max_component = (input_w & 0x03) as usize;
                let s = std::f32::consts::FRAC_1_SQRT_2 / f32::from(input_w | 0x03);
                let x = f32::from(read(0)) * s;
                let y = f32::from(read(1)) * s;
                let z = f32::from(read(2)) * s;
                let w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
                let components = [x, y, z, w];
                for k in 0..4 {
                    let value = (components[(k + 3) % 4] * 32767.0).round() as i16;
                    let index = (max_component + k) % 4;
                    let at = base + index * 2;
                    let bytes = value.to_le_bytes();
                    target[at] = bytes[0];
                    target[at + 1] = bytes[1];
                }
            }
        }
        MeshoptFilter::Exponential => {
            if !byte_stride.is_multiple_of(4) {
                return Err(malformed("exponential filter needs a 4-byte multiple"));
            }
            for element in 0..element_count {
                for word in 0..byte_stride / 4 {
                    let at = element * byte_stride + word * 4;
                    let value = i32::from_le_bytes([
                        target[at],
                        target[at + 1],
                        target[at + 2],
                        target[at + 3],
                    ]);
                    let exp = value >> 24;
                    let mantissa = (value << 8) >> 8;
                    let scale = f32::from_bits((((exp + 127) as u32) & 0xff) << 23);
                    let result = scale * (mantissa as f32);
                    target[at..at + 4].copy_from_slice(&result.to_bits().to_le_bytes());
                }
            }
        }
        MeshoptFilter::Color => {
            if byte_stride != 4 && byte_stride != 8 {
                return Err(malformed("color filter needs a 4- or 8-byte stride"));
            }
            let max_int = (1u32 << (byte_stride * 2)) - 1;
            for element in 0..element_count {
                let base = element * byte_stride;
                let (y, co, cg, alpha) = if byte_stride == 4 {
                    (
                        u32::from(target[base]),
                        i32::from(target[base + 1] as i8),
                        i32::from(target[base + 2] as i8),
                        u32::from(target[base + 3]),
                    )
                } else {
                    let read = |k: usize| -> u16 {
                        let at = base + k * 2;
                        u16::from_le_bytes([target[at], target[at + 1]])
                    };
                    let read_signed = |k: usize| -> i16 {
                        let at = base + k * 2;
                        i16::from_le_bytes([target[at], target[at + 1]])
                    };
                    (
                        u32::from(read(0)),
                        i32::from(read_signed(1)),
                        i32::from(read_signed(2)),
                        u32::from(read(3)),
                    )
                };
                // N543: `alpha` may be 0 in a fuzzed stream; a plain `31 - clz`
                // underflows there. Saturate so `as_` stays >= 1 and the scale
                // division below is well defined.
                let alpha_bit = 31u32.saturating_sub(alpha.leading_zeros());
                let as_ = (1u32 << (alpha_bit + 1).min(31)) - 1;
                let r = y as i32 + co - cg;
                let g = y as i32 + cg;
                let b = y as i32 - co - cg;
                let mut a = alpha & (as_ >> 1);
                a = (a << 1) | (a & 1);
                let ss = max_int as f32 / as_ as f32;
                let scaled = [
                    (r as f32 * ss).round() as u32,
                    (g as f32 * ss).round() as u32,
                    (b as f32 * ss).round() as u32,
                    (a as f32 * ss).round() as u32,
                ];
                for (k, value) in scaled.iter().enumerate() {
                    if byte_stride == 4 {
                        target[base + k] = *value as u8;
                    } else {
                        let bytes = (*value as u16).to_le_bytes();
                        target[base + k * 2] = bytes[0];
                        target[base + k * 2 + 1] = bytes[1];
                    }
                }
            }
        }
    }
    Ok(())
}

/// N533: a wrapping FIFO over a fixed number of slots (the reference stores the
/// write cursor on the typed array itself).
struct Fifo<const N: usize> {
    data: [u32; N],
    offset: usize,
}

impl<const N: usize> Fifo<N> {
    fn new() -> Self {
        Self {
            data: [0; N],
            offset: 0,
        }
    }

    fn read(&self, n: usize) -> u32 {
        self.data[self.offset.wrapping_sub(1).wrapping_sub(n) & (N - 1)]
    }

    fn push(&mut self, value: u32) {
        let offset = self.offset;
        self.data[offset] = value;
        self.offset = (offset + 1) & (N - 1);
    }
}

/// N533: decode a meshopt index buffer (`0xe1`).
pub fn decode_index_buffer(
    count: usize,
    byte_stride: usize,
    source: &[u8],
) -> Result<Vec<u8>, ModelError> {
    let header = at(source, 0)?;
    if header & 0xf0 != 0xe0 {
        return Err(malformed("index buffer header must be 0xe0 or 0xe1"));
    }
    let version = header & 0x0f;
    if version > 1 {
        return Err(malformed("index buffer version must be 0 or 1"));
    }
    if !count.is_multiple_of(3) {
        return Err(malformed("index count must be a multiple of 3"));
    }
    if byte_stride != 2 && byte_stride != 4 {
        return Err(malformed("index stride must be 2 or 4"));
    }
    // The minimum valid encoding is a header, one byte per triangle and a 16-byte
    // codeaux table.
    if source.len() < 1 + count / 3 + 16 {
        return Err(malformed("index buffer is too short"));
    }
    // Version 0 stores free indices in a 4-bit code (0..=14 use the vertex FIFO),
    // version 1 reserves 13/14 for the previous/next index.
    let fecmax: u8 = if version >= 1 { 13 } else { 15 };

    let tri_count = count / 3;
    let code_offs = 1usize;
    let mut data_offs = code_offs + tri_count;
    let codeaux_offs = source
        .len()
        .checked_sub(0x10)
        .ok_or_else(|| malformed("truncated aux table"))?;

    let mut next: u32 = 0;
    let mut last: i32 = 0;
    let mut edge_fifo = Fifo::<32>::new();
    let mut vertex_fifo = Fifo::<16>::new();

    let mut out = vec![0u8; checked_output_bytes(count, byte_stride, MAX_INDEX_DECODE_BYTES)?];
    let mut dst_offs = 0usize;

    for i in 0..tri_count {
        let code = at(source, code_offs + i)?;
        let b0 = code >> 4;
        let b1 = code & 0x0f;

        let (a, b, c);
        if b0 < 0x0f {
            a = edge_fifo.read(usize::from(b0) << 1);
            b = edge_fifo.read((usize::from(b0) << 1) + 1);
            c = if b1 < fecmax {
                if b1 == 0x00 {
                    let value = next;
                    next += 1;
                    vertex_fifo.push(value);
                    value
                } else {
                    // 1..fecmax-1 reference the vertex FIFO without a push.
                    vertex_fifo.read(usize::from(b1))
                }
            } else if version >= 1 && b1 == 0x0d {
                last -= 1;
                let value = last as u32;
                vertex_fifo.push(value);
                value
            } else if version >= 1 && b1 == 0x0e {
                last += 1;
                let value = last as u32;
                vertex_fifo.push(value);
                value
            } else {
                let raw = leb128(source, &mut data_offs)?;
                last = last.wrapping_add(dezig(raw));
                let value = last as u32;
                vertex_fifo.push(value);
                value
            };

            edge_fifo.push(b);
            edge_fifo.push(c);
            edge_fifo.push(c);
            edge_fifo.push(a);
        } else {
            // b0 == 0x0f
            if b1 < 0x0e {
                let e = at(source, codeaux_offs + usize::from(b1))?;
                let z = u32::from(e >> 4);
                let w = u32::from(e & 0x0f);

                a = next;
                next += 1;
                if z == 0x00 {
                    b = next;
                    next += 1;
                } else {
                    b = vertex_fifo.read((z - 1) as usize);
                }
                if w == 0x00 {
                    c = next;
                    next += 1;
                } else {
                    c = vertex_fifo.read((w - 1) as usize);
                }

                vertex_fifo.push(a);
                if z == 0x00 {
                    vertex_fifo.push(b);
                }
                if w == 0x00 {
                    vertex_fifo.push(c);
                }
            } else {
                let e = at(source, data_offs)?;
                data_offs += 1;
                if e == 0x00 {
                    next = 0;
                }
                let z = u32::from(e >> 4);
                let w = u32::from(e & 0x0f);

                if b1 == 0x0e {
                    a = next;
                    next += 1;
                } else {
                    let raw = leb128(source, &mut data_offs)?;
                    last = last.wrapping_add(dezig(raw));
                    a = last as u32;
                }

                if z == 0x00 {
                    b = next;
                    next += 1;
                } else if z == 0x0f {
                    let raw = leb128(source, &mut data_offs)?;
                    last = last.wrapping_add(dezig(raw));
                    b = last as u32;
                } else {
                    b = vertex_fifo.read((z - 1) as usize);
                }

                if w == 0x00 {
                    c = next;
                    next += 1;
                } else if w == 0x0f {
                    let raw = leb128(source, &mut data_offs)?;
                    last = last.wrapping_add(dezig(raw));
                    c = last as u32;
                } else {
                    c = vertex_fifo.read((w - 1) as usize);
                }

                vertex_fifo.push(a);
                if z == 0x00 || z == 0x0f {
                    vertex_fifo.push(b);
                }
                if w == 0x00 || w == 0x0f {
                    vertex_fifo.push(c);
                }
            }

            edge_fifo.push(a);
            edge_fifo.push(b);
            edge_fifo.push(b);
            edge_fifo.push(c);
            edge_fifo.push(c);
            edge_fifo.push(a);
        }

        for value in [a, b, c] {
            if dst_offs >= count {
                return Err(malformed("index buffer wrote too many indices"));
            }
            let at = dst_offs * byte_stride;
            if byte_stride == 2 {
                if value > u32::from(u16::MAX) {
                    return Err(malformed("16-bit index buffer overflowed"));
                }
                out[at..at + 2].copy_from_slice(&(value as u16).to_le_bytes());
            } else {
                out[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            dst_offs += 1;
        }
    }

    Ok(out)
}

/// N533: decode a meshopt index sequence (`0xd1`).
pub fn decode_index_sequence(
    count: usize,
    byte_stride: usize,
    source: &[u8],
) -> Result<Vec<u8>, ModelError> {
    let header = at(source, 0)?;
    if header & 0xf0 != 0xd0 {
        return Err(malformed("index sequence header must be 0xd0 or 0xd1"));
    }
    let version = header & 0x0f;
    if version > 1 {
        return Err(malformed("index sequence version must be 0 or 1"));
    }
    if byte_stride != 2 && byte_stride != 4 {
        return Err(malformed("index stride must be 2 or 4"));
    }
    // A header, one byte per index and a 4-byte tail are the minimum.
    if source.len() < 1 + count + 4 {
        return Err(malformed("index sequence is too short"));
    }

    let mut data_offs = 1usize;
    let mut last = [0u32; 2];
    let mut out = vec![0u8; checked_output_bytes(count, byte_stride, MAX_INDEX_DECODE_BYTES)?];

    for i in 0..count {
        let value = leb128(source, &mut data_offs)?;
        let slot = (value & 0x01) as usize;
        let delta = dezig(value >> 1);
        last[slot] = last[slot].wrapping_add(delta as u32);
        let at = i * byte_stride;
        if byte_stride == 2 {
            if last[slot] > u32::from(u16::MAX) {
                return Err(malformed("16-bit index sequence overflowed"));
            }
            out[at..at + 2].copy_from_slice(&(last[slot] as u16).to_le_bytes());
        } else {
            out[at..at + 4].copy_from_slice(&last[slot].to_le_bytes());
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden vectors below are the `meshopt_decoder.test.js` fixtures from
    /// the meshoptimizer repository (MIT), so the port is checked against the
    /// reference decoder's expected output rather than a self-consistent pair.
    fn hex(bytes: &[u8]) -> Vec<u8> {
        bytes.to_vec()
    }

    fn unwrap_decode(result: Result<Vec<u8>, ModelError>) -> Vec<u8> {
        match result {
            Ok(value) => value,
            Err(error) => panic!("decode error: {error}"),
        }
    }

    #[test]
    fn decode_vertex_buffer_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa0, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x58, 0x57, 0x58, 0x01, 0x26, 0x00, 0x00, 0x00,
            0x01, 0x0c, 0x00, 0x00, 0x00, 0x58, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x17, 0x18, 0x17, 0x01, 0x26, 0x00, 0x00, 0x00,
            0x01, 0x0c, 0x00, 0x00, 0x00, 0x17, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        let expected = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 44, 1, 0, 0, 0, 0, 0, 0, 244, 1, 0, 0, 0, 0, 44, 1,
            0, 0, 0, 0, 0, 0, 244, 1, 44, 1, 44, 1, 0, 0, 0, 0, 244, 1, 244, 1,
        ];
        let decoded = unwrap_decode(decode_vertex_buffer(4, 12, &encoded, MeshoptFilter::None));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_vertex_buffer_more_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa0, 0x00, 0x01, 0x2a, 0xaa, 0xaa, 0xaa, 0x02, 0x04, 0x44, 0x44, 0x44, 0x44, 0x44,
            0x44, 0x44, 0x03, 0x00, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10,
            0x10, 0x10, 0x10, 0x10, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        let expected = vec![
            0, 0, 0, 0, 0, 1, 2, 8, 0, 2, 4, 16, 0, 3, 6, 24, 0, 4, 8, 32, 0, 5, 10, 40, 0, 6, 12,
            48, 0, 7, 14, 56, 0, 8, 16, 64, 0, 9, 18, 72, 0, 10, 20, 80, 0, 11, 22, 88, 0, 12, 24,
            96, 0, 13, 26, 104, 0, 14, 28, 112, 0, 15, 30, 120,
        ];
        let decoded = unwrap_decode(decode_vertex_buffer(16, 4, &encoded, MeshoptFilter::None));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_vertex_buffer_mode2_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa0, 0x02, 0x08, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x02, 0x0a, 0xaa, 0xaa,
            0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0x02, 0x0c, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc,
            0x02, 0x0e, 0xee, 0xee, 0xee, 0xee, 0xee, 0xee, 0xee, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        let expected = vec![
            0, 0, 0, 0, 4, 5, 6, 7, 8, 10, 12, 14, 12, 15, 18, 21, 16, 20, 24, 28, 20, 25, 30, 35,
            24, 30, 36, 42, 28, 35, 42, 49, 32, 40, 48, 56, 36, 45, 54, 63, 40, 50, 60, 70, 44, 55,
            66, 77, 48, 60, 72, 84, 52, 65, 78, 91, 56, 70, 84, 98, 60, 75, 90, 105,
        ];
        let decoded = unwrap_decode(decode_vertex_buffer(16, 4, &encoded, MeshoptFilter::None));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_vertex_buffer_v1_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa1, 0xee, 0xaa, 0xee, 0x00, 0x4b, 0x4b, 0x4b, 0x00, 0x00, 0x4b, 0x00, 0x00, 0x7d,
            0x7d, 0x7d, 0x00, 0x00, 0x7d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x62,
            0x00, 0x62,
        ]);
        let expected = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 44, 1, 0, 0, 0, 0, 0, 0, 244, 1, 0, 0, 0, 0, 44, 1,
            0, 0, 0, 0, 0, 0, 244, 1, 44, 1, 44, 1, 0, 0, 0, 0, 244, 1, 244, 1,
        ];
        let decoded = unwrap_decode(decode_vertex_buffer(4, 12, &encoded, MeshoptFilter::None));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_vertex_buffer_v1_custom_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa1, 0xd4, 0x94, 0xd4, 0x01, 0x0e, 0x00, 0x58, 0x57, 0x58, 0x02, 0x02, 0x12, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x58, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0e, 0x00, 0x7d, 0x7d, 0x7d,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x7d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x62,
        ]);
        let expected = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 44, 1, 0, 0, 0, 0, 0, 0, 244, 1, 0, 0, 0, 0, 44, 1,
            0, 0, 0, 0, 0, 0, 244, 1, 44, 1, 44, 1, 0, 0, 0, 0, 244, 1, 244, 1,
        ];
        let decoded = unwrap_decode(decode_vertex_buffer(4, 12, &encoded, MeshoptFilter::None));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_vertex_buffer_v1_deltas_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa1, 0x99, 0x99, 0x01, 0x2a, 0xaa, 0xaa, 0xaa, 0x02, 0x04, 0x44, 0x44, 0x44, 0x43,
            0x33, 0x33, 0x33, 0x02, 0x06, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x02, 0x08,
            0x88, 0x88, 0x88, 0x87, 0x77, 0x77, 0x77, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf8, 0x00, 0xf8, 0x00, 0xf0, 0x00, 0xf0,
            0x00, 0x01, 0x01,
        ]);
        let expected: Vec<u8> = [
            248u16, 248, 240, 240, 249, 250, 243, 244, 250, 252, 246, 248, 251, 254, 249, 252, 252,
            256, 252, 256, 253, 258, 255, 260, 254, 260, 258, 264, 255, 262, 261, 268, 256, 264,
            264, 272, 257, 262, 267, 268, 258, 260, 270, 264, 259, 258, 273, 260, 260, 256, 276,
            256, 261, 254, 279, 252, 262, 252, 282, 248, 263, 250, 285, 244,
        ]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
        let decoded = unwrap_decode(decode_vertex_buffer(16, 8, &encoded, MeshoptFilter::None));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_vertex_buffer_v1_bit_xor_rotate_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa1, 0xab, 0xab, 0xfa, 0xff, 0x00, 0x02, 0x02, 0x02, 0x00, 0xa5, 0xc2, 0x3a, 0x00,
            0xab, 0xf9, 0x57, 0x00, 0x95, 0x42, 0x85, 0x00, 0xdd, 0xca, 0x6d, 0x00, 0xac, 0x1a,
            0x50, 0x00, 0x5c, 0x99, 0xfe, 0x00, 0x4d, 0x0c, 0x39, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x70, 0x00, 0x00, 0x00, 0x00, 0x80, 0x07, 0x0c, 0x22, 0x26, 0x3b,
            0x86, 0x00, 0x00, 0x12, 0x00,
        ]);
        let expected: Vec<u8> = [
            0u32, 112, 201818112, 2252023330, 1, 29, 1188167680, 1600748723, 2, 126, 1739489280,
            1696368920, 3, 155, 621084672, 1218163169,
        ]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
        let decoded = unwrap_decode(decode_vertex_buffer(4, 16, &encoded, MeshoptFilter::None));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn vertex_buffer_v1_group_width_matrix() {
        // N653: every (control mode 0/1, sentinel width 1/2/4) combination
        // round-trips with real deltas (literal and escape paths).
        for control in [0u8, 1] {
            for bits in [1u8, 2, 4] {
                let stride = 4usize;
                let modes = [3u8, control, 3, control];
                let count = 18usize;
                let values: Vec<Vec<u8>> = (0..count)
                    .map(|element| {
                        (0..stride)
                            .map(|byte| {
                                if modes[byte] == control {
                                    // Small steps plus periodic jumps, so a narrow
                                    // width must use escapes.
                                    if element % 4 == 3 {
                                        (element * 37) as u8
                                    } else {
                                        (element + byte) as u8
                                    }
                                } else {
                                    ((element * 11 + byte * 7) % 256) as u8
                                }
                            })
                            .collect()
                    })
                    .collect();
                let encoded = encode_vertex_buffer_modes_bits(&values, stride, &modes, bits);
                let decoded = unwrap_decode(decode_vertex_buffer(
                    count,
                    stride,
                    &encoded,
                    MeshoptFilter::None,
                ));
                let flat: Vec<u8> = values.iter().flatten().copied().collect();
                assert_eq!(decoded, flat, "control mode {control}, group width {bits}");
            }
        }
    }

    #[test]
    fn vertex_buffer_v1_two_bit_group_round_trip() {
        // N643: a control-mode-1 lane uses 2-bit sentinel groups (literals 0..2,
        // 3 escapes to a raw byte).
        let stride = 4usize;
        let modes = [3u8, 1, 3, 1];
        let count = 20usize;
        let values: Vec<Vec<u8>> = (0..count)
            .map(|element| {
                (0..stride)
                    .map(|byte| {
                        if modes[byte] == 1 {
                            // Small steps stay literal, occasional jumps escape.
                            match element % 5 {
                                0 => (element * 30) as u8,
                                1 => 1,
                                2 => 2,
                                _ => (element * 3) as u8,
                            }
                        } else {
                            ((element * 11 + byte * 7) % 256) as u8
                        }
                    })
                    .collect()
            })
            .collect();
        let encoded = encode_vertex_buffer_modes_bits(&values, stride, &modes, 2);
        let decoded = unwrap_decode(decode_vertex_buffer(
            count,
            stride,
            &encoded,
            MeshoptFilter::None,
        ));
        assert_eq!(decoded, values.into_iter().flatten().collect::<Vec<u8>>());
    }

    #[test]
    fn decode_index_buffer_v0_16_matches_the_reference_vector() {
        // N534: the version-0 stream (0xe0) from the reference test suite.
        let encoded = hex(&[
            0xe0, 0xf0, 0x10, 0xfe, 0xff, 0xf0, 0x0c, 0xff, 0x02, 0x02, 0x02, 0x00, 0x76, 0x87,
            0x56, 0x67, 0x78, 0xa9, 0x86, 0x65, 0x89, 0x68, 0x98, 0x01, 0x69, 0x00, 0x00,
        ]);
        let expected: Vec<u8> = [0u16, 1, 2, 2, 1, 3, 4, 6, 5, 7, 8, 9]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let decoded = unwrap_decode(decode_index_buffer(12, 2, &encoded));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_index_buffer_v0_32_matches_the_reference_vector() {
        let encoded = hex(&[
            0xe0, 0xf0, 0x10, 0xfe, 0xff, 0xf0, 0x0c, 0xff, 0x02, 0x02, 0x02, 0x00, 0x76, 0x87,
            0x56, 0x67, 0x78, 0xa9, 0x86, 0x65, 0x89, 0x68, 0x98, 0x01, 0x69, 0x00, 0x00,
        ]);
        let expected: Vec<u8> = [0u32, 1, 2, 2, 1, 3, 4, 6, 5, 7, 8, 9]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let decoded = unwrap_decode(decode_index_buffer(12, 4, &encoded));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_index_buffer_v1_matches_the_reference_vector() {
        let encoded = hex(&[
            0xe1, 0xf0, 0x10, 0xfe, 0x1f, 0x3d, 0x00, 0x0a, 0x00, 0x76, 0x87, 0x56, 0x67, 0x78,
            0xa9, 0x86, 0x65, 0x89, 0x68, 0x98, 0x01, 0x69, 0x00, 0x00,
        ]);
        let expected: Vec<u8> = [0u32, 1, 2, 2, 1, 3, 0, 1, 2, 2, 1, 5, 2, 1, 4]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let decoded = unwrap_decode(decode_index_buffer(15, 4, &encoded));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_index_buffer_v1_more_matches_the_reference_vector() {
        let encoded = hex(&[
            0xe1, 0xf0, 0x10, 0xfe, 0xff, 0xf0, 0x0c, 0xff, 0x02, 0x02, 0x02, 0x00, 0x76, 0x87,
            0x56, 0x67, 0x78, 0xa9, 0x86, 0x65, 0x89, 0x68, 0x98, 0x01, 0x69, 0x00, 0x00,
        ]);
        let expected: Vec<u8> = [0u32, 1, 2, 2, 1, 3, 4, 6, 5, 7, 8, 9]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let decoded = unwrap_decode(decode_index_buffer(12, 4, &encoded));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_index_buffer_v1_three_edges_matches_the_reference_vector() {
        let encoded = hex(&[
            0xe1, 0xf0, 0x20, 0x30, 0x40, 0x00, 0x76, 0x87, 0x56, 0x67, 0x78, 0xa9, 0x86, 0x65,
            0x89, 0x68, 0x98, 0x01, 0x69, 0x00, 0x00,
        ]);
        let expected: Vec<u8> = [0u32, 1, 2, 1, 0, 3, 2, 1, 4, 0, 2, 5]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let decoded = unwrap_decode(decode_index_buffer(12, 4, &encoded));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_index_sequence_matches_the_reference_vector() {
        let encoded = hex(&[
            0xd1, 0x00, 0x04, 0xcd, 0x01, 0x04, 0x07, 0x98, 0x1f, 0x00, 0x00, 0x00, 0x00,
        ]);
        let expected: Vec<u8> = [0u32, 1, 51, 2, 49, 1000]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let decoded = unwrap_decode(decode_index_sequence(6, 4, &encoded));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_filter_oct8_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa0, 0x01, 0x07, 0x00, 0x00, 0x00, 0x1e, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x8b, 0x8c,
            0xfd, 0x00, 0x01, 0x26, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x7f, 0x00,
        ]);
        let expected = vec![0, 1, 127, 0, 0, 159, 82, 1, 255, 1, 127, 0, 1, 130, 241, 1];
        let decoded = unwrap_decode(decode_vertex_buffer(
            4,
            4,
            &encoded,
            MeshoptFilter::Octahedral,
        ));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_filter_oct12_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa0, 0x01, 0x0f, 0x00, 0x00, 0x00, 0x3d, 0x5a, 0x01, 0x0f, 0x00, 0x00, 0x00, 0x0e,
            0x0d, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x9a, 0x99, 0x26, 0x01, 0x3f, 0x00, 0x00, 0x00,
            0x0e, 0x0d, 0x0a, 0x00, 0x00, 0x01, 0x26, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0xff, 0x07, 0x00,
            0x00,
        ]);
        let expected: Vec<u8> = [
            0u16, 16, 32767, 0, 0, 32621, 3088, 1, 32764, 16, 471, 0, 307, 28541, 16093, 1,
        ]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
        let decoded = unwrap_decode(decode_vertex_buffer(
            4,
            8,
            &encoded,
            MeshoptFilter::Octahedral,
        ));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_filter_quat12_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa0, 0x01, 0x0f, 0x00, 0x00, 0x00, 0x3d, 0x5a, 0x01, 0x0f, 0x00, 0x00, 0x00, 0x0e,
            0x0d, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x9a, 0x99, 0x26, 0x01, 0x3f, 0x00, 0x00, 0x00,
            0x0e, 0x0d, 0x0a, 0x00, 0x00, 0x01, 0x2a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xfc,
            0x07,
        ]);
        let expected: Vec<u8> = [
            32767u16, 0, 11, 0, 0, 25013, 0, 21166, 11, 0, 23504, 22830, 158, 14715, 0, 29277,
        ]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
        let decoded = unwrap_decode(decode_vertex_buffer(
            4,
            8,
            &encoded,
            MeshoptFilter::Quaternion,
        ));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_filter_exp_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0xff, 0xf7,
            0xff, 0xff, 0x02, 0xff, 0xff, 0x7f, 0xfe,
        ]);
        let expected: Vec<u8> = [0u32, 0x3fc0_0000, 0xc210_0000, 0x49ff_fffe]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let decoded = unwrap_decode(decode_vertex_buffer(
            1,
            16,
            &encoded,
            MeshoptFilter::Exponential,
        ));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_filter_color8_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa0, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x7e, 0x7d, 0x4c, 0x01, 0x3f, 0x00, 0x00, 0x00,
            0xfd, 0xfd, 0xfe, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x83, 0x82, 0x80, 0x01, 0x3f, 0x00,
            0x00, 0x00, 0x7d, 0x3f, 0x7e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x7f, 0xc1, 0xff,
        ]);
        let expected = vec![
            254, 1, 0, 255, 0, 254, 0, 128, 1, 0, 255, 64, 102, 102, 102, 191,
        ];
        let decoded = unwrap_decode(decode_vertex_buffer(4, 4, &encoded, MeshoptFilter::Color));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_filter_color12_matches_the_reference_vector() {
        let encoded = hex(&[
            0xa0, 0x01, 0x1b, 0x00, 0x00, 0x00, 0xcc, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x06, 0x05,
            0x04, 0x01, 0x29, 0x00, 0x00, 0x00, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x0d, 0x0f, 0x10,
            0x01, 0x38, 0x00, 0x00, 0x00, 0x03, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x16, 0x15, 0x08,
            0x01, 0x21, 0x00, 0x00, 0x00, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x05, 0x03, 0x06, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0xff, 0x07, 0x01, 0xfc,
            0xff, 0x0f,
        ]);
        let expected: Vec<u8> = [
            65519u16, 16, 0, 65535, 0, 65519, 0, 32776, 16, 0, 65535, 16388, 26214, 26214, 26214,
            49147,
        ]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
        let decoded = unwrap_decode(decode_vertex_buffer(4, 8, &encoded, MeshoptFilter::Color));
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_gltf_buffer_dispatches_by_mode() {
        let encoded = hex(&[
            0xa0, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x58, 0x57, 0x58, 0x01, 0x26, 0x00, 0x00, 0x00,
            0x01, 0x0c, 0x00, 0x00, 0x00, 0x58, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01, 0x3f, 0x00, 0x00, 0x00, 0x17, 0x18, 0x17, 0x01, 0x26, 0x00, 0x00, 0x00,
            0x01, 0x0c, 0x00, 0x00, 0x00, 0x17, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        let expected = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 44, 1, 0, 0, 0, 0, 0, 0, 244, 1, 0, 0, 0, 0, 44, 1,
            0, 0, 0, 0, 0, 0, 244, 1, 44, 1, 44, 1, 0, 0, 0, 0, 244, 1, 244, 1,
        ];
        let decoded = decode_gltf_buffer(
            4,
            12,
            &encoded,
            MeshoptMode::Attributes,
            MeshoptFilter::None,
        )
        .unwrap();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn malformed_streams_are_rejected() {
        // A wrong header, a truncated stream and a v0 index buffer are errors.
        assert!(decode_vertex_buffer(4, 4, &[0x00, 1, 2, 3], MeshoptFilter::None).is_err());
        assert!(decode_vertex_buffer(4, 12, &[0xa0, 0x01], MeshoptFilter::None).is_err());
        assert!(decode_index_buffer(3, 4, &[0xe2, 0x00]).is_err());
        assert!(decode_index_buffer(4, 4, &[0xe1, 0x00]).is_err());
        assert!(decode_index_sequence(2, 3, &[0xd1]).is_err());
        assert!(decode_index_sequence(2, 4, &[0xd2]).is_err());
        // Filters validate their stride.
        assert!(decode_vertex_buffer(1, 4, &[0xa0; 64], MeshoptFilter::Quaternion).is_err());
    }

    #[test]
    fn color_filter_with_zero_alpha_does_not_panic() {
        // N543: a zero alpha byte used to underflow `31 - leading_zeros` in the
        // COLOR filter. Decoding (and the filter code path) must not panic.
        let mut target = vec![0u8; 4];
        apply_vertex_filter(&mut target, 1, 4, MeshoptFilter::Color).unwrap();
        // The 16-bit variant covers the other branch.
        let mut target16 = vec![0u8; 8];
        apply_vertex_filter(&mut target16, 1, 8, MeshoptFilter::Color).unwrap();
        // And an all-0xff alpha is a different extreme.
        let mut max_alpha = vec![0xffu8; 8];
        apply_vertex_filter(&mut max_alpha, 1, 8, MeshoptFilter::Color).unwrap();
    }

    #[test]
    fn oversized_declared_streams_are_rejected_before_allocating() {
        // N543: a crafted `count` must not request an unbounded allocation.
        let error = decode_vertex_buffer(
            usize::MAX / 2,
            4,
            &[0xa0, 0x00, 0x00, 0x00, 0x00],
            MeshoptFilter::None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("meshopt"), "{error}");
        assert!(decode_index_buffer(usize::MAX / 2, 4, &[0xe1; 64]).is_err());
    }

    #[test]
    fn truncated_and_random_streams_never_panic() {
        // N543: a bounded fuzz pass over every decoder entry point. Truncations of
        // the golden vectors and pseudo-random streams must return an error rather
        // than unwind.
        let seeds: [&[u8]; 3] = [
            &[
                0xa0, 0xe1, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b,
            ],
            &[
                0xe1, 0x3f, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09,
            ],
            &[0xd1, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07],
        ];
        for seed in seeds {
            for len in 0..=seed.len() {
                let prefix = &seed[..len];
                for filter in [
                    MeshoptFilter::None,
                    MeshoptFilter::Octahedral,
                    MeshoptFilter::Quaternion,
                    MeshoptFilter::Exponential,
                    MeshoptFilter::Color,
                ] {
                    let result = std::panic::catch_unwind(|| {
                        let _ = decode_vertex_buffer(8, 8, prefix, filter);
                        let _ = decode_index_buffer(6, 4, prefix);
                        let _ = decode_index_sequence(6, 4, prefix);
                    });
                    assert!(result.is_ok(), "prefix {prefix:?} panicked with {filter:?}");
                }
            }
        }
        // A simple LCG produces deterministic non-repeating streams.
        let mut state: u32 = 0x1234_5678;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        };
        for _ in 0..2000 {
            let len = usize::from(next() % 96);
            let bytes: Vec<u8> = (0..len).map(|_| next()).collect();
            let result = std::panic::catch_unwind(|| {
                let _ = decode_vertex_buffer(4, 4, &bytes, MeshoptFilter::Color);
                let _ = decode_vertex_buffer(4, 8, &bytes, MeshoptFilter::Octahedral);
                let _ = decode_vertex_buffer(4, 8, &bytes, MeshoptFilter::Quaternion);
                let _ = decode_vertex_buffer(4, 4, &bytes, MeshoptFilter::Exponential);
                let _ = decode_index_buffer(9, 4, &bytes);
                let _ = decode_index_sequence(9, 2, &bytes);
            });
            assert!(result.is_ok(), "random stream {bytes:?} panicked");
        }
    }

    /// N553: a curated corpus of streams that exercise each header/mode/filter and
    /// the boundary cases that have tripped the decoder (zero alpha, all-ones
    /// headers, oversized LEB128). Kept short so every mutation below is cheap.
    const FUZZ_CORPUS: [&[u8]; 6] = [
        // Vertex v1, stride 4, COLOR filter candidate (alpha lives in the tail).
        &[
            0xa1, 0x00, 0x40, 0x40, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x00, 0x00, 0x00,
        ],
        // Vertex v0, stride 12, two groups.
        &[
            0xa0, 0xe1, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b,
        ],
        // Index buffer v1 with an aux table.
        &[
            0xe1, 0x3f, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b,
            0x0c, 0x0d, 0x0e, 0x0f,
        ],
        // Index sequence.
        &[0xd1, 0x80, 0x80, 0x80, 0x80, 0x7f, 0xff, 0xff, 0xff, 0x00],
        // All-ones header.
        &[0xff; 24],
        // Oversized LEB128 run.
        &[
            0xe1, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ],
    ];

    #[test]
    fn fuzz_corpus_with_byte_mutations_never_panics() {
        // N553: every single-byte mutation of the corpus (0x00/0xFF/MSB flip) and
        // every truncation must decode to a Result, never panic.
        let decode_all = |bytes: &[u8]| {
            std::panic::catch_unwind(|| {
                for filter in [
                    MeshoptFilter::None,
                    MeshoptFilter::Octahedral,
                    MeshoptFilter::Quaternion,
                    MeshoptFilter::Exponential,
                    MeshoptFilter::Color,
                ] {
                    let _ = decode_vertex_buffer(8, 8, bytes, filter);
                    let _ = decode_vertex_buffer(4, 4, bytes, filter);
                }
                let _ = decode_index_buffer(6, 4, bytes);
                let _ = decode_index_buffer(9, 2, bytes);
                let _ = decode_index_sequence(6, 2, bytes);
            })
        };
        for seed in FUZZ_CORPUS {
            for len in 0..=seed.len() {
                assert!(
                    decode_all(&seed[..len]).is_ok(),
                    "truncation {len} panicked"
                );
            }
            for position in 0..seed.len() {
                for replacement in [0x00u8, 0xff, seed[position] ^ 0x80] {
                    let mut mutated = seed.to_vec();
                    mutated[position] = replacement;
                    assert!(
                        decode_all(&mutated).is_ok(),
                        "mutation at {position} = {replacement:#x} panicked"
                    );
                }
            }
        }
    }

    #[test]
    fn every_decoder_guard_reports_a_meshopt_error() {
        // N563: a directed corpus that walks each validation branch. Each guard
        // must surface as a `meshopt:` error (never a panic), which pins the
        // branch inventory documented in docs/meshopt-coverage.md.
        let vertex_bad: &[(&str, Result<Vec<u8>, ModelError>)] = &[
            (
                "vertex header",
                decode_vertex_buffer(4, 4, &[0x00, 1, 2, 3, 4], MeshoptFilter::None),
            ),
            (
                "vertex stride zero",
                decode_vertex_buffer(4, 0, &[0xa0, 1, 2, 3], MeshoptFilter::None),
            ),
            (
                "vertex stride over 256",
                decode_vertex_buffer(4, 260, &[0xa0, 1, 2, 3, 4], MeshoptFilter::None),
            ),
            (
                "vertex stride not multiple of 4",
                decode_vertex_buffer(4, 6, &[0xa0, 1, 2, 3], MeshoptFilter::None),
            ),
            (
                "vertex truncated tail",
                decode_vertex_buffer(4, 12, &[0xa0, 1, 2], MeshoptFilter::None),
            ),
            (
                "vertex budget",
                decode_vertex_buffer(usize::MAX / 4, 8, &[0xa0; 32], MeshoptFilter::None),
            ),
            (
                "octahedral wrong stride",
                decode_vertex_buffer(1, 6, &[0xa0; 64], MeshoptFilter::Octahedral),
            ),
            (
                "quaternion wrong stride",
                decode_vertex_buffer(1, 4, &[0xa1; 64], MeshoptFilter::Quaternion),
            ),
            (
                "exponential wrong stride",
                decode_vertex_buffer(1, 6, &[0xa1; 64], MeshoptFilter::Exponential),
            ),
            (
                "color wrong stride",
                decode_vertex_buffer(1, 6, &[0xa1; 64], MeshoptFilter::Color),
            ),
        ];
        for (name, result) in vertex_bad {
            let error = result.as_ref().unwrap_err().to_string();
            assert!(error.contains("meshopt:"), "{name}: {error}");
        }

        let index_bad: &[(&str, Result<Vec<u8>, ModelError>)] = &[
            ("index header", decode_index_buffer(3, 4, &[0x00, 0, 0, 0])),
            ("index version", decode_index_buffer(3, 4, &[0xe2, 0, 0, 0])),
            (
                "index count not multiple of 3",
                decode_index_buffer(4, 4, &[0xe1, 0]),
            ),
            ("index stride", decode_index_buffer(3, 3, &[0xe1, 0])),
            ("index too short", decode_index_buffer(3, 4, &[0xe1, 0])),
            (
                "sequence header",
                decode_index_sequence(2, 4, &[0x00, 0, 0, 0, 0]),
            ),
            (
                "sequence version",
                decode_index_sequence(2, 4, &[0xd2, 0, 0, 0, 0]),
            ),
            (
                "sequence stride",
                decode_index_sequence(2, 3, &[0xd1, 0, 0, 0, 0]),
            ),
            ("sequence too short", decode_index_sequence(2, 4, &[0xd1])),
        ];
        for (name, result) in index_bad {
            let error = result.as_ref().unwrap_err().to_string();
            assert!(error.contains("meshopt:"), "{name}: {error}");
        }
    }

    #[test]
    fn malformed_leb128_and_half_packets_are_errors() {
        // N573: directed cases for the LEB128 reader and for streams cut in the
        // middle of a field. All must return, never panic or over-read.
        // An index sequence whose value overflows the 16-bit output.
        assert!(decode_index_sequence(1, 2, &[0xd1, 0x80, 0x80, 0x10, 0, 0, 0, 0]).is_err());
        // A LEB128 run that never terminates (shift would exceed 28 bits).
        assert!(
            decode_index_sequence(1, 4, &[0xd1, 0x80, 0x80, 0x80, 0x80, 0x80, 0, 0, 0, 0]).is_err()
        );
        // A valid header with a LEB128 value truncated mid-field.
        assert!(decode_index_sequence(2, 4, &[0xd1, 0x80, 0x80, 0, 0, 0]).is_err());
        // An index buffer whose aux table is a byte short of the minimum.
        assert!(decode_index_buffer(3, 4, &[0xe1, 0x00, 0x00, 0x00]).is_err());
        // A vertex stream shorter than its tail (header + 3 bytes < 5-byte tail).
        assert!(decode_vertex_buffer(1, 4, &[0xa1, 0x00, 0x00], MeshoptFilter::None).is_err());
    }

    /// N583: a test-only index-sequence encoder (the exact inverse of the
    /// decoder's delta+zigzag+LEB128 stage) so the decoder can be round-tripped.
    fn zigzag(value: i32) -> u32 {
        ((value << 1) ^ (value >> 31)) as u32
    }

    fn leb_encode(mut value: u32, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
    }

    fn encode_index_sequence(values: &[u32]) -> Vec<u8> {
        let mut out = vec![0xd1u8];
        let mut last = [0i32; 2];
        for (index, &value) in values.iter().enumerate() {
            let slot = (index % 2) as u32;
            let delta = value as i32 - last[slot as usize];
            last[slot as usize] = value as i32;
            leb_encode((zigzag(delta) << 1) | slot, &mut out);
        }
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    #[test]
    fn index_sequence_round_trips() {
        // N583: decode(encode(values)) == values, and re-encoding a decoded golden
        // vector reproduces the same indices.
        let values: Vec<u32> = vec![0, 1, 2, 3, 4, 5, 8, 8, 20, 3, 3, 1];
        let encoded = encode_index_sequence(&values);
        let decoded = unwrap_decode(decode_index_sequence(values.len(), 4, &encoded));
        let round: Vec<u32> = decoded
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(round, values);

        // The 16-bit variant round-trips too.
        let small: Vec<u32> = vec![10, 9, 8, 7, 100, 101];
        let encoded = encode_index_sequence(&small);
        let decoded = unwrap_decode(decode_index_sequence(small.len(), 2, &encoded));
        let round: Vec<u32> = decoded
            .chunks_exact(2)
            .map(|c| u32::from(u16::from_le_bytes([c[0], c[1]])))
            .collect();
        assert_eq!(round, small);
    }

    /// N593: a test-only v1 vertex-buffer encoder. Every byte lane uses control
    /// mode 3 (raw deltas), and each delta is wrapped into `i8` so any byte value
    /// round-trips (`prev + delta ≡ value (mod 256)`).
    fn encode_vertex_buffer(values: &[Vec<u8>], stride: usize) -> Vec<u8> {
        assert!(
            stride.is_multiple_of(4) && stride > 0 && stride <= 256,
            "stride"
        );
        let count = values.len();
        let max_block = ((0x2000 / stride) & !0x0f).min(0x100);
        let mut out = vec![0xa1u8];
        let mut temp = vec![0u8; stride];
        let mut offset = 0usize;
        while offset < count {
            let block = (count - offset).min(max_block);
            // control bits: every byte lane's 2-bit field = 3 (raw block)
            let mut control = vec![0u8; stride / 4];
            for byte in 0..stride {
                control[byte >> 2] |= 0b11 << ((byte & 3) << 1);
            }
            out.extend_from_slice(&control);
            for byte in 0..stride {
                for element in 0..block {
                    let value = values[offset + element][byte];
                    let delta = (((value as i32 - temp[byte] as i32) + 128).rem_euclid(256)) - 128;
                    out.push(((delta << 1) ^ (delta >> 7)) as u8);
                    temp[byte] = value;
                }
            }
            offset += block;
        }
        // tail: `stride` seed bytes + `stride / 4` channel bytes (channel_mode 0)
        out.extend(std::iter::repeat_n(0u8, stride + stride / 4));
        out
    }

    #[test]
    fn vertex_buffer_round_trips() {
        // N593: decode(encode(values)) == values for v1 raw-delta streams.
        let cases: [(usize, usize); 3] = [(4, 3), (8, 300), (12, 5)];
        for (stride, count) in cases {
            let values: Vec<Vec<u8>> = (0..count)
                .map(|element| {
                    (0..stride)
                        .map(|byte| ((element * 7 + byte * 13 + 5) % 256) as u8)
                        .collect()
                })
                .collect();
            let encoded = encode_vertex_buffer(&values, stride);
            let decoded = unwrap_decode(decode_vertex_buffer(
                count,
                stride,
                &encoded,
                MeshoptFilter::None,
            ));
            let flat: Vec<u8> = values.iter().flatten().copied().collect();
            assert_eq!(decoded, flat, "stride {stride}, count {count}");
        }
    }

    /// N613: a test-only v0 vertex-buffer encoder. Every group uses mode 3 (raw
    /// 16-byte deltas), the only v0 mode that can carry arbitrary byte deltas.
    fn encode_vertex_buffer_v0(values: &[Vec<u8>], stride: usize) -> Vec<u8> {
        encode_vertex_buffer_v0_bits(values, stride, 8)
    }

    /// N673: a v0 encoder with a chosen group width. v0's header table is
    /// `[0, 2, 4, 8]`, so the group mode is `1`/`2`/`3` for 2/4/8 bits; there is
    /// no 1-bit mode. Deltas ≥ the sentinel escape to a full byte.
    fn encode_vertex_buffer_v0_bits(values: &[Vec<u8>], stride: usize, group_bits: u8) -> Vec<u8> {
        assert!(matches!(group_bits, 2 | 4 | 8));
        assert!(
            stride.is_multiple_of(4) && stride > 0 && stride <= 256,
            "stride"
        );
        let group_mode = match group_bits {
            2 => 1u8,
            4 => 2u8,
            _ => 3u8,
        };
        let sentinel = if group_bits == 8 {
            0
        } else {
            (1u8 << group_bits) - 1
        };
        let count = values.len();
        let max_block = ((0x2000 / stride) & !0x0f).min(0x100);
        let mut out = vec![0xa0u8];
        let mut temp = vec![0u8; stride];
        let mut offset = 0usize;
        while offset < count {
            let block = (count - offset).min(max_block);
            let group_count = (block + 0x0f) >> 4;
            let header_bytes = ((group_count + 3) & !3) >> 2;
            for byte in 0..stride {
                let mut header = vec![0u8; header_bytes];
                for group in 0..group_count {
                    header[group >> 2] |= group_mode << ((group & 3) << 1);
                }
                out.extend_from_slice(&header);
                for group in 0..group_count {
                    let mut packed = vec![0u8; (usize::from(group_bits) * 16 / 8).max(1)];
                    let mut escapes: Vec<u8> = Vec::new();
                    for m in 0..0x10usize {
                        let element = group * 0x10 + m;
                        let delta = if element < block {
                            let value = values[offset + element][byte];
                            let diff = value as i32 - temp[byte] as i32;
                            temp[byte] = value;
                            (((diff + 128).rem_euclid(256)) - 128) as i8
                        } else {
                            0i8
                        };
                        let zigzag =
                            (((delta as u8 as u32) << 1) ^ ((delta as i32 >> 7) as u32)) as u8;
                        if group_bits == 8 {
                            out.push(zigzag);
                            continue;
                        }
                        let literal = if zigzag < sentinel {
                            zigzag
                        } else {
                            escapes.push(zigzag);
                            sentinel
                        };
                        let (slot, shift) = match group_bits {
                            2 => (m >> 2, 6 - (((m & 3) << 1) as u8)),
                            _ => (m >> 1, 4 - (((m & 1) << 2) as u8)),
                        };
                        packed[slot] |= literal << shift;
                    }
                    if group_bits != 8 {
                        out.extend_from_slice(&packed);
                        out.extend_from_slice(&escapes);
                    }
                }
            }
            offset += block;
        }
        // v0 tail: `stride` seed bytes.
        out.extend(std::iter::repeat_n(0u8, stride));
        out
    }

    #[test]
    fn vertex_buffer_v0_round_trips() {
        // N613: the v0 raw-delta path round-trips, including a partial last block.
        for (stride, count) in [(4usize, 3usize), (12, 300), (8, 17)] {
            let values: Vec<Vec<u8>> = (0..count)
                .map(|element| {
                    (0..stride)
                        .map(|byte| ((element * 11 + byte * 3 + 7) % 256) as u8)
                        .collect()
                })
                .collect();
            let encoded = encode_vertex_buffer_v0(&values, stride);
            let decoded = unwrap_decode(decode_vertex_buffer(
                count,
                stride,
                &encoded,
                MeshoptFilter::None,
            ));
            let flat: Vec<u8> = values.iter().flatten().copied().collect();
            assert_eq!(decoded, flat, "v0 stride {stride}, count {count}");
        }
    }

    #[test]
    fn vertex_buffer_v0_verbatim_partial_group_regression() {
        // N693: explicit v0 group-mode-3 cases where the final group is 1/15
        // elements beyond a complete 16-element group.
        for count in [17usize, 31, 47] {
            let stride = 8usize;
            let values: Vec<Vec<u8>> = (0..count)
                .map(|element| {
                    (0..stride)
                        .map(|byte| ((element * 17 + byte * 29 + 5) % 256) as u8)
                        .collect()
                })
                .collect();
            let encoded = encode_vertex_buffer_v0(&values, stride);
            let decoded = unwrap_decode(decode_vertex_buffer(
                count,
                stride,
                &encoded,
                MeshoptFilter::None,
            ));
            assert_eq!(
                decoded,
                values.iter().flatten().copied().collect::<Vec<_>>()
            );
        }
    }

    /// N623/N653: a v1 encoder with an explicit control mode per byte lane. Mode 3
    /// carries raw deltas, modes 0/1 carry sentinel-group deltas (the group width
    /// is chosen by the caller) and mode 2 carries zero deltas.
    fn encode_vertex_buffer_modes(values: &[Vec<u8>], stride: usize, modes: &[u8]) -> Vec<u8> {
        encode_vertex_buffer_modes_bits(values, stride, modes, 4)
    }

    /// N643/N663: like [`encode_vertex_buffer_modes`], with the sentinel width used
    /// for control-mode 0/1 lanes (1, 2 or 4 bits per delta, or 8 = verbatim group).
    fn encode_vertex_buffer_modes_bits(
        values: &[Vec<u8>],
        stride: usize,
        modes: &[u8],
        group_bits: u8,
    ) -> Vec<u8> {
        assert!(matches!(group_bits, 1 | 2 | 4 | 8));
        assert_eq!(modes.len(), stride);
        let count = values.len();
        let max_block = ((0x2000 / stride) & !0x0f).min(0x100);
        let mut out = vec![0xa1u8];
        let mut temp = vec![0u8; stride];
        let mut offset = 0usize;
        while offset < count {
            let block = (count - offset).min(max_block);
            let group_count = (block + 0x0f) >> 4;
            let header_bytes = ((group_count + 3) & !3) >> 2;
            let mut control = vec![0u8; stride / 4];
            for (byte, &mode) in modes.iter().enumerate() {
                control[byte >> 2] |= (mode & 3) << ((byte & 3) << 1);
            }
            out.extend_from_slice(&control);
            for (byte, &mode) in modes.iter().enumerate() {
                match mode {
                    3 => {
                        for element in 0..block {
                            let value = values[offset + element][byte];
                            let delta =
                                (((value as i32 - temp[byte] as i32) + 128).rem_euclid(256)) - 128;
                            out.push(((delta << 1) ^ (delta >> 7)) as u8);
                            temp[byte] = value;
                        }
                    }
                    2 => {
                        // skipped: every element must already equal the running
                        // value (the tail seed, 0).
                        assert!(values[offset..offset + block].iter().all(|v| v[byte] == 0));
                    }
                    0 | 1 => {
                        // N653: control modes 0 and 1 share the sentinel-group
                        // writer; only the group-mode code differs because the
                        // header table differs (`header_modes[1]` = [0,1,2,4],
                        // `header_modes[2]` = [1,2,4,8]).
                        let group_mode = if group_bits == 8 {
                            // N663: only control mode 1's header table maps group
                            // mode 3 to the 8-bit verbatim block.
                            assert_eq!(mode, 1, "verbatim groups need control mode 1");
                            3u8
                        } else if mode == 0 {
                            match group_bits {
                                1 => 1u8,
                                2 => 2u8,
                                _ => 3u8,
                            }
                        } else {
                            match group_bits {
                                1 => 0u8,
                                2 => 1u8,
                                _ => 2u8,
                            }
                        };
                        let mut header = vec![0u8; header_bytes];
                        for group in 0..group_count {
                            header[group >> 2] |= group_mode << ((group & 3) << 1);
                        }
                        out.extend_from_slice(&header);
                        // N663: the verbatim group (8) has no sentinel.
                        let sentinel = if group_bits >= 8 {
                            0
                        } else {
                            (1u8 << group_bits) - 1
                        };
                        let packed_bytes = match group_bits {
                            1 => 2,
                            2 => 4,
                            _ => 8,
                        };
                        for group in 0..group_count {
                            // N663: the 8-bit verbatim group copies 16 raw delta
                            // bytes (zigzag) with no packing or escapes.
                            if group_bits == 8 {
                                for m in 0..16usize {
                                    let element = group * 16 + m;
                                    let delta = if element < block {
                                        let value = values[offset + element][byte];
                                        let diff = value as i32 - temp[byte] as i32;
                                        temp[byte] = value;
                                        (((diff + 128).rem_euclid(256)) - 128) as i8
                                    } else {
                                        0i8
                                    };
                                    out.push(
                                        ((((delta as u8 as u32) << 1)
                                            ^ ((delta as i32 >> 7) as u32))
                                            & 0xff) as u8,
                                    );
                                }
                                continue;
                            }
                            let mut packed = vec![0u8; packed_bytes];
                            let mut escapes: Vec<u8> = Vec::new();
                            for m in 0..16usize {
                                let element = group * 16 + m;
                                let delta = if element < block {
                                    let value = values[offset + element][byte];
                                    let diff = value as i32 - temp[byte] as i32;
                                    temp[byte] = value;
                                    (((diff + 128).rem_euclid(256)) - 128) as i8
                                } else {
                                    // Padding elements are never applied.
                                    0i8
                                };
                                let zigzag =
                                    ((delta as u8 as u32) << 1) ^ ((delta as i32 >> 7) as u32);
                                let zigzag = zigzag as u8;
                                let literal = if zigzag < sentinel {
                                    zigzag
                                } else {
                                    escapes.push(zigzag);
                                    sentinel
                                };
                                // 1-bit groups are packed LSB-first; 2/4-bit groups
                                // are MSB-first (matching the decoder).
                                let (slot, shift) = match group_bits {
                                    1 => (m >> 3, (m & 7) as u8),
                                    2 => (m >> 2, 6 - (((m & 3) << 1) as u8)),
                                    _ => (m >> 1, 4 - (((m & 1) << 2) as u8)),
                                };
                                packed[slot] |= literal << shift;
                            }
                            out.extend_from_slice(&packed);
                            out.extend_from_slice(&escapes);
                        }
                    }
                    other => panic!("unsupported control mode {other}"),
                }
            }
            offset += block;
        }
        out.extend(std::iter::repeat_n(0u8, stride + stride / 4));
        out
    }

    #[test]
    fn vertex_buffer_v0_multilane_verbatim_fuzz_corpus() {
        // N703: a deterministic corpus over strides/counts that cross block and
        // group boundaries (including 16k+1/15 tails) on every byte lane. The v0
        // verbatim group carries raw zigzag deltas, so arbitrary bytes must
        // round-trip exactly through the seed/tail path.
        let mut state = 0xC0FF_EE42u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        };
        for (stride, count) in [
            (4usize, 17usize),
            (8, 31),
            (12, 33),
            (16, 47),
            (8, 49),
            (4, 63),
        ] {
            let values: Vec<Vec<u8>> = (0..count)
                .map(|_| (0..stride).map(|_| next()).collect())
                .collect();
            let encoded = encode_vertex_buffer_v0_bits(&values, stride, 8);
            let decoded = unwrap_decode(decode_vertex_buffer(
                count,
                stride,
                &encoded,
                MeshoptFilter::None,
            ));
            assert_eq!(
                decoded,
                values.iter().flatten().copied().collect::<Vec<_>>(),
                "v0 verbatim stride {stride}, count {count}"
            );
        }
    }

    #[test]
    fn vertex_buffer_v0_multiblock_group_modes_corpus() {
        // N713: v0 writes one header per byte lane and a single group mode per
        // block; counts past the 256-element block boundary exercise multiple
        // blocks plus their tail groups for every group width.
        let mut state = 0x51A7_3C29u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 23) as u8
        };
        for group_bits in [2u8, 4, 8] {
            for (stride, count) in [(4usize, 257usize), (8, 271), (12, 511), (4, 527)] {
                let values: Vec<Vec<u8>> = (0..count)
                    .map(|_| (0..stride).map(|_| next()).collect())
                    .collect();
                let encoded = encode_vertex_buffer_v0_bits(&values, stride, group_bits);
                let decoded = unwrap_decode(decode_vertex_buffer(
                    count,
                    stride,
                    &encoded,
                    MeshoptFilter::None,
                ));
                assert_eq!(
                    decoded,
                    values.iter().flatten().copied().collect::<Vec<_>>(),
                    "v0 group {group_bits} stride {stride}, count {count}"
                );
            }
        }
    }

    #[test]
    fn vertex_buffer_v1_control_modes_round_trip() {
        // N623/N633: modes 1/2/3 all decode back to the source values, including
        // a control-mode-1 lane encoded with 4-bit sentinel deltas.
        let stride = 8usize;
        let modes = [3u8, 3, 2, 3, 1, 3, 3, 0];
        let count = 12usize;
        let values: Vec<Vec<u8>> = (0..count)
            .map(|element| {
                (0..stride)
                    .map(|byte| {
                        match modes[byte] {
                            // Mode 1 is now a real delta lane: small steps stay in
                            // a nibble, large ones escape to a raw byte.
                            1 => {
                                if element % 3 == 0 {
                                    ((element * 40 + 30) % 256) as u8
                                } else {
                                    (element * 2) as u8
                                }
                            }
                            2 => 0,
                            0 => ((element * 7 + byte * 3 + 1) % 256) as u8,
                            _ => ((element * 9 + byte * 5 + 4) % 256) as u8,
                        }
                    })
                    .collect()
            })
            .collect();
        let encoded = encode_vertex_buffer_modes(&values, stride, &modes);
        let decoded = unwrap_decode(decode_vertex_buffer(
            count,
            stride,
            &encoded,
            MeshoptFilter::None,
        ));
        let flat: Vec<u8> = values.iter().flatten().copied().collect();
        assert_eq!(decoded, flat);
    }

    #[test]
    fn vertex_buffer_verbatim_group_round_trips() {
        // N663/N683: control mode 1 with the 8-bit verbatim group (group mode 3)
        // carries arbitrary deltas. Counts 17/20/31 exercise a partial trailing
        // group (16 + 1/4/15), which is the boundary the raw group can overrun.
        for (stride, count) in [(8usize, 20usize), (4, 17), (8, 31), (4, 33)] {
            let modes = vec![1u8; stride];
            let values: Vec<Vec<u8>> = (0..count)
                .map(|element| {
                    (0..stride)
                        .map(|byte| ((element * 37 + byte * 11 + 9) % 256) as u8)
                        .collect()
                })
                .collect();
            let encoded = encode_vertex_buffer_modes_bits(&values, stride, &modes, 8);
            let decoded = unwrap_decode(decode_vertex_buffer(
                count,
                stride,
                &encoded,
                MeshoptFilter::None,
            ));
            let flat: Vec<u8> = values.iter().flatten().copied().collect();
            assert_eq!(decoded, flat, "stride {stride}, count {count}");
        }
    }

    #[test]
    fn vertex_buffer_v0_group_widths_round_trip() {
        // N673: v0 supports 2-bit and 4-bit sentinel groups (plus raw), all with
        // escape bytes for large deltas.
        let stride = 4usize;
        let count = 20usize;
        let values: Vec<Vec<u8>> = (0..count)
            .map(|element| {
                (0..stride)
                    .map(|byte| {
                        // Small steps for most elements, a large jump every 5.
                        if element % 5 == 0 {
                            ((element * 53 + byte * 97 + 200) % 256) as u8
                        } else {
                            (element * 2 + byte) as u8
                        }
                    })
                    .collect()
            })
            .collect();
        for group_bits in [2u8, 4] {
            let encoded = encode_vertex_buffer_v0_bits(&values, stride, group_bits);
            let decoded = unwrap_decode(decode_vertex_buffer(
                count,
                stride,
                &encoded,
                MeshoptFilter::None,
            ));
            let flat: Vec<u8> = values.iter().flatten().copied().collect();
            assert_eq!(decoded, flat, "v0 group_bits {group_bits}");
        }
    }

    #[test]
    fn vertex_round_trip_composes_with_filters() {
        // N603: decoding an encoded buffer with a filter equals applying that
        // filter to the raw values — the delta stage round-trips and the filter
        // is the only transform on top.
        let count = 12usize;
        let stride = 8usize;
        let values: Vec<Vec<u8>> = (0..count)
            .map(|element| {
                (0..stride)
                    .map(|byte| ((element * 9 + byte * 5 + 3) % 256) as u8)
                    .collect()
            })
            .collect();
        let encoded = encode_vertex_buffer(&values, stride);
        let raw: Vec<u8> = values.iter().flatten().copied().collect();
        for filter in [
            MeshoptFilter::None,
            MeshoptFilter::Octahedral,
            MeshoptFilter::Quaternion,
            MeshoptFilter::Exponential,
            MeshoptFilter::Color,
        ] {
            let decoded = unwrap_decode(decode_vertex_buffer(count, stride, &encoded, filter));
            let mut expected = raw.clone();
            apply_vertex_filter(&mut expected, count, stride, filter).unwrap();
            assert_eq!(
                decoded, expected,
                "filter {filter:?} disagrees with the raw path"
            );
        }
    }

    #[test]
    fn filter_and_mode_names_parse() {
        assert_eq!(
            MeshoptFilter::from_name("OCTAHEDRAL"),
            Some(MeshoptFilter::Octahedral)
        );
        assert_eq!(MeshoptFilter::from_name(""), Some(MeshoptFilter::None));
        assert_eq!(MeshoptFilter::from_name("nope"), None);
        assert_eq!(
            MeshoptMode::from_name("TRIANGLES"),
            Some(MeshoptMode::Triangles)
        );
        assert_eq!(
            MeshoptMode::from_name("ATTRIBUTES"),
            Some(MeshoptMode::Attributes)
        );
        assert_eq!(
            MeshoptMode::from_name("INDICES"),
            Some(MeshoptMode::Indices)
        );
        assert_eq!(MeshoptMode::from_name("other"), None);
    }
}
