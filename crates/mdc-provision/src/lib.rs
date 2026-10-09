mod preflight;
pub use preflight::{preflight_verdict, ImageGeometry, PreflightInput, PreflightVerdict};

use mdc_protocol::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashSize {
    Known(u64),
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifact {
    pub board_id: String,
    pub mcu: String,
    pub runtime: String,
    pub protocol_major: u8,
    pub protocol_minor: u8,
    pub image_size: u64,
    pub sha256: String,
    pub source: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardFacts {
    pub board_id: String,
    pub mcu: String,
    pub flash: FlashSize,
    pub profile_verified: bool,
}
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PreflightError {
    #[error("board profile is not verified")]
    UnverifiedProfile,
    #[error("artifact board does not match board profile")]
    BoardMismatch,
    #[error("artifact MCU does not match detected MCU")]
    McuMismatch,
    #[error("detected chip family does not match board profile")]
    ChipMismatch,
    #[error("image geometry does not match board profile")]
    ImageGeometryMismatch,
    #[error("flash capacity is unknown; refusing automatic provisioning")]
    UnknownFlash,
    #[error("artifact size exceeds physical flash")]
    ImageTooLarge,
    #[error("artifact protocol major is incompatible")]
    ProtocolMajorMismatch,
    #[error("artifact hash is invalid")]
    HashMismatch,
    #[error("artifact file is missing")]
    MissingFile,
    #[error("artifact metadata is invalid")]
    InvalidMetadata,
}

pub fn verify_artifact(
    path: &Path,
    artifact: &Artifact,
    board: &BoardFacts,
    host: Version,
) -> Result<(), PreflightError> {
    if !board.profile_verified {
        return Err(PreflightError::UnverifiedProfile);
    }
    if artifact.board_id != board.board_id {
        return Err(PreflightError::BoardMismatch);
    }
    if artifact.mcu != board.mcu {
        return Err(PreflightError::McuMismatch);
    }
    if artifact.protocol_major != host.major {
        return Err(PreflightError::ProtocolMajorMismatch);
    }
    if artifact.sha256.len() != 64
        || !artifact.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || artifact.image_size == 0
    {
        return Err(PreflightError::InvalidMetadata);
    }
    let capacity = match board.flash {
        FlashSize::Known(bytes) => bytes,
        FlashSize::Unknown => return Err(PreflightError::UnknownFlash),
    };
    let metadata = std::fs::metadata(path).map_err(|_| PreflightError::MissingFile)?;
    if artifact.image_size > capacity || metadata.len() != artifact.image_size {
        return Err(PreflightError::ImageTooLarge);
    }
    let bytes = std::fs::read(path).map_err(|_| PreflightError::MissingFile)?;
    let digest = Sha256::digest(&bytes);
    let actual = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if actual != artifact.sha256.to_ascii_lowercase() {
        return Err(PreflightError::HashMismatch);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    pub port: String,
    pub artifact: Artifact,
    pub expected_device_id: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    Idle,
    AcquiringPort,
    Flashing,
    Resetting,
    Verifying,
    Succeeded,
    Cancelled,
    Failed,
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum InstallError {
    #[error("serial port is already leased")]
    PortBusy,
    #[error("install was cancelled")]
    Cancelled,
    #[error("flasher failed: {0}")]
    Flasher(String),
    #[error("post-flash identity does not match expected device")]
    IdentityMismatch,
}
pub trait Flasher {
    fn flash(&mut self, plan: &InstallPlan) -> Result<(), String>;
    fn reset(&mut self) -> Result<(), String>;
    fn read_device_id(&mut self) -> Result<String, String>;
}
#[derive(Clone, Default)]
pub struct PortLeases {
    ports: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
}
impl PortLeases {
    pub fn acquire(&self, port: &str) -> Result<PortLease, InstallError> {
        let mut ports = self
            .ports
            .lock()
            .map_err(|_| InstallError::Flasher("port lease lock poisoned".into()))?;
        if !ports.insert(port.into()) {
            return Err(InstallError::PortBusy);
        }
        drop(ports);
        Ok(PortLease {
            leases: self.clone(),
            port: port.into(),
        })
    }
}
pub struct PortLease {
    leases: PortLeases,
    port: String,
}
impl Drop for PortLease {
    fn drop(&mut self) {
        if let Ok(mut ports) = self.leases.ports.lock() {
            ports.remove(&self.port);
        }
    }
}
pub struct Installer {
    pub state: InstallState,
    cancelled: bool,
}
impl Default for Installer {
    fn default() -> Self {
        Self {
            state: InstallState::Idle,
            cancelled: false,
        }
    }
}
impl Installer {
    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.state = InstallState::Cancelled;
    }
    pub fn run<F: Flasher>(
        &mut self,
        leases: &mut PortLeases,
        plan: &InstallPlan,
        flasher: &mut F,
    ) -> Result<(), InstallError> {
        if self.cancelled {
            return Err(InstallError::Cancelled);
        }
        self.state = InstallState::AcquiringPort;
        let _lease = leases.acquire(&plan.port)?;
        if self.cancelled {
            return Err(InstallError::Cancelled);
        }
        self.state = InstallState::Flashing;
        flasher.flash(plan).map_err(|e| {
            self.state = InstallState::Failed;
            InstallError::Flasher(e)
        })?;
        if self.cancelled {
            return Err(InstallError::Cancelled);
        }
        self.state = InstallState::Resetting;
        flasher.reset().map_err(|e| {
            self.state = InstallState::Failed;
            InstallError::Flasher(e)
        })?;
        self.state = InstallState::Verifying;
        let id = flasher.read_device_id().map_err(|e| {
            self.state = InstallState::Failed;
            InstallError::Flasher(e)
        })?;
        if id != plan.expected_device_id {
            self.state = InstallState::Failed;
            return Err(InstallError::IdentityMismatch);
        }
        self.state = InstallState::Succeeded;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    fn artifact(_path: &Path, hash: String, size: u64) -> Artifact {
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
    fn temp(bytes: &[u8]) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let serial = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mdc-provision-{}-{stamp}-{serial}",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }
    #[test]
    fn accepts_verified_artifact() {
        let bytes = b"firmware";
        let path = temp(bytes);
        let hash = format!("{:x}", Sha256::digest(bytes));
        let result = verify_artifact(
            &path,
            &artifact(&path, hash, 8),
            &BoardFacts {
                board_id: "board".into(),
                mcu: "esp32-s3".into(),
                flash: FlashSize::Known(1024),
                profile_verified: true,
            },
            Version { major: 1, minor: 0 },
        );
        assert_eq!(result, Ok(()));
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn rejects_unknown_flash_and_hash() {
        let bytes = b"firmware";
        let path = temp(bytes);
        let a = artifact(&path, "0".repeat(64), 8);
        let b = BoardFacts {
            board_id: "board".into(),
            mcu: "esp32-s3".into(),
            flash: FlashSize::Unknown,
            profile_verified: true,
        };
        assert_eq!(
            verify_artifact(&path, &a, &b, Version { major: 1, minor: 0 }),
            Err(PreflightError::UnknownFlash)
        );
        let _ = std::fs::remove_file(path);
    }
    struct FakeFlasher {
        id: String,
        flashed: bool,
    }
    impl Flasher for FakeFlasher {
        fn flash(&mut self, _: &InstallPlan) -> Result<(), String> {
            self.flashed = true;
            Ok(())
        }
        fn reset(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn read_device_id(&mut self) -> Result<String, String> {
            Ok(self.id.clone())
        }
    }
    #[test]
    fn install_requires_identity_readback_and_releases_port() {
        let plan = InstallPlan {
            port: "/dev/test".into(),
            artifact: Artifact {
                board_id: "b".into(),
                mcu: "m".into(),
                runtime: "r".into(),
                protocol_major: 1,
                protocol_minor: 0,
                image_size: 1,
                sha256: "0".repeat(64),
                source: "s".into(),
            },
            expected_device_id: "id".into(),
        };
        let mut leases = PortLeases::default();
        let first = leases.acquire("/dev/test").unwrap();
        assert_eq!(
            leases.acquire("/dev/test").err(),
            Some(InstallError::PortBusy)
        );
        drop(first);
        let mut installer = Installer::default();
        let mut flasher = FakeFlasher {
            id: "id".into(),
            flashed: false,
        };
        assert!(installer.run(&mut leases, &plan, &mut flasher).is_ok());
        assert_eq!(installer.state, InstallState::Succeeded);
        assert!(flasher.flashed);
        assert!(leases.acquire("/dev/test").is_ok());
    }
    #[test]
    fn mismatched_readback_fails() {
        let plan = InstallPlan {
            port: "p".into(),
            artifact: Artifact {
                board_id: "b".into(),
                mcu: "m".into(),
                runtime: "r".into(),
                protocol_major: 1,
                protocol_minor: 0,
                image_size: 1,
                sha256: "0".repeat(64),
                source: "s".into(),
            },
            expected_device_id: "expected".into(),
        };
        let mut installer = Installer::default();
        let mut leases = PortLeases::default();
        let mut flasher = FakeFlasher {
            id: "other".into(),
            flashed: false,
        };
        assert_eq!(
            installer.run(&mut leases, &plan, &mut flasher),
            Err(InstallError::IdentityMismatch)
        );
        assert_eq!(installer.state, InstallState::Failed);
    }
}
