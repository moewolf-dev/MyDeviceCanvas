mod facts;
mod preflight;
pub use facts::{known_board_facts, KnownBoard, KNOWN_BOARDS};
pub use preflight::{
    preflight_verdict, preflight_verdict_bytes, ImageGeometry, PreflightInput, PreflightInputBytes,
    PreflightVerdict,
};

use mdc_protocol::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Empty means the artifact is unsigned. A present key or signature must match.
    #[serde(default)]
    pub public_key: Vec<u8>,
    #[serde(default)]
    pub signature: Vec<u8>,
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
    #[error("artifact signature does not match the image")]
    SignatureMismatch,
}

/// Simulator stand-in for an artifact signature. It is sha256(public_key || image), not a device key.
pub fn sign_artifact(public_key: &[u8], image: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(public_key);
    hasher.update(image);
    hasher.finalize().to_vec()
}

pub fn verify_signature(
    public_key: &[u8],
    image: &[u8],
    signature: &[u8],
) -> Result<(), PreflightError> {
    if public_key.is_empty() || signature != sign_artifact(public_key, image).as_slice() {
        return Err(PreflightError::SignatureMismatch);
    }
    Ok(())
}

pub fn verify_artifact(
    path: &Path,
    artifact: &Artifact,
    board: &BoardFacts,
    host: Version,
) -> Result<(), PreflightError> {
    // Keep the path-specific failures distinct from metadata checks.
    std::fs::metadata(path).map_err(|_| PreflightError::MissingFile)?;
    let bytes = std::fs::read(path).map_err(|_| PreflightError::MissingFile)?;
    verify_artifact_bytes(&bytes, artifact, board, host)
}

/// Byte-input variant of [`verify_artifact`] for callers that already hold the
/// image (the Node SDK receives a Buffer and must not re-read the file).
pub fn verify_artifact_bytes(
    bytes: &[u8],
    artifact: &Artifact,
    board: &BoardFacts,
    host: Version,
) -> Result<(), PreflightError> {
    match preflight_verdict_bytes(PreflightInputBytes {
        bytes,
        artifact,
        board,
        host,
        detected_chip_family: None,
        image_geometry: None,
        allow_unknown_flash: false,
        flash_id_unusable: false,
    }) {
        PreflightVerdict::Reject(error) => Err(error),
        PreflightVerdict::Ok | PreflightVerdict::OkUnknownFlash => Ok(()),
    }
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
    ports: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, Option<u64>>>>,
}
impl PortLeases {
    pub fn acquire(&self, port: &str) -> Result<PortLease, InstallError> {
        self.insert(port, None)
    }
    /// Hold a port until `expires_ms`. A later `session_may_open` at or after that time may proceed.
    pub fn hold_until(&self, port: &str, expires_ms: u64) -> Result<PortLease, InstallError> {
        self.insert(port, Some(expires_ms))
    }
    fn insert(&self, port: &str, expires_ms: Option<u64>) -> Result<PortLease, InstallError> {
        let mut ports = self
            .ports
            .lock()
            .map_err(|_| InstallError::Flasher("port lease lock poisoned".into()))?;
        if ports.contains_key(port) {
            return Err(InstallError::PortBusy);
        }
        ports.insert(port.into(), expires_ms);
        drop(ports);
        Ok(PortLease {
            leases: self.clone(),
            port: port.into(),
        })
    }
    pub fn expire_before(&self, now_ms: u64) {
        let Ok(mut ports) = self.ports.lock() else {
            return;
        };
        ports.retain(|_, expires| match expires {
            Some(at) => *at > now_ms,
            None => true,
        });
    }
    /// Check a flash lease before opening a serial port. This does not open a device node.
    pub fn session_may_open(&self, port: &str, now_ms: u64) -> Result<(), InstallError> {
        self.expire_before(now_ms);
        let ports = self
            .ports
            .lock()
            .map_err(|_| InstallError::Flasher("port lease lock poisoned".into()))?;
        if ports.contains_key(port) {
            Err(InstallError::PortBusy)
        } else {
            Ok(())
        }
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
        leases: &PortLeases,
        plan: &InstallPlan,
        flasher: &mut F,
    ) -> Result<(), InstallError> {
        let never = AtomicBool::new(false);
        self.run_with(leases, plan, flasher, &never, &mut |_| {})
    }

    /// Observable variant used by the public SDK: `cancel` is checked at every
    /// stage boundary and before/after the flasher call, so a simulator job can
    /// abort within its delay window. `progress` receives each state transition.
    /// A real esptool write cannot be preempted mid-process; it is observed at
    /// the next boundary (documented in `docs/node-sdk.md`).
    pub fn run_with<F: Flasher>(
        &mut self,
        leases: &PortLeases,
        plan: &InstallPlan,
        flasher: &mut F,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(InstallState),
    ) -> Result<(), InstallError> {
        let is_cancelled = |installer: &Self| installer.cancelled || cancel.load(Ordering::Acquire);
        if is_cancelled(self) {
            self.transition(InstallState::Cancelled, progress);
            return Err(InstallError::Cancelled);
        }
        self.transition(InstallState::AcquiringPort, progress);
        let _lease = match leases.acquire(&plan.port) {
            Ok(lease) => lease,
            Err(error) => {
                self.transition(InstallState::Failed, progress);
                return Err(error);
            }
        };
        if is_cancelled(self) {
            self.transition(InstallState::Cancelled, progress);
            return Err(InstallError::Cancelled);
        }
        self.transition(InstallState::Flashing, progress);
        let flashed = flasher.flash(plan);
        if let Err(error) = flashed {
            if is_cancelled(self) {
                self.transition(InstallState::Cancelled, progress);
                return Err(InstallError::Cancelled);
            }
            self.transition(InstallState::Failed, progress);
            return Err(InstallError::Flasher(error));
        }
        // Even a cancelled write is reset: leaving the device in the bootloader
        // is worse than completing the pulse.
        self.transition(InstallState::Resetting, progress);
        flasher.reset().map_err(|error| {
            self.transition(InstallState::Failed, progress);
            InstallError::Flasher(error)
        })?;
        if is_cancelled(self) {
            self.transition(InstallState::Cancelled, progress);
            return Err(InstallError::Cancelled);
        }
        self.transition(InstallState::Verifying, progress);
        let id = flasher.read_device_id().map_err(|error| {
            self.transition(InstallState::Failed, progress);
            InstallError::Flasher(error)
        })?;
        if id != plan.expected_device_id {
            self.transition(InstallState::Failed, progress);
            return Err(InstallError::IdentityMismatch);
        }
        self.transition(InstallState::Succeeded, progress);
        Ok(())
    }

    fn transition(&mut self, state: InstallState, progress: &mut dyn FnMut(InstallState)) {
        self.state = state;
        progress(state);
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
            public_key: Vec::new(),
            signature: Vec::new(),
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
    fn fake_signature_mismatch_rejects_without_opening_a_port() {
        let image = b"firmware-bytes";
        let key = b"fake-public-key";
        let good = sign_artifact(key, image);
        assert_eq!(verify_signature(key, image, &good), Ok(()));
        assert_eq!(
            verify_signature(key, image, b"not-the-signature"),
            Err(PreflightError::SignatureMismatch)
        );
        assert_eq!(
            verify_signature(b"", image, &good),
            Err(PreflightError::SignatureMismatch)
        );
    }
    #[test]
    fn verify_artifact_rejects_a_bad_signature_without_opening_a_port() {
        let bytes = b"firmware";
        let path = temp(bytes);
        let hash = format!("{:x}", Sha256::digest(bytes));
        let mut signed = artifact(&path, hash, bytes.len() as u64);
        signed.public_key = b"fake-public-key".to_vec();
        signed.signature = b"nope".to_vec();
        let board = BoardFacts {
            board_id: "board".into(),
            mcu: "esp32-s3".into(),
            flash: FlashSize::Known(1024),
            profile_verified: true,
        };
        assert_eq!(
            verify_artifact(&path, &signed, &board, Version { major: 1, minor: 0 }),
            Err(PreflightError::SignatureMismatch)
        );
        signed.signature = sign_artifact(&signed.public_key, bytes);
        assert_eq!(
            verify_artifact(&path, &signed, &board, Version { major: 1, minor: 0 }),
            Ok(())
        );
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
                public_key: Vec::new(),
                signature: Vec::new(),
            },
            expected_device_id: "id".into(),
        };
        let leases = PortLeases::default();
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
        assert!(installer.run(&leases, &plan, &mut flasher).is_ok());
        assert_eq!(installer.state, InstallState::Succeeded);
        assert!(flasher.flashed);
        assert!(leases.acquire("/dev/test").is_ok());
    }

    #[test]
    fn flash_lease_blocks_a_second_open_until_release_or_timeout() {
        let leases = PortLeases::default();
        let held = leases.hold_until("/dev/cu.fake-mdc", 1_000).unwrap();
        assert_eq!(
            leases.session_may_open("/dev/cu.fake-mdc", 500),
            Err(InstallError::PortBusy)
        );
        drop(held);
        assert!(leases.session_may_open("/dev/cu.fake-mdc", 500).is_ok());
        let held = leases.hold_until("/dev/cu.fake-mdc", 1_000).unwrap();
        leases.expire_before(1_000);
        assert!(leases.session_may_open("/dev/cu.fake-mdc", 1_000).is_ok());
        drop(held);
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
                public_key: Vec::new(),
                signature: Vec::new(),
            },
            expected_device_id: "expected".into(),
        };
        let mut installer = Installer::default();
        let leases = PortLeases::default();
        let mut flasher = FakeFlasher {
            id: "other".into(),
            flashed: false,
        };
        assert_eq!(
            installer.run(&leases, &plan, &mut flasher),
            Err(InstallError::IdentityMismatch)
        );
        assert_eq!(installer.state, InstallState::Failed);
    }

    #[test]
    fn bytes_preflight_matches_the_file_path_and_verifies_signatures() {
        use preflight::{preflight_verdict_bytes, PreflightInputBytes};
        let image = b"signed-firmware";
        let hash = format!("{:x}", Sha256::digest(image));
        let key = b"fake-public-key";
        let mut signed = artifact(Path::new("ignored"), hash, image.len() as u64);
        signed.public_key = key.to_vec();
        signed.signature = sign_artifact(key, image);
        let board = BoardFacts {
            board_id: "board".into(),
            mcu: "esp32-s3".into(),
            flash: FlashSize::Known(4096),
            profile_verified: true,
        };
        let verdict = |bytes: &[u8], artifact: &Artifact| {
            preflight_verdict_bytes(PreflightInputBytes {
                bytes,
                artifact,
                board: &board,
                host: Version { major: 1, minor: 0 },
                detected_chip_family: None,
                image_geometry: None,
                allow_unknown_flash: false,
                flash_id_unusable: false,
            })
        };
        assert_eq!(verdict(image, &signed), preflight::PreflightVerdict::Ok);
        // A truncated buffer is rejected before hashing as a display image.
        assert_eq!(
            verdict(&image[..3], &signed),
            preflight::PreflightVerdict::Reject(PreflightError::ImageTooLarge)
        );
        let mut forged = signed.clone();
        forged.signature = b"not-the-signature".to_vec();
        assert_eq!(
            verdict(image, &forged),
            preflight::PreflightVerdict::Reject(PreflightError::SignatureMismatch)
        );
        assert_eq!(
            verify_artifact_bytes(image, &signed, &board, Version { major: 1, minor: 0 }),
            Ok(())
        );
    }

    struct CancellingFlasher {
        cancel: std::sync::Arc<AtomicBool>,
        resets: u32,
    }
    impl Flasher for CancellingFlasher {
        fn flash(&mut self, _: &InstallPlan) -> Result<(), String> {
            self.cancel.store(true, Ordering::Release);
            Ok(())
        }
        fn reset(&mut self) -> Result<(), String> {
            self.resets = self.resets.saturating_add(1);
            Ok(())
        }
        fn read_device_id(&mut self) -> Result<String, String> {
            Ok("id".into())
        }
    }

    #[test]
    fn run_with_reports_progress_and_cancels_at_a_stage_boundary() {
        let plan = InstallPlan {
            port: "sim://cancel".into(),
            artifact: artifact(Path::new("ignored"), "0".repeat(64), 1),
            expected_device_id: "id".into(),
        };
        let leases = PortLeases::default();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let mut flasher = CancellingFlasher {
            cancel: cancel.clone(),
            resets: 0,
        };
        let mut seen = Vec::new();
        let mut installer = Installer::default();
        let result = installer.run_with(&leases, &plan, &mut flasher, &cancel, &mut |state| {
            seen.push(state)
        });
        assert_eq!(result, Err(InstallError::Cancelled));
        assert_eq!(installer.state, InstallState::Cancelled);
        // The reset still ran so the device is not left in the bootloader, and
        // the job never claimed a verified identity.
        assert_eq!(flasher.resets, 1);
        assert_eq!(
            seen,
            vec![
                InstallState::AcquiringPort,
                InstallState::Flashing,
                InstallState::Resetting,
                InstallState::Cancelled,
            ]
        );
        assert!(leases.acquire("sim://cancel").is_ok(), "lease released");
    }
}
