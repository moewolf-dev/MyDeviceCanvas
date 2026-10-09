#pragma once
#include <cstddef>
#include <cstdint>

namespace mdc {

/// RGB565 packed bytes for a rectangle (checked). Returns 0 on overflow/illegal.
inline std::size_t rgb565_bytes(std::uint16_t width, std::uint16_t height) {
  const std::size_t w = width;
  const std::size_t h = height;
  if (w == 0 || h == 0) return 0;
  if (w > (static_cast<std::size_t>(-1) / h)) return 0;
  const std::size_t pixels = w * h;
  if (pixels > (static_cast<std::size_t>(-1) / 2)) return 0;
  return pixels * 2;
}

inline std::size_t dual_rgb565_budget(std::uint16_t width, std::uint16_t height) {
  const std::size_t one = rgb565_bytes(width, height);
  if (one == 0 || one > (static_cast<std::size_t>(-1) / 2)) return 0;
  return one * 2;
}

/// Half-PSRAM canvas bpp gate (project-owned; inspired by FrameOS design notes).
inline std::uint8_t canvas_bytes_per_pixel(std::uint16_t width, std::uint16_t height,
                                          std::uint64_t psram_total, std::uint64_t share = 2) {
  if (share == 0) share = 1;
  const std::uint64_t rgbx =
      static_cast<std::uint64_t>(width) * static_cast<std::uint64_t>(height) * 4ull;
  if (psram_total > 0 && rgbx * share <= psram_total) return 4;
  return 2;
}

}  // namespace mdc
