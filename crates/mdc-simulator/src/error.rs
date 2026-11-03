//! Simulator error taxonomy.
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SimError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("disconnected")]
    Disconnected,
    #[error("base frame mismatch")]
    BaseMismatch,
}

/// Virtual time counter (no wall clock).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtualClock {
    pub tick: u64,
}
impl VirtualClock {
    pub fn advance(&mut self, by: u64) {
        self.tick = self.tick.saturating_add(by);
    }
}

/// Optional fault injection for simulator peer tests.
#[derive(Debug, Clone, Default)]
pub struct FaultInjector {
    pub drop_next: bool,
    /// 先放行这么多个包，再丢掉接下来的一个，并清掉未完成的分块重组。
    pub drop_skip: u32,
    pub armed_drop: bool,
    /// SIM-R08: silently drop one packet after `silent_skip` accepted packets.
    /// Unlike `armed_drop` this sends no Error, simulating a lost request/ACK.
    pub silent_skip: u32,
    pub armed_silent: bool,
    /// Defer processing for this many virtual ticks after enqueue.
    pub delay_ticks: u64,
    pub disconnect: bool,
}
impl FaultInjector {
    pub fn drop_next(&mut self) {
        self.drop_next = true;
    }
    /// `accept` 个包正常处理，然后丢掉下一个包。用来丢掉分块帧的 commit。
    pub fn drop_one_after(&mut self, accept: u32) {
        self.drop_skip = accept;
        self.armed_drop = true;
    }
    /// Accept `accept` packets, then silently drop the next one.
    pub fn drop_silent_after(&mut self, accept: u32) {
        self.silent_skip = accept;
        self.armed_silent = true;
    }
    pub fn delay(&mut self, ticks: u64) {
        self.delay_ticks = ticks;
    }
    pub fn request_disconnect(&mut self) {
        self.disconnect = true;
    }
}
