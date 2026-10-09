#pragma once
/**
 * Minimal CBOR writers matching serde_cbor map encoding for Capabilities / Ack.
 * Host-testable; no third-party CBOR dependency on the device runtime path.
 */
#include <cstdint>
#include <string>
#include <vector>

namespace mdc {

struct PeerIdentity {
  std::string device_id = "sim-device";
  std::string firmware = "0.1.0";
  bool touch = false;
  bool ota = false;
  std::uint32_t max_message = 1024u * 1024u;
  std::uint32_t max_chunk = 16384;
  std::uint8_t max_in_flight = 1;
  std::uint16_t max_fps = 30;
};

struct SurfaceDesc {
  std::string id = "main";
  std::uint16_t width = 0;
  std::uint16_t height = 0;
  std::string pixel_format = "RGB565";  // protocol SSOT; little-endian on wire
  std::uint32_t stride = 0;  // bytes per row; 0 → width * 2
  std::uint16_t rotation = 0;
};

/// Encode Capabilities control payload (MessageType::Capabilities).
std::vector<std::uint8_t> encode_capabilities_cbor(const PeerIdentity& id,
                                                   const SurfaceDesc& surface);

/// Encode Ack control payload (MessageType::Ack).
std::vector<std::uint8_t> encode_ack_cbor(bool received, bool displayed, std::uint64_t frame_id);

}  // namespace mdc
