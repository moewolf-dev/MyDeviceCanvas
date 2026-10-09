#include "mdc_runtime.hpp"
#include <algorithm>

namespace mdc {
namespace {
std::uint32_t checksum(const std::uint8_t* p, std::size_t n) {
  std::uint32_t h = 0x811c9dc5u;
  for (std::size_t i = 0; i < n; ++i) h = (h * 16777619u) ^ p[i];
  return h;
}
std::uint32_t u32(const std::uint8_t* p) {
  return static_cast<std::uint32_t>(p[0]) | (static_cast<std::uint32_t>(p[1]) << 8) |
         (static_cast<std::uint32_t>(p[2]) << 16) | (static_cast<std::uint32_t>(p[3]) << 24);
}
bool known_type(std::uint8_t type) {
  switch (type) {
    case 1:
    case 2:
    case 0x10:
    case 0x11:
    case 0x20:
    case 0x30:
    case 0x40:
    case 0x41:
    case 0x50:
    case 0x7f:
      return true;
    default:
      return false;
  }
}
}  // namespace

ParseResult Parser::feed(const std::uint8_t* bytes, std::size_t length, Packet& output) {
  if (length > 0) buffer_.insert(buffer_.end(), bytes, bytes + length);
  while (buffer_.size() >= kHeaderSize) {
    if (buffer_[0] != 'M' || buffer_[1] != 'D') {
      buffer_.erase(buffer_.begin());
      continue;
    }
    if (buffer_[2] != 1) {
      buffer_.clear();
      return ParseResult::Invalid;
    }
    const auto type = buffer_[4];
    if (!known_type(type)) {
      buffer_.clear();
      return ParseResult::Invalid;
    }
    const std::size_t size = u32(buffer_.data() + 12);
    if (size > max_payload_) {
      buffer_.clear();
      return ParseResult::Invalid;
    }
    if (size > static_cast<std::size_t>(-1) - kHeaderSize) {
      buffer_.clear();
      return ParseResult::Invalid;
    }
    const std::size_t total = kHeaderSize + size;
    if (buffer_.size() < total) return ParseResult::NeedMore;
    if (checksum(buffer_.data() + kHeaderSize, size) != u32(buffer_.data() + 16)) {
      buffer_.erase(buffer_.begin(), buffer_.begin() + static_cast<std::ptrdiff_t>(total));
      return ParseResult::Invalid;
    }
    output.type = static_cast<MessageType>(type);
    output.flags = static_cast<std::uint16_t>(buffer_[6] | (buffer_[7] << 8));
    output.request_id = u32(buffer_.data() + 8);
    output.payload.assign(buffer_.begin() + kHeaderSize, buffer_.begin() + total);
    buffer_.erase(buffer_.begin(), buffer_.begin() + static_cast<std::ptrdiff_t>(total));
    return ParseResult::Packet;
  }
  return ParseResult::NeedMore;
}

std::vector<std::uint8_t> encode_packet(MessageType type, std::uint32_t request_id,
                                        const std::vector<std::uint8_t>& payload) {
  std::vector<std::uint8_t> out(kHeaderSize + payload.size());
  out[0] = 'M';
  out[1] = 'D';
  out[2] = 1;
  out[3] = 0;
  out[4] = static_cast<std::uint8_t>(type);
  out[5] = 0;
  out[6] = 0;
  out[7] = 0;
  out[8] = static_cast<std::uint8_t>(request_id);
  out[9] = static_cast<std::uint8_t>(request_id >> 8);
  out[10] = static_cast<std::uint8_t>(request_id >> 16);
  out[11] = static_cast<std::uint8_t>(request_id >> 24);
  const auto len = static_cast<std::uint32_t>(payload.size());
  out[12] = static_cast<std::uint8_t>(len);
  out[13] = static_cast<std::uint8_t>(len >> 8);
  out[14] = static_cast<std::uint8_t>(len >> 16);
  out[15] = static_cast<std::uint8_t>(len >> 24);
  const auto sum = checksum(payload.data(), payload.size());
  out[16] = static_cast<std::uint8_t>(sum);
  out[17] = static_cast<std::uint8_t>(sum >> 8);
  out[18] = static_cast<std::uint8_t>(sum >> 16);
  out[19] = static_cast<std::uint8_t>(sum >> 24);
  std::copy(payload.begin(), payload.end(), out.begin() + kHeaderSize);
  return out;
}
}  // namespace mdc
