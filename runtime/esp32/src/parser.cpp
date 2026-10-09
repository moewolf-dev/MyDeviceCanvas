#include "mdc_runtime.hpp"
#include <algorithm>

namespace mdc {
namespace {
std::uint32_t checksum(const std::uint8_t* p, std::size_t n) {
  std::uint32_t h = 0x811c9dc5u;
  for (std::size_t i = 0; i < n; ++i) h = (h * 16777619u) ^ p[i];
  return h;
}
std::uint32_t u32(const std::uint8_t* p) { return static_cast<std::uint32_t>(p[0]) | (static_cast<std::uint32_t>(p[1]) << 8) | (static_cast<std::uint32_t>(p[2]) << 16) | (static_cast<std::uint32_t>(p[3]) << 24); }
}
ParseResult Parser::feed(const std::uint8_t* bytes, std::size_t length, Packet& output) {
  if (length > 0) buffer_.insert(buffer_.end(), bytes, bytes + length);
  while (buffer_.size() >= kHeaderSize) {
    if (buffer_[0] != 'M' || buffer_[1] != 'D') { buffer_.erase(buffer_.begin()); continue; }
    if (buffer_[2] != 1) { buffer_.clear(); return ParseResult::Invalid; }
    const auto type = buffer_[4];
    if (type != 1 && type != 2 && type != 0x10 && type != 0x11 && type != 0x40 && type != 0x41) { buffer_.clear(); return ParseResult::Invalid; }
    const std::size_t size = u32(buffer_.data() + 12);
    if (size > max_payload_) { buffer_.clear(); return ParseResult::Invalid; }
    if (size > static_cast<std::size_t>(-1) - kHeaderSize) { buffer_.clear(); return ParseResult::Invalid; }
    const std::size_t total = kHeaderSize + size;
    if (buffer_.size() < total) return ParseResult::NeedMore;
    if (checksum(buffer_.data() + kHeaderSize, size) != u32(buffer_.data() + 16)) { buffer_.erase(buffer_.begin(), buffer_.begin() + total); return ParseResult::Invalid; }
    output.type = static_cast<MessageType>(type);
    output.request_id = u32(buffer_.data() + 8);
    output.payload.assign(buffer_.begin() + kHeaderSize, buffer_.begin() + total);
    buffer_.erase(buffer_.begin(), buffer_.begin() + total);
    return ParseResult::Packet;
  }
  return ParseResult::NeedMore;
}
}  // namespace mdc
