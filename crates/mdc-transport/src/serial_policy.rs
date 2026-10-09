//! Serial open policy reimplemented from AgentDeck bridge experience (MIT reference).
//! Source of algorithm: AgentDeck `bridge/src/esp32-serial.ts` (`serialOpenFailureBackoffMs`,
//! port patterns). No upstream source files are copied.

/// Escalation threshold: failCount >= this uses the 5-minute permanent-style cap for wedges.
pub const SERIAL_OPEN_FAIL_ESCALATION_THRESHOLD: u32 = 4;
/// Cap while failCount is below the escalation threshold (60s).
pub const SERIAL_TRANSIENT_MAX_BACKOFF_MS: u64 = 60_000;
/// Flat block for EACCES / escalated wedge (5 min).
pub const SERIAL_OPEN_PERMANENT_BLOCK_MS: u64 = 300_000;
/// Foreign device denylist after repeated failed identification.
pub const FOREIGN_MAX_PROBE_FAILURES: u32 = 3;
pub const FOREIGN_DENYLIST_COOLDOWN_MS: u64 = 10 * 60_000;

/// Post-flash DTR/RTS pulse sequence from AgentDeck flash path.
/// Format: D{0|1}|R{0|1}|W{ms}|… — raise IO0 (DTR), pulse EN (RTS), wait, release.
pub const POST_WRITE_RESET_SEQUENCE: &str = "D0|R1|W100|R0";

/// Backoff before retrying `open` after a failure.
///
/// Opening a serial port toggles DTR/RTS and can reset the board, so failures must
/// escalate instead of reopening every poll. Cadence (transient):
/// 1→10s, 2→20s, 3→40s, 4→80s (then cap rises to 300s), … permanent → 300s flat.
pub fn serial_open_failure_backoff_ms(fail_count: u32, is_permanent: bool) -> u64 {
    if is_permanent {
        return SERIAL_OPEN_PERMANENT_BLOCK_MS;
    }
    if fail_count == 0 {
        return 0;
    }
    let cap = if fail_count >= SERIAL_OPEN_FAIL_ESCALATION_THRESHOLD {
        SERIAL_OPEN_PERMANENT_BLOCK_MS
    } else {
        SERIAL_TRANSIENT_MAX_BACKOFF_MS
    };
    let exp = fail_count.saturating_sub(1).min(16);
    let raw = 10_000u64.saturating_mul(1u64 << exp);
    raw.min(cap)
}

/// True if the path looks like a candidate ESP32 USB-serial device (not Bluetooth/WLAN).
pub fn is_candidate_serial_port(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    if lower.contains("bluetooth") || lower.contains("wlan") {
        return false;
    }
    lower.contains("usbserial")
        || lower.contains("wchusbserial")
        || lower.contains("usbmodem")
        || lower.contains("ttyusb")
        || lower.contains("ttyacm")
}

/// Tracks consecutive open failures and generation tokens (invalidates in-flight opens).
#[derive(Debug, Default, Clone)]
pub struct SerialOpenGuard {
    pub fail_count: u32,
    pub generation: u64,
    pub permanent: bool,
}
impl SerialOpenGuard {
    pub fn note_failure(&mut self, permanent: bool) -> u64 {
        self.fail_count = self.fail_count.saturating_add(1);
        if permanent {
            self.permanent = true;
        }
        self.backoff_ms()
    }
    pub fn note_success(&mut self) {
        self.fail_count = 0;
        self.permanent = false;
    }
    pub fn bump_generation(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.generation
    }
    pub fn backoff_ms(&self) -> u64 {
        serial_open_failure_backoff_ms(self.fail_count, self.permanent)
    }
    pub fn is_stale(&self, observed_generation: u64) -> bool {
        observed_generation != self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_matches_agentdeck_cadence() {
        assert_eq!(serial_open_failure_backoff_ms(0, false), 0);
        assert_eq!(serial_open_failure_backoff_ms(1, false), 10_000);
        assert_eq!(serial_open_failure_backoff_ms(2, false), 20_000);
        assert_eq!(serial_open_failure_backoff_ms(3, false), 40_000);
        assert_eq!(serial_open_failure_backoff_ms(4, false), 80_000);
        assert_eq!(serial_open_failure_backoff_ms(5, false), 160_000);
        assert_eq!(serial_open_failure_backoff_ms(6, false), 300_000);
        assert_eq!(serial_open_failure_backoff_ms(1, true), 300_000);
    }

    #[test]
    fn candidate_port_filter() {
        assert!(is_candidate_serial_port("/dev/cu.usbmodem834101"));
        assert!(is_candidate_serial_port("/dev/ttyUSB0"));
        assert!(!is_candidate_serial_port("/dev/cu.Bluetooth-Incoming-Port"));
    }

    #[test]
    fn generation_invalidates_inflight() {
        let mut g = SerialOpenGuard::default();
        let gen = g.generation;
        g.bump_generation();
        assert!(g.is_stale(gen));
        assert!(!g.is_stale(g.generation));
    }
}
