use mdc_protocol::{
    decode_frame_payload, encode_control, encode_frame_begin, encode_frame_chunk,
    encode_frame_commit, encode_frame_payload, encode_package_begin, encode_package_chunk,
    encode_package_commit, encode_tile_payload, negotiate_mirror, package_checksum, Ack, BootInfo,
    Capabilities, ConfigCommand, ConfigReply, ErrorPayload, FramePayload, Hello, InputEvent,
    MessageType, MetricReport, MirrorCommand, OtaCommand, Packet, Surface, VERSION,
};
mod ledger;
pub use ledger::{AllocationLedger, HostCopyBudget, SimHeap};

use mdc_transport::Transport;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

mod limits;
pub use limits::*;

/// N316: one diagnostic record received on the (separate) diagnostics channel.
/// Kept apart from frame/ack bookkeeping so a reboot or a busy image link cannot
/// mix old results into the current run.
#[derive(Debug, Clone, PartialEq)]
pub enum DiagnosticRecord {
    Metric(MetricReport),
    BootInfo(BootInfo),
    /// N335: structured telemetry event.
    Telemetry(mdc_protocol::TelemetryEvent),
    /// N367: a crash/fault report.
    Crash(mdc_protocol::CrashReport),
}

mod error;
pub use error::CoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Handshaking,
    Ready,
    Reconnecting,
    Closing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaState {
    Unsupported,
    Idle,
    Verifying,
    Installing,
    Rebooting,
    Confirming,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchPoint {
    pub surface_id: u16,
    pub pointer_id: u16,
    pub x: u16,
    pub y: u16,
}

pub fn transform_touch(
    width: u16,
    height: u16,
    rotation: u16,
    x: u16,
    y: u16,
) -> Result<(u16, u16), CoreError> {
    if x >= width || y >= height {
        return Err(CoreError::TileBounds);
    }
    match rotation % 360 {
        0 => Ok((x, y)),
        90 => Ok((height - 1 - y, x)),
        180 => Ok((width - 1 - x, height - 1 - y)),
        270 => Ok((y, width - 1 - x)),
        _ => Err(CoreError::Dimensions),
    }
}

pub struct OtaController {
    pub state: OtaState,
    pub render_frozen: bool,
    has_slot: bool,
}
impl OtaController {
    pub fn new(has_slot: bool) -> Self {
        Self {
            state: if has_slot {
                OtaState::Idle
            } else {
                OtaState::Unsupported
            },
            render_frozen: false,
            has_slot,
        }
    }
    pub fn begin(&mut self) -> Result<(), CoreError> {
        if !self.has_slot {
            return Err(CoreError::Unsupported);
        }
        if self.state != OtaState::Idle {
            return Err(CoreError::Transport("OTA is not idle".into()));
        }
        self.state = OtaState::Verifying;
        Ok(())
    }
    pub fn verified(&mut self) -> Result<(), CoreError> {
        if self.state != OtaState::Verifying {
            return Err(CoreError::Transport("OTA image was not verified".into()));
        }
        self.state = OtaState::Installing;
        self.render_frozen = true;
        Ok(())
    }
    pub fn reboot(&mut self) -> Result<(), CoreError> {
        if self.state != OtaState::Installing {
            return Err(CoreError::Transport("OTA install is not active".into()));
        }
        self.state = OtaState::Rebooting;
        Ok(())
    }
    pub fn confirm(&mut self, success: bool) {
        self.state = if success {
            OtaState::Succeeded
        } else {
            OtaState::Failed
        };
        self.render_frozen = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter_ms: u64,
}
impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 1_000,
            max_delay_ms: 30_000,
            jitter_ms: 0,
        }
    }
}
impl ReconnectPolicy {
    pub fn delay_ms(&self, attempt: u32, jitter: u64) -> Option<u64> {
        if attempt == 0 || attempt > self.max_attempts {
            return None;
        }
        let exponent = attempt.saturating_sub(1).min(5);
        let base = self
            .base_delay_ms
            .saturating_mul(1u64 << exponent)
            .min(self.max_delay_ms);
        Some(
            base.saturating_add(jitter.min(self.jitter_ms))
                .min(self.max_delay_ms),
        )
    }
}

/// Timeouts and reconnect knobs for Session::tick (virtual clock friendly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    pub connect_timeout_ms: u64,
    pub ack_timeout_ms: u64,
    pub heartbeat_ms: u64,
    pub reconnect: bool,
}
impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            connect_timeout_ms: 20_000,
            ack_timeout_ms: 5_000,
            heartbeat_ms: 5_000,
            reconnect: true,
        }
    }
}

mod gesture;
pub use gesture::*;

/// N750: the result of one joint streaming tuning pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveTuning {
    pub bitrate: u64,
    pub pacing: Option<mdc_schedule::SchedulerConfig>,
    pub dirty_threshold: Option<f64>,
}

/// N398: host-side session counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionStats {
    pub packets_sent: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub acks_received: u64,
    pub frames_sent: u64,
    /// N436: frames the device dropped (reported by the device).
    pub dropped_frames: u64,
    /// N446: packages installed on the device.
    pub packages_sent: u64,
    /// N446: complete mirror frames received.
    pub mirror_frames: u64,
    /// N446: frames refused by the host pacing budget.
    pub pacing_dropped: u64,
    /// N506: partial (tile) updates sent.
    pub tiles_sent: u64,
    /// N506: bytes covered by the tiles sent.
    pub tile_bytes_sent: u64,
    /// N547: tiles the device reports having applied (from its `tiles_received`
    /// metric). 0 until the device reports a count.
    pub device_tiles_received: u64,
    /// N547: tiles the host sent that the device has not acknowledged.
    pub tiles_missing: u64,
}

/// SIM-R09: bounded container occupancies. A soak samples this to prove that
/// completed transactions do not grow history or input/mirror state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContainerCounts {
    pub rx_bytes: usize,
    pub pending_requests: usize,
    pub pending_control: usize,
    pub pending_frames: usize,
    pub request_deadlines: usize,
    pub finished_acks: usize,
    pub displayed_timings: usize,
    pub held_frames: usize,
    pub diagnostics: usize,
    pub config_cache: usize,
    pub pending_configs: usize,
    pub active_pointers: usize,
    pub mirror_inbox_frames: usize,
    pub mirror_base_frames: usize,
    pub frame_transactions: usize,
    pub manager_events: usize,
    pub devices: usize,
}

/// N547: the metric name a device uses to report applied partial-update tiles.
pub const TILES_RECEIVED_METRIC: &str = "tiles_received";

/// N586: the device config key that negotiates how many unacknowledged tiles the
/// host tolerates before forcing a full frame.
pub const TILES_MISSING_THRESHOLD_CONFIG: &str = "tile_missing_threshold";

mod frame;
pub use frame::*;

mod device;
pub use device::*;

pub struct DisplayTiming {
    pub transport_us: u128,
    pub render_us: u128,
    pub e2e_us: u128,
}

mod session;
pub use session::*;

#[cfg(test)]
mod tests;
