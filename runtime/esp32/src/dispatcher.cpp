#include "mdc_runtime.hpp"
#include <cstring>

namespace mdc {
namespace {
std::uint64_t u64le(const std::uint8_t* p) {
  std::uint64_t v = 0;
  for (int i = 0; i < 8; ++i) v |= (static_cast<std::uint64_t>(p[i]) << (8 * i));
  return v;
}
std::uint16_t u16le(const std::uint8_t* p) {
  return static_cast<std::uint16_t>(p[0] | (p[1] << 8));
}
std::uint32_t u32le(const std::uint8_t* p) {
  return static_cast<std::uint32_t>(p[0] | (p[1] << 8) | (p[2] << 16) | (p[3] << 24));
}
}  // namespace

SessionDispatcher::SessionDispatcher(DisplayBackend& display, ReplyFn reply)
    : display_(display), reply_(std::move(reply)), store_(display.width(), display.height()) {}

bool SessionDispatcher::handle(const Packet& packet) {
  if (transport_busy_ && packet.type != MessageType::Ping) return false;
  switch (packet.type) {
    case MessageType::Ping:
      send_pong(packet.request_id);
      return true;
    case MessageType::Hello: {
      // Minimal CBOR-free capabilities stub: empty payload ack via Pong-like Capabilities marker.
      // Host tests use Rust FakeDevice; this path answers Ping/Frame for C++ host tests.
      Packet caps;
      caps.type = MessageType::Capabilities;
      caps.flags = 0;
      caps.request_id = packet.request_id;
      caps.payload = {'s', 'i', 'm'};
      reply_(caps);
      return true;
    }
    case MessageType::Frame:
      return handle_frame(packet);
    case MessageType::Tile:
      return handle_tile(packet);
    default:
      return false;
  }
}

bool SessionDispatcher::handle_frame(const Packet& packet) {
  if (packet.payload.empty()) return false;
  const auto phase = packet.payload[0];
  if (phase == 0) {
    // legacy: id_len + id + frame_id + w + h + pixels
    if (packet.payload.size() < 1 + 1 + 8 + 2 + 2) return false;
    const auto id_len = packet.payload[1];
    const std::size_t header = 2 + id_len + 8 + 2 + 2;
    if (packet.payload.size() < header) return false;
    const auto* p = packet.payload.data() + 2 + id_len;
    current_frame_id_ = u64le(p);
    const auto w = u16le(p + 8);
    const auto h = u16le(p + 10);
    const auto* pixels = p + 12;
    const auto pix_len = packet.payload.size() - header;
    if (w != display_.width() || h != display_.height()) return false;
    if (!store_.stage(pixels, pix_len)) return false;
    if (!store_.commit()) return false;
    if (!display_.flush_rect(0, 0, w, h, store_.active().data(), store_.active().size())) return false;
    if (!display_.present()) return false;
    send_ack(packet.request_id, current_frame_id_, true);
    return true;
  }
  if (phase == 1) {
    // begin
    if (packet.payload.size() < 2) return false;
    const auto id_len = packet.payload[1];
    const std::size_t need = 2 + id_len + 8 + 2 + 2 + 4;
    if (packet.payload.size() < need) return false;
    const auto* p = packet.payload.data() + 2 + id_len;
    current_frame_id_ = u64le(p);
    chunk_expected_ = u32le(p + 12);
    chunk_staging_.assign(chunk_expected_, 0);
    return true;
  }
  if (phase == 2) {
    if (packet.payload.size() < 1 + 8 + 4) return false;
    const auto* p = packet.payload.data() + 1;
    const auto frame_id = u64le(p);
    const auto offset = u32le(p + 8);
    const auto* data = p + 12;
    const auto data_len = packet.payload.size() - (1 + 8 + 4);
    if (frame_id != current_frame_id_) return false;
    if (static_cast<std::size_t>(offset) + data_len > chunk_staging_.size()) return false;
    std::memcpy(chunk_staging_.data() + offset, data, data_len);
    return true;
  }
  if (phase == 3) {
    if (packet.payload.size() < 1 + 8) return false;
    const auto frame_id = u64le(packet.payload.data() + 1);
    if (frame_id != current_frame_id_) return false;
    if (!store_.stage(chunk_staging_.data(), chunk_staging_.size())) return false;
    if (!store_.commit()) return false;
    if (!display_.flush_rect(0, 0, display_.width(), display_.height(), store_.active().data(),
                             store_.active().size()))
      return false;
    if (!display_.present()) return false;
    send_ack(packet.request_id, current_frame_id_, true);
    chunk_staging_.clear();
    return true;
  }
  return false;
}

bool SessionDispatcher::handle_tile(const Packet& packet) {
  // surface_id_len + id + base_frame_id + x + y + w + h + pixels
  if (packet.payload.size() < 1) return false;
  const auto id_len = packet.payload[0];
  const std::size_t header = 1 + id_len + 8 + 2 + 2 + 2 + 2;
  if (packet.payload.size() < header) return false;
  const auto* p = packet.payload.data() + 1 + id_len;
  const auto base = u64le(p);
  if (base != current_frame_id_) return false;
  const auto x = u16le(p + 8);
  const auto y = u16le(p + 10);
  const auto w = u16le(p + 12);
  const auto h = u16le(p + 14);
  const auto* pixels = p + 16;
  const auto pix_len = packet.payload.size() - header;
  if (!display_.flush_rect(x, y, w, h, pixels, pix_len)) return false;
  if (!display_.present()) return false;
  send_ack(packet.request_id, current_frame_id_, true);
  return true;
}

void SessionDispatcher::send_ack(std::uint32_t request_id, std::uint64_t frame_id, bool displayed) {
  // Minimal CBOR map-free marker payload understood by host tests as opaque bytes.
  std::vector<std::uint8_t> payload(10);
  payload[0] = displayed ? 1 : 0;
  payload[1] = 1;  // received
  for (int i = 0; i < 8; ++i) payload[2 + i] = static_cast<std::uint8_t>(frame_id >> (8 * i));
  Packet ack;
  ack.type = MessageType::Ack;
  ack.flags = 0;
  ack.request_id = request_id;
  ack.payload = std::move(payload);
  reply_(ack);
}

void SessionDispatcher::send_pong(std::uint32_t request_id) {
  Packet pong;
  pong.type = MessageType::Pong;
  pong.flags = 0;
  pong.request_id = request_id;
  reply_(pong);
}

}  // namespace mdc
