//! Simulator flasher: fake write + identity readout (no serial hardware).
use mdc_provision::{Flasher, InstallPlan};

/// Records flash plans in memory and returns a configured device id on readback.
#[derive(Debug, Clone)]
pub struct SimFlasher {
    pub device_id: String,
    pub written: Vec<u8>,
    pub reset_count: u32,
    pub last_plan: Option<InstallPlan>,
    fail_flash: bool,
}
impl Default for SimFlasher {
    fn default() -> Self {
        Self {
            device_id: "sim-esp32-jc3248w535".into(),
            written: Vec::new(),
            reset_count: 0,
            last_plan: None,
            fail_flash: false,
        }
    }
}
impl SimFlasher {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            ..Self::default()
        }
    }
    pub fn fail_next_flash(&mut self) {
        self.fail_flash = true;
    }
}
impl Flasher for SimFlasher {
    fn flash(&mut self, plan: &InstallPlan) -> Result<(), String> {
        if self.fail_flash {
            self.fail_flash = false;
            return Err("sim flash injected failure".into());
        }
        // Fake write: pretend artifact bytes equal image_size of zeros.
        let size = plan.artifact.image_size.min(4 * 1024 * 1024) as usize;
        self.written = vec![0u8; size];
        self.last_plan = Some(plan.clone());
        Ok(())
    }
    fn reset(&mut self) -> Result<(), String> {
        self.reset_count = self.reset_count.saturating_add(1);
        Ok(())
    }
    fn read_device_id(&mut self) -> Result<String, String> {
        Ok(self.device_id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdc_provision::{Artifact, Installer, InstallState, PortLeases};

    #[test]
    fn sim_flasher_install_succeeds() {
        let plan = InstallPlan {
            port: "sim:///dev/null".into(),
            artifact: Artifact {
                board_id: "esp32-jc3248w535-sim".into(),
                mcu: "esp32-s3".into(),
                runtime: "0.1.0".into(),
                protocol_major: 1,
                protocol_minor: 0,
                image_size: 64,
                sha256: "0".repeat(64),
                source: "sim".into(),
            },
            expected_device_id: "sim-esp32-jc3248w535".into(),
        };
        let mut flasher = SimFlasher::new("sim-esp32-jc3248w535");
        let mut installer = Installer::default();
        let mut leases = PortLeases::default();
        assert!(installer.run(&mut leases, &plan, &mut flasher).is_ok());
        assert_eq!(installer.state, InstallState::Succeeded);
        assert_eq!(flasher.written.len(), 64);
        assert_eq!(flasher.reset_count, 1);
    }
}
