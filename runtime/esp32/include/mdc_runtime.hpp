#pragma once
#include "mdc_cbor.hpp"
#include <cstddef>
#include <cstdint>
#include <functional>
#include <string>
#include <vector>

namespace mdc {

constexpr std::size_t kHeaderSize = 20;
constexpr std::size_t kMaxPayload = 1024 * 1024;

enum class MessageType : std::uint8_t {
  Hello = 1,
  Capabilities = 2,
  Frame = 0x10,
  Tile = 0x11,
  Input = 0x20,
  Ota = 0x30,
  Ping = 0x40,
  Pong = 0x41,
  Ack = 0x50,
  Error = 0x7f,
};

enum class ParseResult { NeedMore, Packet, Invalid };

struct Packet {
  MessageType type;
  std::uint16_t flags;
  std::uint32_t request_id;
  std::vector<std::uint8_t> payload;
};

class Parser {
 public:
  explicit Parser(std::size_t max_payload = kMaxPayload) : max_payload_(max_payload) {}
  ParseResult feed(const std::uint8_t* bytes, std::size_t length, Packet& output);

 private:
  std::vector<std::uint8_t> buffer_;
  std::size_t max_payload_;
};

/// Display HAL shared by SimDisplay and future hardware adapters.
class DisplayBackend {
 public:
  virtual ~DisplayBackend() = default;
  virtual std::uint16_t width() const = 0;
  virtual std::uint16_t height() const = 0;
  virtual bool flush_rect(std::uint16_t x, std::uint16_t y, std::uint16_t w, std::uint16_t h,
                          const std::uint8_t* rgb565, std::size_t length) = 0;
  virtual bool present() = 0;
  virtual const std::vector<std::uint8_t>& pixels() const = 0;
};

class SimDisplay : public DisplayBackend {
 public:
  SimDisplay(std::uint16_t width, std::uint16_t height);
  std::uint16_t width() const override { return width_; }
  std::uint16_t height() const override { return height_; }
  bool flush_rect(std::uint16_t x, std::uint16_t y, std::uint16_t w, std::uint16_t h,
                  const std::uint8_t* rgb565, std::size_t length) override;
  bool present() override;
  const std::vector<std::uint8_t>& pixels() const override { return front_; }

 private:
  std::uint16_t width_;
  std::uint16_t height_;
  std::vector<std::uint8_t> front_;
  std::vector<std::uint8_t> back_;
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

using ReplyFn = std::function<void(const Packet&)>;

/// Dispatches HELLO / Ping / Frame / Tile against a DisplayBackend.
class SessionDispatcher {
 public:
  SessionDispatcher(DisplayBackend& display, ReplyFn reply, PeerIdentity identity = {});
  bool handle(const Packet& packet);
  bool busy() const { return transport_busy_; }
  void set_transport_busy(bool busy) { transport_busy_ = busy; }
  const PeerIdentity& identity() const { return identity_; }

 private:
  DisplayBackend& display_;
  ReplyFn reply_;
  PeerIdentity identity_;
  FrameStore store_;
  bool transport_busy_ = false;
  std::uint64_t current_frame_id_ = 0;
  std::vector<std::uint8_t> chunk_staging_;
  std::uint32_t chunk_expected_ = 0;
  bool handle_frame(const Packet& packet);
  bool handle_tile(const Packet& packet);
  void send_ack(std::uint32_t request_id, std::uint64_t frame_id, bool displayed);
  void send_pong(std::uint32_t request_id);
  void send_capabilities(std::uint32_t request_id);
};

std::vector<std::uint8_t> encode_packet(MessageType type, std::uint32_t request_id,
                                        const std::vector<std::uint8_t>& payload);

}  // namespace mdc
