#include "mdc_runtime.hpp"
#include <cassert>
#include <cstdint>
#include <vector>

static std::uint32_t checksum(const std::vector<std::uint8_t>& p) { std::uint32_t h = 0x811c9dc5u; for (auto b : p) h = (h * 16777619u) ^ b; return h; }
static void put32(std::vector<std::uint8_t>& v, std::size_t at, std::uint32_t n) { for (int i = 0; i < 4; ++i) v[at + i] = static_cast<std::uint8_t>(n >> (8 * i)); }

int main() {
  std::vector<std::uint8_t> packet(20 + 4, 0);
  packet[0] = 'M'; packet[1] = 'D'; packet[2] = 1; packet[4] = 0x10; packet[8] = 7; put32(packet, 12, 4);
  packet[20] = 1; packet[21] = 2; packet[22] = 3; packet[23] = 4; put32(packet, 16, checksum({1,2,3,4}));
  mdc::Parser parser; mdc::Packet output;
  assert(parser.feed(packet.data(), 5, output) == mdc::ParseResult::NeedMore);
  assert(parser.feed(packet.data() + 5, packet.size() - 5, output) == mdc::ParseResult::Packet);
  assert(output.request_id == 7 && output.payload.size() == 4);
  mdc::FrameStore store(2, 2); assert(store.stage(output.payload.data(), 4) == false); std::vector<std::uint8_t> frame(8, 9); assert(store.stage(frame.data(), frame.size())); assert(store.commit()); assert(store.active()[0] == 9);
  return 0;
}
