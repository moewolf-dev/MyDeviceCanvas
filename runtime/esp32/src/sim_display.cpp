#include "mdc_runtime.hpp"
#include <cstring>

namespace mdc {

SimDisplay::SimDisplay(std::uint16_t width, std::uint16_t height)
    : width_(width), height_(height), front_(static_cast<std::size_t>(width) * height * 2, 0),
      back_(front_) {}

bool SimDisplay::flush_rect(std::uint16_t x, std::uint16_t y, std::uint16_t w, std::uint16_t h,
                            const std::uint8_t* rgb565, std::size_t length) {
  if (x + w > width_ || y + h > height_) return false;
  const std::size_t expected = static_cast<std::size_t>(w) * h * 2;
  if (length != expected || rgb565 == nullptr) return false;
  for (std::uint16_t row = 0; row < h; ++row) {
    const std::size_t dst =
        (static_cast<std::size_t>(y + row) * width_ + x) * 2;
    const std::size_t src = static_cast<std::size_t>(row) * w * 2;
    std::memcpy(back_.data() + dst, rgb565 + src, static_cast<std::size_t>(w) * 2);
  }
  return true;
}

bool SimDisplay::present() {
  front_.swap(back_);
  return true;
}

}  // namespace mdc
