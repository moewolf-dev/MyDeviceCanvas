//! EspTool flasher adapter (AgentDeck flash semantics, reimplemented).
//!
//! Writes one merged image at offset 0x0. For ips_35 / JC3248W535 class boards:
//! `--before no_reset --no-stub`, upload baud 460800, flash mode dio / 80m / 16MB.
//! Post-write uses `D0|R1|W100|R0` (not esptool hard_reset alone).
//!
//! `dry_run` builds argv without invoking esptool — unit tests stay offline.

use crate::post_write_reset_sequence;
use mdc_provision::{Flasher, InstallPlan};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct EspToolBoardFlags {
    pub chip: String,
    pub before: String,
    pub after: String,
    pub stub: bool,
    pub baud: u32,
    pub flash_mode: String,
    pub flash_freq: String,
    pub flash_size: String,
}

impl Default for EspToolBoardFlags {
    fn default() -> Self {
        // AgentDeck ips_35 / JC3248W535 defaults
        Self {
            chip: "esp32s3".into(),
            before: "no_reset".into(),
            after: "hard_reset".into(),
            stub: false,
            baud: 460_800,
            flash_mode: "dio".into(),
            flash_freq: "80m".into(),
            flash_size: "16MB".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EspToolFlasher {
    pub esptool_bin: PathBuf,
    pub image_path: PathBuf,
    pub flags: EspToolBoardFlags,
    /// When true, never spawn esptool; record planned argv instead.
    pub dry_run: bool,
    pub last_argv: Vec<String>,
    pub reset_count: u32,
    pub expected_device_id: String,
    pub identity_override: Option<String>,
    /// Port used for post-write DTR/RTS pulse when not dry_run.
    pub reset_port: Option<String>,
}

impl EspToolFlasher {
    pub fn new(image_path: impl Into<PathBuf>) -> Self {
        Self {
            esptool_bin: PathBuf::from("esptool.py"),
            image_path: image_path.into(),
            flags: EspToolBoardFlags::default(),
            dry_run: true,
            last_argv: Vec::new(),
            reset_count: 0,
            expected_device_id: String::new(),
            identity_override: None,
            reset_port: None,
        }
    }

    pub fn with_live_esptool(mut self, bin: impl Into<PathBuf>) -> Self {
        self.esptool_bin = bin.into();
        self.dry_run = false;
        self
    }

    /// Build esptool write-flash argv for merged image @ 0x0.
    pub fn build_write_argv(&self, port: &str) -> Vec<String> {
        let mut argv = vec![
            self.esptool_bin.display().to_string(),
            "--chip".into(),
            self.flags.chip.clone(),
            "--port".into(),
            port.into(),
            "--baud".into(),
            self.flags.baud.to_string(),
            "--before".into(),
            self.flags.before.clone(),
            "--after".into(),
            "no_reset".into(), // AgentDeck: custom post-write pulse, not esptool hard_reset
        ];
        if !self.flags.stub {
            argv.push("--no-stub".into());
        }
        argv.extend([
            "write-flash".into(),
            "--flash-mode".into(),
            self.flags.flash_mode.clone(),
            "--flash-freq".into(),
            self.flags.flash_freq.clone(),
            "--flash-size".into(),
            self.flags.flash_size.clone(),
            "0x0".into(),
            self.image_path.display().to_string(),
        ]);
        argv
    }

    pub fn validate_image_present(&self) -> Result<(), String> {
        if !self.image_path.is_file() {
            return Err(format!("merged image missing: {}", self.image_path.display()));
        }
        Ok(())
    }
}

impl Flasher for EspToolFlasher {
    fn flash(&mut self, plan: &InstallPlan) -> Result<(), String> {
        self.expected_device_id = plan.expected_device_id.clone();
        self.reset_port = Some(plan.port.clone());
        if plan.port.starts_with("sim://") {
            return Err("EspToolFlasher refuses sim:// ports; use SimFlasher".into());
        }
        self.validate_image_present()?;
        let argv = self.build_write_argv(&plan.port);
        self.last_argv = argv.clone();
        if self.dry_run {
            return Ok(());
        }
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .status()
            .map_err(|e| format!("failed to spawn esptool: {e}"))?;
        if !status.success() {
            return Err(format!("esptool exited with {status}"));
        }
        Ok(())
    }

    fn reset(&mut self) -> Result<(), String> {
        self.reset_count = self.reset_count.saturating_add(1);
        let seq = post_write_reset_sequence();
        if self.dry_run {
            // Record that the AgentDeck sequence would run.
            self.last_argv.push(format!("# post-write-reset {seq}"));
            return Ok(());
        }
        #[cfg(feature = "serial")]
        {
            use mdc_transport::{SerialConfig, SerialTransport};
            use std::time::Duration;
            let port = self
                .reset_port
                .clone()
                .ok_or_else(|| "reset_port missing".to_string())?;
            let mut serial = SerialTransport::open_with_config(
                &port,
                SerialConfig {
                    baud: 115_200,
                    timeout: Duration::from_millis(100),
                    reset_on_open: false,
                },
            )
            .map_err(|e| e.to_string())?;
            serial
                .apply_post_write_reset()
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        #[cfg(not(feature = "serial"))]
        {
            Err("live reset requires the serial feature".into())
        }
    }

    fn read_device_id(&mut self) -> Result<String, String> {
        if let Some(id) = &self.identity_override {
            return Ok(id.clone());
        }
        if self.dry_run {
            if self.expected_device_id.is_empty() {
                return Err("dry_run identity requires expected_device_id".into());
            }
            return Ok(self.expected_device_id.clone());
        }
        Err("live identity readback not wired; set identity_override or use SimFlasher".into())
    }
}

/// Resolve a board id to EspToolBoardFlags from known SSOT names.
pub fn flags_for_board(board_id: &str) -> Option<EspToolBoardFlags> {
    match board_id {
        "jc3248w535" | "esp32-jc3248w535-sim" | "ips_35" | "ips35" => {
            Some(EspToolBoardFlags::default())
        }
        _ => None,
    }
}

pub fn require_merged_image(path: &Path) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !name.contains("merged") && !name.ends_with(".bin") {
        return Err("expected a merged .bin image (AgentDeck writes merged@0x0)".into());
    }
    if !path.is_file() {
        return Err(format!("image not found: {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdc_provision::{Artifact, Installer, PortLeases};
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn temp_image() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mdc-merged-{}-{}.bin",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, b"MERGED").unwrap();
        path
    }

    #[test]
    fn argv_is_stubless_merged_at_zero() {
        let image = temp_image();
        let flasher = EspToolFlasher::new(&image);
        let argv = flasher.build_write_argv("/dev/cu.usbmodem1");
        let joined = argv.join(" ");
        assert!(joined.contains("--no-stub"));
        assert!(joined.contains("--before no_reset"));
        assert!(joined.contains("0x0"));
        assert!(joined.contains(image.file_name().unwrap().to_str().unwrap()));
        assert!(joined.contains("--after no_reset")); // custom pulse after
        let _ = std::fs::remove_file(image);
    }

    #[test]
    fn dry_run_install_with_identity() {
        let image = temp_image();
        let mut flasher = EspToolFlasher::new(&image);
        flasher.dry_run = true;
        let plan = InstallPlan {
            port: "/dev/cu.usbmodem1".into(),
            artifact: Artifact {
                board_id: "jc3248w535".into(),
                mcu: "esp32-s3".into(),
                runtime: "0.1.0".into(),
                protocol_major: 1,
                protocol_minor: 0,
                image_size: 6,
                sha256: "0".repeat(64),
                source: "test".into(),
            },
            expected_device_id: "device-a".into(),
        };
        let mut installer = Installer::default();
        let mut leases = PortLeases::default();
        assert!(installer.run(&mut leases, &plan, &mut flasher).is_ok());
        assert!(!flasher.last_argv.is_empty());
        assert_eq!(flasher.reset_count, 1);
        let _ = std::fs::remove_file(image);
    }

    #[test]
    fn flags_for_ips35_board_ids() {
        assert!(flags_for_board("ips35").is_some());
        assert!(flags_for_board("unknown-board").is_none());
    }
}
