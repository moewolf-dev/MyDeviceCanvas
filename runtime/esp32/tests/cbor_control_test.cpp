#include "mdc_cbor.hpp"
#include <cassert>
#include <cstdio>
#include <string>

static std::string to_hex(const std::vector<std::uint8_t>& b) {
  static const char* k = "0123456789abcdef";
  std::string out;
  out.reserve(b.size() * 2);
  for (auto x : b) {
    out.push_back(k[x >> 4]);
    out.push_back(k[x & 0xf]);
  }
  return out;
}

int main() {
  mdc::PeerIdentity id;
  id.device_id = "sim-linux-virt";
  id.firmware = "0.1.0-linux";
  id.touch = false;
  id.ota = false;
  id.max_message = 1048576;
  id.max_chunk = 16384;
  id.max_in_flight = 1;
  id.max_fps = 30;

  mdc::SurfaceDesc surface;
  surface.id = "main";
  surface.width = 800;
  surface.height = 480;
  surface.pixel_format = "RGB565";
  surface.stride = 1600;
  surface.rotation = 0;

  const auto caps = mdc::encode_capabilities_cbor(id, surface);
  const auto caps_hex = to_hex(caps);
  const char* expect_caps =
      "ab696465766963655f69646e73696d2d6c696e75782d76697274686669726d776172656b302e312e302d6c696e7578"
      "68737572666163657381a6626964646d61696e657769647468190320666865696768741901e06c706978656c5f666f"
      "726d6174665247423536356673747269646519064068726f746174696f6e00656672616d65f56474696c65f5"
      "65746f756368f4636f7461f46b6d61785f6d6573736167651a00100000696d61785f6368756e6b1940006d6d61785f"
      "696e5f666c6967687401676d61785f667073181e";
  if (caps_hex != expect_caps) {
    std::fprintf(stderr, "caps mismatch\ngot %s\nexp %s\n", caps_hex.c_str(), expect_caps);
    return 1;
  }

  const auto ack = mdc::encode_ack_cbor(true, true, 42);
  const auto ack_hex = to_hex(ack);
  const char* expect_ack = "a4687265636569766564f569646973706c61796564f5686672616d655f6964182a656572726f72f6";
  if (ack_hex != expect_ack) {
    std::fprintf(stderr, "ack mismatch\ngot %s\nexp %s\n", ack_hex.c_str(), expect_ack);
    return 2;
  }

  std::printf("cbor_control_test ok\n");
  return 0;
}
