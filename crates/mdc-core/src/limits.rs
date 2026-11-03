//! Host-side resource limits used by the session, queues and tests.
//!
//! These are intentionally public: the SDK and the contract tests must use the
//! same numbers instead of duplicating literals. Protocol-level defaults stay in
//! `mdc-protocol`; this module only owns Core/accounting limits.

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// SIM-R07: one poll reads at most this many bytes and handles at most this
/// many packets, so a burst cannot starve timers or cancellation.
pub const MAX_POLL_BYTES: usize = 64 * 1024;
pub const MAX_POLL_PACKETS: usize = 32;
/// Stop reading once the unparsed buffer reaches this size. A transport read
/// that delivers more is parsed over subsequent polls, not read again.
pub const MAX_RX_BYTES: usize = MAX_FRAME_BYTES * 2;
pub const MAX_MANAGER_EVENTS: usize = 128;
/// SIM-R07: dedup history is a sliding window, not a per-frame log.
pub const MAX_FINISHED_ACKS: usize = 4096;
pub const MAX_DISPLAY_TIMINGS: usize = 256;
/// SIM-R07: an input flood cannot allocate an unbounded pointer map.
pub const MAX_ACTIVE_POINTERS: usize = 16;
/// SIM-R07: chunk coverage lists are capped; a later chunk starts a new BEGIN.
pub const MAX_FRAME_RANGES: usize = 256;
pub const MAX_CONTROL_IN_FLIGHT: usize = 32;
/// N40: longest configuration value the host will send (bytes).
pub const MAX_CONFIG_VALUE_BYTES: usize = 256;
/// N316: bounded host-side diagnostic queue (metrics + boot info).
pub const MAX_DIAGNOSTICS: usize = 64;
