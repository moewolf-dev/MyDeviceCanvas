#pragma once
#include <cstddef>
#include <cstdint>
#include <vector>

namespace mdc {
constexpr std::size_t kHeaderSize = 20;
constexpr std::size_t kMaxPayload = 1024 * 1024;
enum class MessageType : std::uint8_t { Hello = 1, Capabilities = 2, Frame = 0x10, Tile = 0x11, Ping = 0x40, Pong = 0x41 };
enum class ParseResult { NeedMore, Packet, Invalid };
struct Packet { MessageType type; std::uint32_t request_id; std::vector<std::uint8_t> payload; };

class Parser {
 public:
  explicit Parser(std::size_t max_payload = kMaxPayload) : max_payload_(max_payload) {}
  ParseResult feed(const std::uint8_t* bytes, std::size_t length, Packet& output);
 private:
  std::vector<std::uint8_t> buffer_;
  std::size_t max_payload_;
};

class FrameStore {
 public:
  FrameStore(std::uint16_t width, std::uint16_t height);
  bool stage(const std::uint8_t* bytes, std::size_t length);
  bool commit();
  const std::vector<std::uint8_t>& active() const { return active_; }
 private:
  std::vector<std::uint8_t> active_, staging_;
};
}  // namespace mdc
