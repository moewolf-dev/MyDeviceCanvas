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

    /// Parse a wire container. Structural validation only (magic, version,
    /// bounds); N20 verifies the checksum and the device capability.
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
        Ok(Self { manifest, package })
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
}
