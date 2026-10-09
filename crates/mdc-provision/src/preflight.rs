//! Flash preflight verdicts reimplemented from AgentDeck `esp32PreflightVerdict`
//! semantics (MIT reference). No upstream TypeScript is copied.

use crate::{Artifact, BoardFacts, FlashSize, PreflightError};
use mdc_protocol::Version;
use std::path::Path;

/// Declared image geometry that must match the board SSOT when present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGeometry {
    pub chip_family: String,
    pub flash_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightVerdict {
    /// Safe to flash.
    Ok,
    /// Flash id was unreadable (0 / 0xffffff class). Size unknown — may flash
    /// only when artifact + profile declare an explicit size and caller accepts risk.
    OkUnknownFlash,
    Reject(PreflightError),
}

#[derive(Debug, Clone)]
pub struct PreflightInput<'a> {
    pub path: &'a Path,
    pub artifact: &'a Artifact,
    pub board: &'a BoardFacts,
    pub host: Version,
    /// Detected chip description family, e.g. "ESP32-S3". None = not probed yet.
    pub detected_chip_family: Option<&'a str>,
    /// Manifest / artifact image geometry. None skips geometry check.
    pub image_geometry: Option<&'a ImageGeometry>,
    /// When true, Unknown flash may become OkUnknownFlash instead of Reject.
    pub allow_unknown_flash: bool,
    /// flash-id unusable (AgentDeck: 0 or 0xffffff) — treat detected size as Unknown.
    pub flash_id_unusable: bool,
}

pub fn preflight_verdict(input: PreflightInput<'_>) -> PreflightVerdict {
    if !input.board.profile_verified {
        return PreflightVerdict::Reject(PreflightError::UnverifiedProfile);
    }
    if input.artifact.board_id != input.board.board_id {
        return PreflightVerdict::Reject(PreflightError::BoardMismatch);
    }
    if input.artifact.mcu != input.board.mcu {
        return PreflightVerdict::Reject(PreflightError::McuMismatch);
    }
    if let Some(detected) = input.detected_chip_family {
        if !chip_family_matches(&input.board.mcu, detected) {
            return PreflightVerdict::Reject(PreflightError::ChipMismatch);
        }
    }
    if input.artifact.protocol_major != input.host.major {
        return PreflightVerdict::Reject(PreflightError::ProtocolMajorMismatch);
    }
    if input.artifact.sha256.len() != 64
        || !input.artifact.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || input.artifact.image_size == 0
    {
        return PreflightVerdict::Reject(PreflightError::InvalidMetadata);
    }
    if let Some(geo) = input.image_geometry {
        if !chip_family_matches(&input.board.mcu, &geo.chip_family) {
            return PreflightVerdict::Reject(PreflightError::ImageGeometryMismatch);
        }
        match input.board.flash {
            FlashSize::Known(profile_bytes) if profile_bytes != geo.flash_bytes => {
                return PreflightVerdict::Reject(PreflightError::ImageGeometryMismatch);
            }
            _ => {}
        }
    }

    let flash = if input.flash_id_unusable {
        FlashSize::Unknown
    } else {
        input.board.flash
    };

    let capacity = match flash {
        FlashSize::Known(bytes) => Some(bytes),
        FlashSize::Unknown => None,
    };

    let metadata = match std::fs::metadata(input.path) {
        Ok(m) => m,
        Err(_) => return PreflightVerdict::Reject(PreflightError::MissingFile),
    };
    if metadata.len() != input.artifact.image_size {
        return PreflightVerdict::Reject(PreflightError::ImageTooLarge);
    }
    if let Some(cap) = capacity {
        // Directional: declared must be <= detected; never invent 4MB for unknown.
        if input.artifact.image_size > cap {
            return PreflightVerdict::Reject(PreflightError::ImageTooLarge);
        }
    } else if !input.allow_unknown_flash {
        return PreflightVerdict::Reject(PreflightError::UnknownFlash);
    }

    let bytes = match std::fs::read(input.path) {
        Ok(b) => b,
        Err(_) => return PreflightVerdict::Reject(PreflightError::MissingFile),
    };
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(&bytes);
    let actual = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if actual != input.artifact.sha256.to_ascii_lowercase() {
        return PreflightVerdict::Reject(PreflightError::HashMismatch);
    }

    if capacity.is_none() {
        PreflightVerdict::OkUnknownFlash
    } else {
        PreflightVerdict::Ok
    }
}

fn chip_family_matches(board_mcu: &str, detected: &str) -> bool {
    let normalize = |s: &str| {
        s.to_ascii_lowercase()
            .replace('_', "-")
            .replace(' ', "")
    };
    // Exact family match only. "ESP32" must not satisfy "esp32-s3".
    normalize(board_mcu) == normalize(detected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Artifact;
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP: AtomicU64 = AtomicU64::new(0);

    fn temp(bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mdc-preflight-{}-{}",
            std::process::id(),
            TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn art(hash: String, size: u64) -> Artifact {
        Artifact {
            board_id: "board".into(),
            mcu: "esp32-s3".into(),
            runtime: "0.1.0".into(),
            protocol_major: 1,
            protocol_minor: 0,
            image_size: size,
            sha256: hash,
            source: "test".into(),
        }
    }

    #[test]
    fn chip_mismatch_and_unknown_flash_paths() {
        let bytes = b"fw";
        let path = temp(bytes);
        let hash = format!("{:x}", Sha256::digest(bytes));
        let artifact = art(hash, 2);
        let board = BoardFacts {
            board_id: "board".into(),
            mcu: "esp32-s3".into(),
            flash: FlashSize::Known(1024),
            profile_verified: true,
        };
        let reject = preflight_verdict(PreflightInput {
            path: &path,
            artifact: &artifact,
            board: &board,
            host: Version { major: 1, minor: 0 },
            detected_chip_family: Some("ESP32"),
            image_geometry: None,
            allow_unknown_flash: false,
            flash_id_unusable: false,
        });
        assert!(matches!(
            reject,
            PreflightVerdict::Reject(PreflightError::ChipMismatch)
        ));

        let unknown_board = BoardFacts {
            flash: FlashSize::Unknown,
            ..board.clone()
        };
        let ok_unknown = preflight_verdict(PreflightInput {
            path: &path,
            artifact: &artifact,
            board: &unknown_board,
            host: Version { major: 1, minor: 0 },
            detected_chip_family: Some("ESP32-S3"),
            image_geometry: None,
            allow_unknown_flash: true,
            flash_id_unusable: true,
        });
        assert_eq!(ok_unknown, PreflightVerdict::OkUnknownFlash);
        let _ = std::fs::remove_file(path);
    }
}
