//! Firmware provisioning helpers (DTO -> provision types, progress mapping,
//! cancellable simulator writer).
use super::*;

pub(crate) fn image_len(value: f64) -> Result<u64> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return Err(Error::from_reason(
            "artifact.imageSize must be a whole byte count",
        ));
    }
    Ok(value as u64)
}

pub(crate) fn artifact_from_dto(dto: &FirmwareArtifactDto) -> Result<Artifact> {
    Ok(Artifact {
        board_id: dto.board_id.clone(),
        mcu: dto.mcu.clone(),
        runtime: dto.runtime.clone(),
        protocol_major: dto.protocol_major.min(u32::from(u8::MAX)) as u8,
        protocol_minor: dto.protocol_minor.min(u32::from(u8::MAX)) as u8,
        image_size: image_len(dto.image_size)?,
        sha256: dto.sha256.clone(),
        source: dto.source.clone(),
        public_key: dto
            .public_key
            .as_ref()
            .map(|bytes| bytes.to_vec())
            .unwrap_or_default(),
        signature: dto
            .signature
            .as_ref()
            .map(|bytes| bytes.to_vec())
            .unwrap_or_default(),
    })
}

pub(crate) fn board_facts_from_dto(dto: &FirmwareBoardFactsDto) -> Result<BoardFacts> {
    let flash = match dto.flash_bytes {
        Some(bytes) => FlashSize::Known(image_len(bytes)?),
        None => FlashSize::Unknown,
    };
    Ok(BoardFacts {
        board_id: dto.board_id.clone(),
        mcu: dto.mcu.clone(),
        flash,
        profile_verified: dto.profile_verified,
    })
}

pub(crate) fn preflight_verdict_for(
    request: &FirmwarePreflightRequest,
) -> Result<PreflightVerdict> {
    let artifact = artifact_from_dto(&request.artifact)?;
    let board = board_facts_from_dto(&request.board)?;
    let geometry = match request.image_geometry.as_ref() {
        Some(geometry) => Some(ImageGeometry {
            chip_family: geometry.chip_family.clone(),
            flash_bytes: image_len(geometry.flash_bytes)?,
        }),
        None => None,
    };
    Ok(preflight_verdict_bytes(PreflightInputBytes {
        bytes: &request.image,
        artifact: &artifact,
        board: &board,
        host: mdc_protocol::Version {
            major: request.host_protocol_major.min(u32::from(u8::MAX)) as u8,
            minor: request.host_protocol_minor.min(u32::from(u8::MAX)) as u8,
        },
        detected_chip_family: request.detected_chip_family.as_deref(),
        image_geometry: geometry.as_ref(),
        allow_unknown_flash: request.allow_unknown_flash.unwrap_or(false),
        flash_id_unusable: request.flash_id_unusable.unwrap_or(false),
    }))
}

pub(crate) fn preflight_result(verdict: PreflightVerdict) -> FirmwarePreflightResult {
    match verdict {
        PreflightVerdict::Ok => FirmwarePreflightResult {
            ok: true,
            verdict: "ok".into(),
            reason: None,
        },
        PreflightVerdict::OkUnknownFlash => FirmwarePreflightResult {
            ok: true,
            verdict: "okUnknownFlash".into(),
            reason: None,
        },
        PreflightVerdict::Reject(error) => FirmwarePreflightResult {
            ok: false,
            verdict: "reject".into(),
            reason: Some(format!("{error:?}")),
        },
    }
}

pub(crate) fn install_phase(state: InstallState) -> &'static str {
    match state {
        InstallState::Idle => "idle",
        InstallState::AcquiringPort => "lease",
        InstallState::Flashing => "writing",
        InstallState::Resetting => "rebooting",
        InstallState::Verifying => "readback",
        InstallState::Succeeded => "done",
        InstallState::Cancelled => "cancelled",
        InstallState::Failed => "failed",
    }
}

pub(crate) fn install_ratio(state: InstallState) -> Option<f64> {
    match state {
        InstallState::AcquiringPort => Some(0.1),
        InstallState::Flashing => Some(0.3),
        InstallState::Resetting => Some(0.7),
        InstallState::Verifying => Some(0.9),
        InstallState::Succeeded => Some(1.0),
        _ => None,
    }
}

pub(crate) fn install_error_reason(error: &InstallError) -> String {
    match error {
        InstallError::PortBusy => "PortBusy".into(),
        InstallError::Cancelled => "Cancelled".into(),
        InstallError::Flasher(message) => format!("Flasher: {message}"),
        InstallError::IdentityMismatch => "IdentityMismatch".into(),
    }
}

pub(crate) fn set_progress(
    progress: &Mutex<FirmwareProgressDto>,
    phase: &str,
    ratio: Option<f64>,
    error: Option<String>,
) {
    if let Ok(mut slot) = progress.lock() {
        *slot = FirmwareProgressDto {
            phase: phase.into(),
            ratio,
            error,
        };
    }
}

/// Simulator flasher with a cancellable delay so the public SDK can show
/// progress and settle `cancelFirmware()` before the write completes.
pub(crate) struct DelayedSimFlasher {
    inner: SimFlasher,
    delay: Duration,
    cancel: Arc<AtomicBool>,
}

impl DelayedSimFlasher {
    pub(crate) fn new(device_id: String, delay: Duration, cancel: Arc<AtomicBool>) -> Self {
        Self {
            inner: SimFlasher::new(device_id),
            delay,
            cancel,
        }
    }

    fn wait(&self) -> std::result::Result<(), String> {
        let mut remaining = self.delay;
        while !remaining.is_zero() {
            if self.cancel.load(AtomicOrdering::Acquire) {
                return Err("cancelled".into());
            }
            let slice = remaining.min(Duration::from_millis(20));
            thread::sleep(slice);
            remaining = remaining.saturating_sub(slice);
        }
        Ok(())
    }
}

impl Flasher for DelayedSimFlasher {
    fn flash(&mut self, plan: &InstallPlan) -> std::result::Result<(), String> {
        self.wait()?;
        self.inner.flash(plan)
    }
    fn reset(&mut self) -> std::result::Result<(), String> {
        self.inner.reset()
    }
    fn read_device_id(&mut self) -> std::result::Result<String, String> {
        self.inner.read_device_id()
    }
}

/// esptool needs a file; the SDK receives bytes. Write a bounded temp image and
/// remove it even when the write fails.
pub(crate) fn write_temp_image(bytes: &[u8]) -> std::result::Result<PathBuf, String> {
    let serial = FIRMWARE_TEMP_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("mdc-firmware-{}-{serial}.bin", std::process::id()));
    std::fs::write(&path, bytes).map_err(|error| format!("temp image: {error}"))?;
    Ok(path)
}
