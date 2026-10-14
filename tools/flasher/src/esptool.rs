//! EspTool flasher adapter (AgentDeck flash semantics, reimplemented).
//!
//! Writes one merged image at offset 0x0. For ips_35 / JC3248W535 class boards:
//! `--before no_reset --no-stub`, upload baud 460800, flash mode dio / 80m / 16MB.
//! Post-write uses `D0|R1|W100|R0` (not esptool hard_reset alone).
//!
//! `dry_run` builds argv without invoking esptool — unit tests stay offline.

use crate::post_write_reset_sequence;
use mdc_provision::{Flasher, InstallPlan};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;

/// esptool version this project pins (see `tools/flasher/esptool.lock`).
pub const REQUIRED_ESPTOOL_VERSION: &str = "4.7.0";

/// Output of one external command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// How the flasher runs esptool. The production runner spawns a process; tests
/// script outputs so no binary is required.
pub trait Runner: std::fmt::Debug {
    fn run(&mut self, argv: &[String]) -> Result<RunnerOutput, String>;
}

/// Spawns the real command.
#[derive(Debug, Default)]
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&mut self, argv: &[String]) -> Result<RunnerOutput, String> {
        let output = Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .map_err(|e| format!("failed to spawn {}: {e}", argv[0]))?;
        Ok(RunnerOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Scripted runner for offline tests.
#[derive(Debug, Default)]
pub struct FakeRunner {
    outputs: VecDeque<RunnerOutput>,
    pub calls: Vec<Vec<String>>,
}

impl FakeRunner {
    pub fn with_outputs(outputs: Vec<RunnerOutput>) -> Self {
        Self {
            outputs: outputs.into(),
            calls: Vec::new(),
        }
    }
    pub fn push(&mut self, output: RunnerOutput) {
        self.outputs.push_back(output);
    }
}

impl Runner for FakeRunner {
    fn run(&mut self, argv: &[String]) -> Result<RunnerOutput, String> {
        self.calls.push(argv.to_vec());
        self.outputs
            .pop_front()
            .ok_or_else(|| "FakeRunner has no scripted output".to_string())
    }
}

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

#[derive(Debug)]
pub struct EspToolFlasher {
    pub esptool_bin: PathBuf,
    pub image_path: PathBuf,
    pub flags: EspToolBoardFlags,
    /// When true, never spawn esptool; record planned argv instead.
    pub dry_run: bool,
    pub last_argv: Vec<String>,
    pub reset_count: u32,
    pub run_count: u32,
    pub expected_device_id: String,
    pub identity_override: Option<String>,
    /// Port used for post-write DTR/RTS pulse when not dry_run.
    pub reset_port: Option<String>,
    /// N44: check the pinned esptool version before a live write.
    pub verify_version_on_flash: bool,
    runner: Box<dyn Runner>,
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
            run_count: 0,
            expected_device_id: String::new(),
            identity_override: None,
            reset_port: None,
            verify_version_on_flash: true,
            runner: Box::new(ProcessRunner),
        }
    }

    pub fn with_live_esptool(mut self, bin: impl Into<PathBuf>) -> Self {
        self.esptool_bin = bin.into();
        self.dry_run = false;
        self
    }

    /// Replace the command runner (tests inject a [`FakeRunner`]).
    pub fn with_runner(mut self, runner: Box<dyn Runner>) -> Self {
        self.runner = runner;
        self
    }

    /// Run `esptool version` and check it matches [`REQUIRED_ESPTOOL_VERSION`].
    pub fn verify_esptool_version(&mut self) -> Result<String, String> {
        let argv = vec![
            self.esptool_bin.display().to_string(),
            "version".to_string(),
        ];
        let out = self.runner.run(&argv)?;
        self.run_count = self.run_count.saturating_add(1);
        if out.status != 0 {
            return Err(format!("esptool version check exited {}", out.status));
        }
        let text = format!("{}\n{}", out.stdout, out.stderr);
        if !text.contains(REQUIRED_ESPTOOL_VERSION) {
            return Err(format!(
                "esptool {REQUIRED_ESPTOOL_VERSION} required, got: {}",
                text.trim()
            ));
        }
        Ok(text.trim().to_string())
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
            return Err(format!(
                "merged image missing: {}",
                self.image_path.display()
            ));
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
        if self.dry_run {
            self.last_argv = argv;
            return Ok(());
        }
        // N44: never write with an unpinned esptool version.
        if self.verify_version_on_flash {
            self.verify_esptool_version()?;
        }
        self.last_argv = argv.clone();
        let out = self.runner.run(&argv)?;
        self.run_count = self.run_count.saturating_add(1);
        if out.status != 0 {
            return Err(format!(
                "esptool exited with {}: {}",
                out.status,
                out.stderr.trim()
            ));
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
            serial.apply_post_write_reset().map_err(|e| e.to_string())?;
            Ok(())
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
                public_key: Vec::new(),
                signature: Vec::new(),
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

    fn plan() -> InstallPlan {
        InstallPlan {
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
                public_key: Vec::new(),
                signature: Vec::new(),
            },
            expected_device_id: "device-a".into(),
        }
    }

    fn output(status: i32, stdout: &str, stderr: &str) -> RunnerOutput {
        RunnerOutput {
            status,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn fake_runner_drives_flash_and_reports_failures() {
        let image = temp_image();
        let ok = FakeRunner::with_outputs(vec![output(0, "", "")]);
        let mut flasher = EspToolFlasher::new(&image).with_runner(Box::new(ok));
        flasher.dry_run = false;
        flasher.verify_version_on_flash = false; // this test isolates the write path
        flasher.flash(&plan()).unwrap();
        assert_eq!(flasher.run_count, 1);
        assert!(flasher.last_argv.join(" ").contains("write-flash"));

        let bad = FakeRunner::with_outputs(vec![output(2, "", "boom")]);
        let mut flasher = EspToolFlasher::new(&image).with_runner(Box::new(bad));
        flasher.dry_run = false;
        flasher.verify_version_on_flash = false;
        let err = flasher.flash(&plan()).unwrap_err();
        assert!(err.contains("boom"), "{err}");

        let _ = std::fs::remove_file(image);
    }

    #[test]
    fn version_lock_accepts_and_rejects() {
        let image = temp_image();
        let mut ok =
            EspToolFlasher::new(&image).with_runner(Box::new(FakeRunner::with_outputs(vec![
                output(0, "esptool.py v4.7.0\n", ""),
            ])));
        assert!(ok.verify_esptool_version().is_ok());

        let mut bad =
            EspToolFlasher::new(&image).with_runner(Box::new(FakeRunner::with_outputs(vec![
                output(0, "esptool.py v4.6.2\n", ""),
            ])));
        assert!(bad.verify_esptool_version().unwrap_err().contains("4.7.0"));

        let mut failed =
            EspToolFlasher::new(&image).with_runner(Box::new(FakeRunner::with_outputs(vec![
                output(1, "", "no such file"),
            ])));
        assert!(failed
            .verify_esptool_version()
            .unwrap_err()
            .contains("exited 1"));

        let _ = std::fs::remove_file(image);
    }

    #[test]
    fn live_flash_enforces_the_version_lock_before_writing() {
        let image = temp_image();
        // Good version, then a successful write.
        let runner = FakeRunner::with_outputs(vec![
            output(0, "esptool.py v4.7.0\n", ""),
            output(0, "", ""),
        ]);
        let mut flasher = EspToolFlasher::new(&image).with_runner(Box::new(runner));
        flasher.dry_run = false;
        flasher.flash(&plan()).unwrap();
        assert_eq!(flasher.run_count, 2, "version check + write");

        // Wrong version refuses before any write argv is recorded.
        let runner = FakeRunner::with_outputs(vec![output(0, "esptool.py v4.6.0\n", "")]);
        let mut flasher = EspToolFlasher::new(&image).with_runner(Box::new(runner));
        flasher.dry_run = false;
        let error = flasher.flash(&plan()).unwrap_err();
        assert!(error.contains("4.7.0"), "{error}");
        assert!(flasher.last_argv.is_empty(), "no write was attempted");

        let _ = std::fs::remove_file(image);
    }
}
