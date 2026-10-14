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
std::uint32_t u32be(const std::uint8_t* p) {
  return (static_cast<std::uint32_t>(p[0]) << 24) | (static_cast<std::uint32_t>(p[1]) << 16) |
         (static_cast<std::uint32_t>(p[2]) << 8) | static_cast<std::uint32_t>(p[3]);
}
std::uint64_t u64be(const std::uint8_t* p) {
  std::uint64_t v = 0;
  for (int i = 0; i < 8; ++i) v = (v << 8) | p[i];
  return v;
}
/// FNV-1a 64-bit, matching mdc-protocol::package_checksum (N22/N33).
std::uint64_t fnv1a64(const std::uint8_t* p, std::size_t n) {
  std::uint64_t h = 0xcbf29ce484222325ull;
  for (std::size_t i = 0; i < n; ++i) {
    h ^= p[i];
    h *= 0x100000001b3ull;
  }
  return h;
}
constexpr std::size_t kMaxPackage = 16u * 1024u * 1024u;
}  // namespace

SessionDispatcher::SessionDispatcher(DisplayBackend& display, ReplyFn reply, PeerIdentity identity)
    : display_(display),
      reply_(std::move(reply)),
      identity_(std::move(identity)),
      store_(display.width(), display.height()) {}

void SessionDispatcher::sync_geometry() {
  if (!store_.resize(display_.width(), display_.height())) {
    store_ = FrameStore(display_.width(), display_.height());
  }
  chunk_staging_.clear();
  chunk_expected_ = 0;
  chunk_filled_ = 0;
  current_frame_id_ = 0;
}

bool SessionDispatcher::handle(const Packet& packet) {
  if (transport_busy_ && packet.type != MessageType::Ping) return false;
  switch (packet.type) {
    case MessageType::Ping:
      send_pong(packet.request_id);
      return true;
    case MessageType::Hello:
      send_capabilities(packet.request_id);
      return true;
    case MessageType::Frame:
      if (render_frozen_) {
        send_error(packet.request_id, "E_FROZEN", "render frozen during ota");
        return true;
      }
      return handle_frame(packet);
    case MessageType::Tile:
      if (render_frozen_) {
        send_error(packet.request_id, "E_FROZEN", "render frozen during ota");
        return true;
      }
      return handle_tile(packet);
    case MessageType::Ota: {
      const std::string text(packet.payload.begin(), packet.payload.end());
      if (text.find("abort") != std::string::npos || text.find("timeout") != std::string::npos) {
        render_frozen_ = false;
      } else if (text.find("begin") != std::string::npos) {
        render_frozen_ = true;
      }
      return true;
    }
    case MessageType::MirrorCtrl:
      return handle_mirror_ctrl(packet);
    case MessageType::Package:
      return handle_package(packet);
    default:
      return false;
  }
}

void SessionDispatcher::send_capabilities(std::uint32_t request_id) {
  SurfaceDesc surface;
  surface.id = "main";
  surface.width = display_.width();
  surface.height = display_.height();
  surface.pixel_format = "RGB565";
  surface.stride = static_cast<std::uint32_t>(display_.width()) * 2u;
  surface.rotation = 0;
  Packet caps;
  caps.type = MessageType::Capabilities;
  caps.flags = 0;
  caps.request_id = request_id;
  caps.payload = encode_capabilities_cbor(identity_, surface);
  reply_(caps);
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
    const auto w = u16le(p + 8);
    const auto h = u16le(p + 10);
    const auto* pixels = p + 12;
    const auto pix_len = packet.payload.size() - header;
    if (w != display_.width() || h != display_.height()) {
      send_error(packet.request_id, "E_DIM", "geometry mismatch");
      return true;
    }
    current_frame_id_ = u64le(p);
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
    const auto w = u16le(p + 8);
    const auto h = u16le(p + 10);
    if (w != display_.width() || h != display_.height()) {
      send_error(packet.request_id, "E_DIM", "geometry mismatch");
      return true;
    }
    current_frame_id_ = u64le(p);
    chunk_expected_ = u32le(p + 12);
    chunk_filled_ = 0;
    chunk_staging_.assign(chunk_expected_, 0);
    return true;
  }
  if (phase == 2) {
    if (chunk_expected_ == 0) {
      send_error(packet.request_id, "E_CHUNK", "no begin");
      return true;
    }
    if (packet.payload.size() < 1 + 8 + 4) return false;
    const auto* p = packet.payload.data() + 1;
    const auto frame_id = u64le(p);
    const auto offset = u32le(p + 8);
    const auto* data = p + 12;
    const auto data_len = packet.payload.size() - (1 + 8 + 4);
    if (frame_id != current_frame_id_) return false;
    if (static_cast<std::size_t>(offset) + data_len > chunk_staging_.size()) return false;
    std::memcpy(chunk_staging_.data() + offset, data, data_len);
    chunk_filled_ += static_cast<std::uint32_t>(data_len);
    return true;
  }
  if (phase == 3) {
    if (chunk_expected_ == 0) {
      send_error(packet.request_id, "E_COMMIT", "no begin");
      return true;
    }
    if (packet.payload.size() < 1 + 8) return false;
    const auto frame_id = u64le(packet.payload.data() + 1);
    if (frame_id != current_frame_id_) return false;
    if (chunk_filled_ != chunk_expected_) {
      send_error(packet.request_id, "E_INCOMPLETE", "incomplete frame");
      return true;
    }
    if (!store_.stage(chunk_staging_.data(), chunk_staging_.size())) return false;
    if (!store_.commit()) return false;
    if (!display_.flush_rect(0, 0, display_.width(), display_.height(), store_.active().data(),
                             store_.active().size()))
      return false;
    if (!display_.present()) return false;
    send_ack(packet.request_id, current_frame_id_, true);
    chunk_staging_.clear();
    chunk_expected_ = 0;
    chunk_filled_ = 0;
    return true;
  }
  return false;
}

bool SessionDispatcher::handle_tile(const Packet& packet) {
  // surface_id_len + id + base_frame_id + x + y + w + h + pixels
  if (packet.payload.empty()) return false;
  const auto id_len = packet.payload[0];
  const std::size_t header = 1 + id_len + 8 + 2 + 2 + 2 + 2;
  if (packet.payload.size() < header) return false;
  const auto* p = packet.payload.data() + 1 + id_len;
  const auto base = u64le(p);
  if (base != current_frame_id_) {
    send_error(packet.request_id, "E_BASE", "base frame mismatch");
    return true;
  }
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

/// Form B: host asked to start/stop mirroring. Capability-gated.
bool SessionDispatcher::handle_mirror_ctrl(const Packet& packet) {
  if (!identity_.mirror) {
    send_error(packet.request_id, "E_UNSUPPORTED", "mirror capability disabled");
    return true;
  }
  const std::string text(packet.payload.begin(), packet.payload.end());
  if (text.find("stop") != std::string::npos) {
    mirror_active_ = false;
  } else if (text.find("start") != std::string::npos) {
    mirror_active_ = true;
  } else {
    send_error(packet.request_id, "E_MIRROR", "unknown action");
    return true;
  }
  send_ack(packet.request_id, current_frame_id_, true);
  return true;
}

/// N33: assemble a host runtime package. The device verifies the transport
/// checksum and the container magic; the IR itself is the host's concern.
bool SessionDispatcher::handle_package(const Packet& packet) {
  if (packet.payload.empty()) return false;
  const auto phase = packet.payload[0];
  if (phase == 1) {
    if (packet.payload.size() != 1 + 4 + 8) return false;
    package_expected_ = u32be(packet.payload.data() + 1);
    package_checksum_ = u64be(packet.payload.data() + 5);
    if (package_expected_ == 0 || package_expected_ > kMaxPackage) {
      send_error(packet.request_id, "E_PACKAGE", "package size over limit");
      return true;
    }
    package_staging_.assign(package_expected_, 0);
    package_filled_ = 0;
    return true;
  }
  if (phase == 2) {
    if (package_expected_ == 0) {
      send_error(packet.request_id, "E_PACKAGE", "chunk before begin");
      return true;
    }
    if (packet.payload.size() < 1 + 4) return false;
    const auto offset = u32be(packet.payload.data() + 1);
    const auto* data = packet.payload.data() + 5;
    const auto data_len = packet.payload.size() - 5;
    if (static_cast<std::size_t>(offset) + data_len > package_staging_.size()) {
      send_error(packet.request_id, "E_PACKAGE", "chunk over package");
      return true;
    }
    std::memcpy(package_staging_.data() + offset, data, data_len);
    package_filled_ += static_cast<std::uint32_t>(data_len);
    return true;
  }
  if (phase == 3) {
    if (package_expected_ == 0) {
      send_error(packet.request_id, "E_PACKAGE", "commit before begin");
      return true;
    }
    if (package_filled_ != package_expected_) {
      send_error(packet.request_id, "E_PACKAGE", "incomplete package");
      return true;
    }
    if (fnv1a64(package_staging_.data(), package_staging_.size()) != package_checksum_) {
      send_error(packet.request_id, "E_PACKAGE", "checksum mismatch");
      return true;
    }
    if (package_staging_.size() < 4 ||
        std::memcmp(package_staging_.data(), "MDCP", 4) != 0) {
      send_error(packet.request_id, "E_PACKAGE", "bad package magic");
      return true;
    }
    package_ready_ = true;
    package_bytes_ = package_staging_.size();
    package_staging_.clear();
    package_expected_ = 0;
    package_filled_ = 0;
    send_ack(packet.request_id, current_frame_id_, true);
    return true;
  }
  return false;
}

/// N33: emit the current framebuffer as a device -> host mirror frame.
bool SessionDispatcher::emit_mirror_frame() {
  if (!mirror_active_ || !identity_.mirror) return false;
  const auto& pixels = store_.active();
  if (pixels.empty()) return false;
  const std::string id = "main";
  Packet frame;
  frame.type = MessageType::MirrorFrame;
  frame.flags = 0;
  frame.request_id = 0;
  frame.payload.push_back(0);  // legacy phase
  frame.payload.push_back(static_cast<std::uint8_t>(id.size()));
  frame.payload.insert(frame.payload.end(), id.begin(), id.end());
  for (int i = 0; i < 8; ++i)
    frame.payload.push_back(static_cast<std::uint8_t>(current_frame_id_ >> (8 * i)));
  frame.payload.push_back(static_cast<std::uint8_t>(display_.width() & 0xff));
  frame.payload.push_back(static_cast<std::uint8_t>((display_.width() >> 8) & 0xff));
  frame.payload.push_back(static_cast<std::uint8_t>(display_.height() & 0xff));
  frame.payload.push_back(static_cast<std::uint8_t>((display_.height() >> 8) & 0xff));
  frame.payload.insert(frame.payload.end(), pixels.begin(), pixels.end());
  mirror_frames_ += 1;
  reply_(frame);
  return true;
}

void SessionDispatcher::send_ack(std::uint32_t request_id, std::uint64_t frame_id, bool displayed) {
  Packet ack;
  ack.type = MessageType::Ack;
  ack.flags = 0;
  ack.request_id = request_id;
  ack.payload = encode_ack_cbor(true, displayed, frame_id);
  reply_(ack);
}

void SessionDispatcher::send_error(std::uint32_t request_id, const char* code, const char* message) {
  Packet err;
  err.type = MessageType::Error;
  err.flags = 0;
  err.request_id = request_id;
  err.payload = encode_error_cbor(code, message);
  reply_(err);
}

void SessionDispatcher::send_pong(std::uint32_t request_id) {
  Packet pong;
  pong.type = MessageType::Pong;
  pong.flags = 0;
  pong.request_id = request_id;
  reply_(pong);
}

}  // namespace mdc
