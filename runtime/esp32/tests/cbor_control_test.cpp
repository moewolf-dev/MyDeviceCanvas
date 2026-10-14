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
  id.mirror = true;
  id.local_display_fps = 60;
  id.mirror_fps = 10;
  id.mirror_bandwidth = 7680000ull;

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
      "af696465766963655f69646e73696d2d6c696e75782d76697274686669726d776172656b302e312e302d6c696e757868737572666163657381a6626964646d61696e657769647468190320666865696768741901e06c706978656c5f666f726d6174665247423536356673747269646519064068726f746174696f6e00656672616d65f56474696c65f565746f756368f4636f7461f46b6d61785f6d6573736167651a00100000696d61785f6368756e6b1940006d6d61785f696e5f666c6967687401676d61785f667073181e666d6972726f72a6646d6f6465816a66756c6c2d6672616d65676d61785f6670730a696d61785f77696474681903206a6d61785f6865696768741901e06d706978656c5f666f726d6174738166524742353635757265636f6d6d656e6465645f7472616e73706f7274f6716c6f63616c5f646973706c61795f667073183c6a6d6972726f725f6670730a706d6972726f725f62616e6477696474681a00753000";
  if (caps_hex != expect_caps) {
    std::fprintf(stderr, "caps mismatch\ngot %s\nexp %s\n", caps_hex.c_str(), expect_caps);
    return 1;
  }

  // N17: renderer capability is appended only when advertised. This hex is the
  // Rust serde_cbor encoding of the same Capabilities (mdc-protocol test).
  mdc::PeerIdentity rid;
  rid.device_id = "sim-linux-virt";
  rid.firmware = "0.1.0-linux";
  rid.touch = false;
  rid.ota = false;
  rid.max_message = 1048576;
  rid.max_chunk = 16384;
  rid.max_in_flight = 1;
  rid.max_fps = 30;
  rid.renderer = true;
  rid.renderer_sprite_2d = true;
  rid.renderer_mesh2d = true;
  rid.renderer_mesh3d = true;
  rid.renderer_skeleton = true;
  rid.renderer_max_texture = 2048;
  rid.renderer_max_vertices = 200000;

  mdc::SurfaceDesc rsurface;
  rsurface.id = "main";
  rsurface.width = 800;
  rsurface.height = 480;
  rsurface.pixel_format = "RGB565";
  rsurface.stride = 1600;
  rsurface.rotation = 0;

  const auto rcaps_hex = to_hex(mdc::encode_capabilities_cbor(rid, rsurface));
  const char* expect_rcaps =
      "b0696465766963655f69646e73696d2d6c696e75782d76697274686669726d776172656b302e312e302d6c696e757868737572666163657381a6626964646d61696e657769647468190320666865696768741901e06c706978656c5f666f726d6174665247423536356673747269646519064068726f746174696f6e00656672616d65f56474696c65f565746f756368f4636f7461f46b6d61785f6d6573736167651a00100000696d61785f6368756e6b1940006d6d61785f696e5f666c6967687401676d61785f667073181e666d6972726f72a6646d6f646580676d61785f66707300696d61785f7769647468006a6d61785f686569676874006d706978656c5f666f726d61747380757265636f6d6d656e6465645f7472616e73706f7274f6716c6f63616c5f646973706c61795f667073006a6d6972726f725f66707300706d6972726f725f62616e647769647468006872656e6465726572a6697370726974655f3264f5666d6573683264f5666d6573683364f568736b656c65746f6ef56b6d61785f746578747572651908006c6d61785f76657274696365731a00030d40";
  if (rcaps_hex != expect_rcaps) {
    std::fprintf(stderr, "renderer caps mismatch\ngot %s\nexp %s\n", rcaps_hex.c_str(),
                 expect_rcaps);
    return 3;
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
