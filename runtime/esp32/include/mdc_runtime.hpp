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
  MirrorFrame = 0x12,
  Input = 0x20,
  Ota = 0x30,
  MirrorCtrl = 0x31,
  /// Host -> device runtime package (N33): phase byte like Frame.
  Package = 0x32,
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
  bool resize(std::uint16_t width, std::uint16_t height);
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
  bool resize(std::uint16_t width, std::uint16_t height);
  bool stage(const std::uint8_t* bytes, std::size_t length);
  bool commit();
  void set_heap(std::size_t free_bytes, std::size_t min_free);
  const std::vector<std::uint8_t>& active() const { return active_; }

 private:
  bool admit(std::size_t bytes);
  std::vector<std::uint8_t> active_, staging_;
  std::size_t free_bytes_ = kMaxPayload;
  std::size_t min_free_ = 0;
};

using ReplyFn = std::function<void(const Packet&)>;

/// Dispatches HELLO / Ping / Frame / Tile against a DisplayBackend.
class SessionDispatcher {
 public:
  SessionDispatcher(DisplayBackend& display, ReplyFn reply, PeerIdentity identity = {});
  bool handle(const Packet& packet);
  void sync_geometry();
  bool busy() const { return transport_busy_; }
  void set_transport_busy(bool busy) { transport_busy_ = busy; }
  const PeerIdentity& identity() const { return identity_; }
  /// Form B: whether the host has started mirroring on this session.
  bool mirror_active() const { return mirror_active_; }
  /// N33: true once a runtime package was accepted (transport checksum + magic).
  bool package_ready() const { return package_ready_; }
  std::size_t package_bytes() const { return package_bytes_; }
  std::uint64_t mirror_frames_sent() const { return mirror_frames_; }
  /// N33: device -> host mirror frame using the legacy Frame layout (phase 0).
  bool emit_mirror_frame();

 private:
  DisplayBackend& display_;
  ReplyFn reply_;
  PeerIdentity identity_;
  FrameStore store_;
  bool transport_busy_ = false;
  std::uint64_t current_frame_id_ = 0;
  std::vector<std::uint8_t> chunk_staging_;
  std::uint32_t chunk_expected_ = 0;
  std::uint32_t chunk_filled_ = 0;
  bool render_frozen_ = false;
  /// Form B: true after a host MirrorCtrl start; cleared by stop.
  bool mirror_active_ = false;
  /// N33: package assembly state.
  std::vector<std::uint8_t> package_staging_;
  std::uint32_t package_expected_ = 0;
  std::uint32_t package_filled_ = 0;
  std::uint64_t package_checksum_ = 0;
  bool package_ready_ = false;
  std::size_t package_bytes_ = 0;
  std::uint64_t mirror_frames_ = 0;
  bool handle_frame(const Packet& packet);
  bool handle_tile(const Packet& packet);
  bool handle_mirror_ctrl(const Packet& packet);
  bool handle_package(const Packet& packet);
  void send_ack(std::uint32_t request_id, std::uint64_t frame_id, bool displayed);
  void send_error(std::uint32_t request_id, const char* code, const char* message);
  void send_pong(std::uint32_t request_id);
  void send_capabilities(std::uint32_t request_id);
};

std::vector<std::uint8_t> encode_packet(MessageType type, std::uint32_t request_id,
                                        const std::vector<std::uint8_t>& payload);

}  // namespace mdc
