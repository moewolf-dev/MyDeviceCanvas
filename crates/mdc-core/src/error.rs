//! Stable Core error taxonomy.
//!
//! The SDK maps these variants to its own error codes; business code must not
//! match on message strings.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    #[error("unsupported pixel format: {0}")]
    Format(String),
    #[error("frame dimensions do not match surface")]
    Dimensions,
    #[error("frame is too large")]
    TooLarge,
    #[error("host copy budget exceeded")]
    CopyBudget,
    #[error("tile is outside surface")]
    TileBounds,
    #[error("queue is full")]
    QueueFull,
    #[error("control queue is full")]
    ControlQueueFull,
    #[error("short write; resume the same frame before sending another")]
    ShortWrite,
    #[error("feature is not supported by device")]
    Unsupported,
    #[error("transport error: {0}")]
    Transport(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("device major version is incompatible")]
    VersionMismatch,
    #[error("session is not ready")]
    NotReady,
    #[error("ack timeout")]
    AckTimeout,
    #[error("heartbeat missed")]
    HeartbeatMissed,
    #[error("connect timeout")]
    ConnectTimeout,
    #[error("network endpoint is not paired")]
    NotPaired,
    #[error("full frame required after reconnect or endpoint switch before tiles")]
    NeedFullFrame,
    /// N329: the frame-pacing budget refused this frame; retry on a later tick.
    #[error("frame throttled by the pacing budget")]
    Throttled,
}
