//! N544: a bounded reader for the KTX2 container header.
//!
//! `KHR_texture_basisu` stores textures as a KTX2 file whose payload is Basis
//! Universal. The host does not transcode Basis (that needs a full transcoder),
//! but it *can* read the fixed 80-byte header to report the real pixel size,
//! `vkFormat` and supercompression scheme. That keeps a compressed asset from
//! being reported as a 0x0 texture while still refusing to decode it.
//!
//! The parser only reads fixed-width little-endian fields and never trusts a
//! length past the header, so a truncated or crafted file is reported as `None`
//! rather than panicking.

/// The 12-byte KTX2 file identifier (`«KTX 20»\r\n\x1A\n`).
pub const KTX2_IDENTIFIER: [u8; 12] = [
    0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
];

/// The fixed KTX2 header length (identifier + 17 `u32` fields).
pub const KTX2_HEADER_BYTES: usize = 80;

/// KTX2 supercompression schemes.
pub const KTX2_SCHEME_NONE: u32 = 0;
pub const KTX2_SCHEME_BASISLZ: u32 = 1;
pub const KTX2_SCHEME_ZSTD: u32 = 2;
pub const KTX2_SCHEME_ZLIB: u32 = 3;

/// N544: the fixed-width fields of a KTX2 header that describe the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ktx2Header {
    /// `VK_FORMAT_UNDEFINED` (0) when the payload is Basis Universal.
    pub vk_format: u32,
    /// Bytes per pixel for an uncompressed format (0 otherwise).
    pub type_size: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub pixel_depth: u32,
    pub layer_count: u32,
    pub face_count: u32,
    pub level_count: u32,
    pub supercompression_scheme: u32,
}

impl Ktx2Header {
    /// N544: read the header, or `None` when the bytes are not a KTX2 file or
    /// are shorter than the 80-byte header.
    pub fn parse(bytes: &[u8]) -> Option<Ktx2Header> {
        if !is_ktx2(bytes) {
            return None;
        }
        let header = bytes.get(..KTX2_HEADER_BYTES)?;
        let read = |offset: usize| -> u32 {
            u32::from_le_bytes([
                header[offset],
                header[offset + 1],
                header[offset + 2],
                header[offset + 3],
            ])
        };
        Some(Ktx2Header {
            vk_format: read(12),
            type_size: read(16),
            pixel_width: read(20),
            pixel_height: read(24),
            pixel_depth: read(28),
            layer_count: read(32),
            face_count: read(36),
            level_count: read(40),
            supercompression_scheme: read(44),
        })
    }

    /// N544: whether the payload is Basis Universal (its `vkFormat` is undefined
    /// or it uses the BasisLZ supercompression scheme), which the host does not
    /// transcode.
    pub fn is_basis(&self) -> bool {
        self.vk_format == 0 || self.supercompression_scheme == KTX2_SCHEME_BASISLZ
    }

    /// N544: the supercompression scheme as a stable name.
    pub fn scheme_name(&self) -> &'static str {
        match self.supercompression_scheme {
            KTX2_SCHEME_NONE => "none",
            KTX2_SCHEME_BASISLZ => "BasisLZ",
            KTX2_SCHEME_ZSTD => "ZSTD",
            KTX2_SCHEME_ZLIB => "ZLIB",
            _ => "unknown",
        }
    }

    /// N544: a short description of the `vkFormat`, falling back to the numeric
    /// value for formats this reader does not name.
    pub fn format_name(&self) -> String {
        match self.vk_format {
            0 => "undefined".to_string(),
            23 => "R8G8B8_UNORM".to_string(),
            37 => "R8G8B8A8_UNORM".to_string(),
            43 => "R8G8B8A8_SRGB".to_string(),
            131 => "ASTC_4x4_UNORM".to_string(),
            137 => "ASTC_4x4_SRGB".to_string(),
            147 => "ETC2_R8G8B8_UNORM".to_string(),
            148 => "ETC2_R8G8B8_SRGB".to_string(),
            157 => "BC7_UNORM".to_string(),
            other => format!("vkFormat {other}"),
        }
    }
}

/// N544: whether the bytes begin with the KTX2 identifier.
pub fn is_ktx2(bytes: &[u8]) -> bool {
    bytes.len() >= KTX2_IDENTIFIER.len() && bytes[..KTX2_IDENTIFIER.len()] == KTX2_IDENTIFIER
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 80-byte KTX2 header with the given fields.
    fn header(vk_format: u32, width: u32, height: u32, scheme: u32) -> Vec<u8> {
        let mut bytes = KTX2_IDENTIFIER.to_vec();
        let mut push = |value: u32| bytes.extend_from_slice(&value.to_le_bytes());
        push(vk_format); // vkFormat
        push(0); // typeSize
        push(width); // pixelWidth
        push(height); // pixelHeight
        push(0); // pixelDepth
        push(0); // layerCount
        push(1); // faceCount
        push(1); // levelCount
        push(scheme); // supercompressionScheme
        push(0); // dfdByteOffset
        push(0); // dfdByteLength
        push(0); // kvdByteOffset
        push(0); // kvdByteLength
        bytes.extend_from_slice(&0u64.to_le_bytes()); // sgdByteOffset
        bytes.extend_from_slice(&0u64.to_le_bytes()); // sgdByteLength
        assert_eq!(bytes.len(), KTX2_HEADER_BYTES);
        bytes
    }

    #[test]
    fn header_fields_are_read_from_the_fixed_layout() {
        let bytes = header(0, 1024, 512, KTX2_SCHEME_BASISLZ);
        let parsed = Ktx2Header::parse(&bytes).expect("a KTX2 header");
        assert_eq!(parsed.pixel_width, 1024);
        assert_eq!(parsed.pixel_height, 512);
        assert_eq!(parsed.face_count, 1);
        assert_eq!(parsed.level_count, 1);
        assert_eq!(parsed.supercompression_scheme, KTX2_SCHEME_BASISLZ);
        assert!(parsed.is_basis());
        assert_eq!(parsed.scheme_name(), "BasisLZ");
        assert_eq!(parsed.format_name(), "undefined");
    }

    #[test]
    fn vk_format_is_named_when_known() {
        let bytes = header(43, 64, 64, KTX2_SCHEME_NONE);
        let parsed = Ktx2Header::parse(&bytes).unwrap();
        assert_eq!(parsed.format_name(), "R8G8B8A8_SRGB");
        assert!(!parsed.is_basis());
        assert_eq!(parsed.scheme_name(), "none");
        let unknown = header(999, 1, 1, KTX2_SCHEME_ZSTD);
        let parsed = Ktx2Header::parse(&unknown).unwrap();
        assert_eq!(parsed.format_name(), "vkFormat 999");
        assert_eq!(parsed.scheme_name(), "ZSTD");
    }

    #[test]
    fn short_or_foreign_bytes_are_not_ktx2() {
        assert!(Ktx2Header::parse(b"not a ktx2 file").is_none());
        // A valid identifier without the 80-byte header is rejected.
        let short = KTX2_IDENTIFIER.to_vec();
        assert!(Ktx2Header::parse(&short).is_none());
        assert!(is_ktx2(&short));
        assert!(!is_ktx2(b"PNG"));
    }
}
