//! Display geometry helpers.
//! Packed-buffer rules are project-owned; sizing ideas were informed by FrameOS
//! design notes (AGPL — no FrameOS source is included). AgentDeck board geometry
//! (480×320 RGB565) is the 0.1 default surface.

use crate::CodecError;

/// RGB565 tightly packed row bytes for a rectangle (checked).
pub fn rgb565_bytes(width: u16, height: u16) -> Result<usize, CodecError> {
    usize::from(width)
        .checked_mul(usize::from(height))
        .and_then(|n| n.checked_mul(2))
        .ok_or(CodecError::Length(0))
}

/// Bytes-per-pixel for a host/runtime canvas given PSRAM capacity.
///
/// Rule (reimplemented): if `width*height*4 * share <= psram_total` use 4 B/px
/// (RGBA), else 2 B/px (RGB565). `share` defaults to 2 (canvas ≤ half of PSRAM).
/// FrameOS used the same half-PSRAM idea for Nim/JS stacks; MDC does not copy
/// FrameOS's 1536 KiB render reserve.
pub fn canvas_bytes_per_pixel(width: u16, height: u16, psram_total: u64, share: u64) -> u8 {
    let share = share.max(1);
    let rgbx = u64::from(width).saturating_mul(u64::from(height)).saturating_mul(4);
    if psram_total > 0 && rgbx.saturating_mul(share) <= psram_total {
        4
    } else {
        2
    }
}

/// Dual full-screen RGB565 budget (active + staging).
pub fn dual_rgb565_budget(width: u16, height: u16) -> Result<usize, CodecError> {
    rgb565_bytes(width, height)?.checked_mul(2).ok_or(CodecError::Length(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb565_and_dual_budget_for_ips35() {
        assert_eq!(rgb565_bytes(480, 320).unwrap(), 307_200);
        assert_eq!(dual_rgb565_budget(480, 320).unwrap(), 614_400);
    }

    #[test]
    fn canvas_bpp_half_psram_gate() {
        // 480×320×4 = 614400; ×2 share = 1_228_800 — fits in 8 MiB → 4 bpp
        assert_eq!(canvas_bytes_per_pixel(480, 320, 8 * 1024 * 1024, 2), 4);
        // Huge panel that does not fit half of 8 MiB as RGBA → 2 bpp
        assert_eq!(canvas_bytes_per_pixel(1200, 1600, 8 * 1024 * 1024, 2), 2);
        // No PSRAM → always 2
        assert_eq!(canvas_bytes_per_pixel(480, 320, 0, 2), 2);
    }
}
