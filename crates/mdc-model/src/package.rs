//! MDC Runtime Package container (N19).
//!
//! A compiled [`DevicePackage`] is wrapped in a versioned, self-describing
//! container: a 4-byte magic, a version, a JSON manifest and the JSON IR body.
//! The manifest lets a device or provisioner inspect a package before it is
//! uploaded; N20 adds checksum verification and the capability gate.
use crate::backend::{compile_auto, DevicePackage, PackageMode, RendererCapability};
use crate::ir::{Model, ModelError};
use serde::{Deserialize, Serialize};

/// Container magic ("MDCP").
pub const PACKAGE_MAGIC: &[u8; 4] = b"MDCP";
/// Container format version.
pub const PACKAGE_VERSION: u16 = 1;
/// Largest package body accepted (16 MiB); a compile budget, not a promise.
pub const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;
/// Largest manifest accepted (it is small; guards a hostile length).
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// N408: the result of verifying a sealed package's manifest against its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageVerification {
    pub valid: bool,
    pub checksum_ok: bool,
    pub body_bytes_ok: bool,
    pub target_ok: bool,
    pub version_ok: bool,
    pub checksum: u64,
    pub body_bytes: u64,
    pub manifest_checksum: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageManifest {
    pub version: u16,
    pub target: String,
    pub mode: PackageMode,
    pub estimated_bytes: u64,
    pub body_bytes: u64,
    /// FNV-1a 64-bit checksum of the serialized body.
    pub checksum: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdcPackage {
    pub manifest: PackageManifest,
    pub package: DevicePackage,
}

impl MdcPackage {
    /// Wrap a compiled device package, computing the manifest.
    pub fn seal(package: DevicePackage) -> Result<Self, ModelError> {
        let body = body_bytes(&package)?;
        if body.len() > MAX_PACKAGE_BYTES {
            return Err(ModelError::Budget(format!(
                "package body {} over {MAX_PACKAGE_BYTES} limit",
                body.len()
            )));
        }
        Ok(Self {
            manifest: PackageManifest {
                version: PACKAGE_VERSION,
                target: package.target.clone(),
                mode: package.mode,
                estimated_bytes: package.estimated_bytes,
                body_bytes: body.len() as u64,
                checksum: fnv1a(&body),
            },
            package,
        })
    }

    /// N408: verify the manifest against the serialized body (checksum + sizes).
    pub fn verify(&self) -> PackageVerification {
        let body = body_bytes(&self.package).unwrap_or_default();
        let checksum = fnv1a(&body);
        let body_bytes = body.len() as u64;
        let checksum_ok = checksum == self.manifest.checksum;
        let body_bytes_ok = body_bytes == self.manifest.body_bytes;
        let target_ok = self.package.target == self.manifest.target;
        let version_ok = self.manifest.version == PACKAGE_VERSION;
        PackageVerification {
            valid: checksum_ok && body_bytes_ok && target_ok && version_ok,
            checksum_ok,
            body_bytes_ok,
            target_ok,
            version_ok,
            checksum,
            body_bytes,
            manifest_checksum: self.manifest.checksum,
        }
    }

    /// Serialize to the wire container bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, ModelError> {
        let manifest = serde_json::to_vec(&self.manifest).map_err(json_err)?;
        if manifest.len() > MAX_MANIFEST_BYTES {
            return Err(ModelError::Budget("manifest over limit".into()));
        }
        let body = body_bytes(&self.package)?;
        if body.len() > MAX_PACKAGE_BYTES {
            return Err(ModelError::Budget("package body over limit".into()));
        }
        let mut out = Vec::with_capacity(10 + manifest.len() + body.len());
        out.extend_from_slice(PACKAGE_MAGIC);
        out.extend_from_slice(&PACKAGE_VERSION.to_be_bytes());
        out.extend_from_slice(&(manifest.len() as u32).to_be_bytes());
        out.extend_from_slice(&manifest);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Parse a wire container. Structural validation (magic, version, bounds)
    /// plus integrity (checksum / declared length).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ModelError> {
        if bytes.len() < 10 {
            return Err(ModelError::Unsupported("package header truncated".into()));
        }
        if &bytes[0..4] != PACKAGE_MAGIC {
            return Err(ModelError::Unsupported("bad package magic".into()));
        }
        let version = u16::from_be_bytes([bytes[4], bytes[5]]);
        if version != PACKAGE_VERSION {
            return Err(ModelError::Unsupported(format!(
                "package version {version}"
            )));
        }
        let manifest_len = u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]) as usize;
        if manifest_len > MAX_MANIFEST_BYTES {
            return Err(ModelError::Budget("manifest length over limit".into()));
        }
        let manifest_start: usize = 10;
        let manifest_end = manifest_start
            .checked_add(manifest_len)
            .ok_or_else(|| ModelError::Unsupported("manifest length overflow".into()))?;
        let manifest_slice = bytes
            .get(manifest_start..manifest_end)
            .ok_or_else(|| ModelError::Unsupported("package manifest truncated".into()))?;
        let manifest: PackageManifest = serde_json::from_slice(manifest_slice).map_err(json_err)?;
        let body = bytes
            .get(manifest_end..)
            .ok_or_else(|| ModelError::Unsupported("package body truncated".into()))?;
        if body.len() > MAX_PACKAGE_BYTES {
            return Err(ModelError::Budget("package body over limit".into()));
        }
        let package: DevicePackage = serde_json::from_slice(body).map_err(json_err)?;
        let parsed = Self { manifest, package };
        parsed.verify_integrity()?;
        Ok(parsed)
    }

    /// Check the manifest against the actual body (length, checksum, target/mode).
    pub fn verify_integrity(&self) -> Result<(), ModelError> {
        let body = body_bytes(&self.package)?;
        if body.len() as u64 != self.manifest.body_bytes {
            return Err(ModelError::Unsupported(format!(
                "body length {} != manifest {}",
                body.len(),
                self.manifest.body_bytes
            )));
        }
        if fnv1a(&body) != self.manifest.checksum {
            return Err(ModelError::Unsupported("package checksum mismatch".into()));
        }
        if self.manifest.target != self.package.target || self.manifest.mode != self.package.mode {
            return Err(ModelError::Unsupported(
                "manifest target/mode does not match the package".into(),
            ));
        }
        Ok(())
    }

    /// Capability gate: a device may only accept a package its renderer can run.
    /// Mesh needs `mesh2d`; a sprite atlas needs sprite support. Checked against
    /// the advertised capability, never a board id.
    pub fn accepted_by(&self, caps: &RendererCapability) -> Result<(), ModelError> {
        self.verify_integrity()?;
        if self.manifest.estimated_bytes > MAX_PACKAGE_BYTES as u64 {
            return Err(ModelError::Budget("package over device size limit".into()));
        }
        match self.manifest.mode {
            PackageMode::Mesh if !caps.mesh2d => Err(ModelError::Unsupported(
                "device has no mesh2d for a mesh package".into(),
            )),
            PackageMode::SpriteAtlas if !caps.sprite => Err(ModelError::Unsupported(
                "device has no sprite support".into(),
            )),
            PackageMode::BakedSprites => {
                if !caps.sprite {
                    return Err(ModelError::Unsupported(
                        "device has no sprite support".into(),
                    ));
                }
                let atlas = self.package.atlas.as_ref().ok_or_else(|| {
                    ModelError::Unsupported("baked package is missing its atlas".into())
                })?;
                // The pixel buffer covers the whole grid (columns × rows), which
                // may be larger than the frame count when the grid is padded.
                if atlas.columns == 0 || atlas.rows == 0 || atlas.frames == 0 {
                    return Err(ModelError::Unsupported(
                        "baked atlas has an empty grid".into(),
                    ));
                }
                if atlas.frames > atlas.columns.saturating_mul(atlas.rows) {
                    return Err(ModelError::Unsupported(
                        "baked atlas has more frames than its grid".into(),
                    ));
                }
                let expected = u64::from(atlas.columns)
                    .saturating_mul(u64::from(atlas.rows))
                    .saturating_mul(u64::from(atlas.sprite_width))
                    .saturating_mul(u64::from(atlas.sprite_height))
                    .saturating_mul(2);
                if atlas.rgb565.len() as u64 != expected {
                    return Err(ModelError::Unsupported(
                        "baked atlas pixel size does not match its grid".into(),
                    ));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Assemble a package from chunks (N21). The header is validated as soon as the
/// first 10 bytes arrive, so a bad magic/version is rejected before the rest of
/// a hostile upload is buffered. `finish` runs the full integrity check.
#[derive(Debug, Default)]
pub struct PackageAssembler {
    limit: usize,
    buffer: Vec<u8>,
    prechecked: bool,
}

impl PackageAssembler {
    /// New assembler bounded by `MAX_PACKAGE_BYTES` for the body.
    pub fn begin() -> Self {
        Self {
            limit: MAX_PACKAGE_BYTES + MAX_MANIFEST_BYTES,
            buffer: Vec::new(),
            prechecked: false,
        }
    }

    pub fn received(&self) -> usize {
        self.buffer.len()
    }

    pub fn push_chunk(&mut self, bytes: &[u8]) -> Result<(), ModelError> {
        if bytes.is_empty() {
            return Err(ModelError::Unsupported("empty package chunk".into()));
        }
        if self.buffer.len() + bytes.len() > self.limit {
            return Err(ModelError::Budget("package over the upload limit".into()));
        }
        self.buffer.extend_from_slice(bytes);
        if !self.prechecked && self.buffer.len() >= 10 {
            self.precheck()?;
            self.prechecked = true;
        }
        Ok(())
    }

    fn precheck(&self) -> Result<(), ModelError> {
        if &self.buffer[0..4] != PACKAGE_MAGIC {
            return Err(ModelError::Unsupported("bad package magic".into()));
        }
        let version = u16::from_be_bytes([self.buffer[4], self.buffer[5]]);
        if version != PACKAGE_VERSION {
            return Err(ModelError::Unsupported(format!(
                "package version {version}"
            )));
        }
        let manifest_len = u32::from_be_bytes([
            self.buffer[6],
            self.buffer[7],
            self.buffer[8],
            self.buffer[9],
        ]) as usize;
        if manifest_len > MAX_MANIFEST_BYTES {
            return Err(ModelError::Budget("manifest length over limit".into()));
        }
        Ok(())
    }

    /// Validate and decode the assembled package.
    pub fn finish(self) -> Result<MdcPackage, ModelError> {
        MdcPackage::from_bytes(&self.buffer)
    }
}

/// Compile a model to a runtime package using the capability-selected backend.
pub fn compile_to_package(
    model: &Model,
    caps: &RendererCapability,
) -> Result<MdcPackage, ModelError> {
    MdcPackage::seal(compile_auto(model, caps)?)
}

fn body_bytes(package: &DevicePackage) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(package).map_err(json_err)
}

fn json_err(error: serde_json::Error) -> ModelError {
    ModelError::Json(error.to_string())
}

/// FNV-1a 64-bit, matching the packet integrity style used elsewhere.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gltf::GltfImporter;
    use crate::ir::ImportLimits;
    use serde_json::json;

    fn model() -> Model {
        let doc = json!({
            "asset": {"version": "2.0"},
            "nodes": [{"name": "root", "mesh": 0}],
            "meshes": [{"name": "body", "primitives": [{"attributes": {"POSITION": 0}}]}],
            "accessors": [{"count": 4, "type": "VEC3"}]
        });
        GltfImporter
            .import(&serde_json::to_vec(&doc).unwrap(), &ImportLimits::default())
            .unwrap()
    }

    #[test]
    fn compile_seal_round_trip() {
        let package = compile_to_package(&model(), &RendererCapability::sprite_only()).unwrap();
        assert_eq!(package.manifest.version, PACKAGE_VERSION);
        assert_eq!(package.manifest.target, "esp32-plane");
        assert_eq!(package.manifest.mode, PackageMode::SpriteAtlas);
        assert!(package.manifest.body_bytes > 0);
        assert_eq!(
            package.manifest.checksum,
            fnv1a(&body_bytes(&package.package).unwrap())
        );

        let bytes = package.to_bytes().unwrap();
        assert_eq!(&bytes[0..4], PACKAGE_MAGIC);
        let back = MdcPackage::from_bytes(&bytes).unwrap();
        assert_eq!(back, package);
    }

    #[test]
    fn bad_magic_and_version_are_rejected() {
        let package = compile_to_package(&model(), &RendererCapability::sprite_only()).unwrap();
        let mut bytes = package.to_bytes().unwrap();
        bytes[0] = b'X';
        assert!(matches!(
            MdcPackage::from_bytes(&bytes),
            Err(ModelError::Unsupported(_))
        ));

        let mut bytes = package.to_bytes().unwrap();
        bytes[5] = 9;
        assert!(matches!(
            MdcPackage::from_bytes(&bytes),
            Err(ModelError::Unsupported(_))
        ));
    }

    #[test]
    fn truncated_and_over_long_headers_are_rejected() {
        assert!(matches!(
            MdcPackage::from_bytes(b"MDC"),
            Err(ModelError::Unsupported(_))
        ));
        let mut bytes = vec![0u8; 10];
        bytes[0..4].copy_from_slice(PACKAGE_MAGIC);
        bytes[5] = PACKAGE_VERSION as u8;
        bytes[6..10].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            MdcPackage::from_bytes(&bytes),
            Err(ModelError::Budget(_))
        ));
    }

    #[test]
    fn tampered_packages_fail_integrity() {
        let mut package = compile_to_package(&model(), &RendererCapability::sprite_only()).unwrap();
        package.manifest.checksum ^= 1;
        assert!(matches!(
            package.verify_integrity(),
            Err(ModelError::Unsupported(_))
        ));

        let mut mismatched =
            compile_to_package(&model(), &RendererCapability::sprite_only()).unwrap();
        mismatched.package.target = "somewhere-else".into();
        assert!(matches!(
            mismatched.verify_integrity(),
            Err(ModelError::Unsupported(_))
        ));
    }

    #[test]
    fn capability_gate_rejects_a_package_the_device_cannot_run() {
        let sprite = compile_to_package(&model(), &RendererCapability::sprite_only()).unwrap();
        assert!(sprite
            .accepted_by(&RendererCapability::sprite_only())
            .is_ok());

        let mesh = compile_to_package(&model(), &RendererCapability::full_3d()).unwrap();
        assert_eq!(mesh.manifest.mode, PackageMode::Mesh);
        assert!(mesh.accepted_by(&RendererCapability::full_3d()).is_ok());
        assert!(matches!(
            mesh.accepted_by(&RendererCapability::sprite_only()),
            Err(ModelError::Unsupported(_))
        ));
    }

    #[test]
    fn assembler_rebuilds_a_package_from_chunks() {
        let package = compile_to_package(&model(), &RendererCapability::sprite_only()).unwrap();
        let bytes = package.to_bytes().unwrap();
        let mut assembler = PackageAssembler::begin();
        for chunk in bytes.chunks(7) {
            assembler.push_chunk(chunk).unwrap();
        }
        assert_eq!(assembler.received(), bytes.len());
        assert_eq!(assembler.finish().unwrap(), package);
    }

    #[test]
    fn assembler_checks_the_header_early_and_bounds_the_upload() {
        // A bad magic is rejected on the very first header chunk.
        let mut assembler = PackageAssembler::begin();
        let err = assembler.push_chunk(b"XXXX000000").unwrap_err();
        assert!(matches!(err, ModelError::Unsupported(_)), "{err:?}");

        // A hostile manifest length is rejected at the header too.
        let mut header = Vec::new();
        header.extend_from_slice(PACKAGE_MAGIC);
        header.extend_from_slice(&PACKAGE_VERSION.to_be_bytes());
        header.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            PackageAssembler::begin().push_chunk(&header),
            Err(ModelError::Budget(_))
        ));

        // The upload is bounded.
        let mut assembler = PackageAssembler::begin();
        let mut big = vec![0u8; MAX_PACKAGE_BYTES + MAX_MANIFEST_BYTES + 8];
        big[0..4].copy_from_slice(PACKAGE_MAGIC);
        big[4..6].copy_from_slice(&PACKAGE_VERSION.to_be_bytes());
        assert!(matches!(
            assembler.push_chunk(&big),
            Err(ModelError::Budget(_))
        ));
    }
}
