#include "mdc_runtime.hpp"
#include <algorithm>
#include <limits>

namespace mdc {
FrameStore::FrameStore(std::uint16_t width, std::uint16_t height) {
  const std::size_t pixels = static_cast<std::size_t>(width) * height;
  if (pixels <= std::numeric_limits<std::size_t>::max() / 2 && pixels * 2 <= kMaxPayload) {
    active_.assign(pixels * 2, 0);
  }
}
bool FrameStore::resize(std::uint16_t width, std::uint16_t height) {
  *this = FrameStore(width, height);
  return !active_.empty();
}
void FrameStore::set_heap(std::size_t free_bytes, std::size_t min_free) {
  free_bytes_ = free_bytes;
  min_free_ = min_free;
}
bool FrameStore::admit(std::size_t bytes) {
  if (bytes > free_bytes_ || free_bytes_ - bytes < min_free_) return false;
  free_bytes_ -= bytes;
  return true;
}
bool FrameStore::stage(const std::uint8_t* bytes, std::size_t length) {
  if (active_.empty() || length != active_.size()) return false;
  if (staging_.size() != length) {
    if (!admit(length)) return false;
    staging_.assign(length, 0);
  }
  std::copy(bytes, bytes + length, staging_.begin());
  return true;
}
bool FrameStore::commit() {
  if (staging_.empty()) return false;
  active_.swap(staging_);
  return true;
}
}  // namespace mdc
