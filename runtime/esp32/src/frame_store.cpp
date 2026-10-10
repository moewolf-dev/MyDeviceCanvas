#include "mdc_runtime.hpp"
#include <algorithm>
#include <limits>

namespace mdc {
FrameStore::FrameStore(std::uint16_t width, std::uint16_t height) {
  const std::size_t pixels = static_cast<std::size_t>(width) * height;
  if (pixels <= std::numeric_limits<std::size_t>::max() / 2 && pixels * 2 <= kMaxPayload) {
    active_.assign(pixels * 2, 0);
    staging_.assign(pixels * 2, 0);
  }
}
bool FrameStore::resize(std::uint16_t width, std::uint16_t height) {
  *this = FrameStore(width, height);
  return !active_.empty();
}
bool FrameStore::stage(const std::uint8_t* bytes, std::size_t length) {
  if (staging_.empty() || length != staging_.size()) return false;
  std::copy(bytes, bytes + length, staging_.begin());
  return true;
}
bool FrameStore::commit() {
  if (staging_.empty()) return false;
  active_.swap(staging_);
  return true;
}
}  // namespace mdc
