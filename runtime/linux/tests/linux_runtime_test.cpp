#include "linux_runtime.hpp"
#include <cassert>
#include <vector>

int main() {
  mdc::LinuxRuntime runtime(2, 2);
  assert(runtime.width() == 2);
  assert(runtime.height() == 2);
  assert(runtime.framebuffer().size() == 8);

  mdc::Packet ping;
  ping.type = mdc::MessageType::Ping;
  ping.request_id = 1;
  assert(runtime.handle_packet(ping));
  assert(!runtime.replies().empty());
  assert(runtime.replies().back().type == mdc::MessageType::Pong);

  runtime.clear_replies();
  mdc::Packet hello;
  hello.type = mdc::MessageType::Hello;
  hello.request_id = 7;
  assert(runtime.handle_packet(hello));
  assert(runtime.replies().back().type == mdc::MessageType::Capabilities);
  assert(!runtime.replies().back().payload.empty());
  assert(runtime.replies().back().payload[0] == 0xab);  // CBOR map(11)

  std::vector<std::uint8_t> legacy;
  legacy.push_back(0);  // phase
  legacy.push_back(1);
  legacy.push_back('m');
  for (int i = 0; i < 8; ++i) legacy.push_back(static_cast<std::uint8_t>(1 >> (8 * i)));
  legacy.push_back(2);
  legacy.push_back(0);
  legacy.push_back(2);
  legacy.push_back(0);
  legacy.insert(legacy.end(), 8, 0xCD);
  mdc::Packet frame;
  frame.type = mdc::MessageType::Frame;
  frame.request_id = 2;
  frame.payload = legacy;
  assert(runtime.handle_packet(frame));
  assert(runtime.framebuffer()[0] == 0xCD);
  assert(runtime.replies().back().type == mdc::MessageType::Ack);

  assert(mdc::mdc_linux_probe(0, 800, 480) == 2);
  assert(mdc::mdc_linux_probe(1, 0, 480) == 3);
  assert(mdc::mdc_linux_probe(1, 4, 2) == 0);
  assert(runtime.update_geometry(0, 2) == false);
  assert(runtime.update_geometry(4, 2));
  assert(runtime.width() == 4);
  assert(runtime.height() == 2);
  assert(runtime.framebuffer().size() == 16);

  std::vector<std::uint8_t> resized;
  resized.push_back(0);
  resized.push_back(1);
  resized.push_back('m');
  for (int i = 0; i < 8; ++i) resized.push_back(static_cast<std::uint8_t>(2 >> (8 * i)));
  resized.push_back(4);
  resized.push_back(0);
  resized.push_back(2);
  resized.push_back(0);
  resized.insert(resized.end(), 16, 0xAB);
  mdc::Packet resized_frame;
  resized_frame.type = mdc::MessageType::Frame;
  resized_frame.request_id = 9;
  resized_frame.payload = resized;
  assert(runtime.handle_packet(resized_frame));
  assert(runtime.framebuffer().size() == 16);
  assert(runtime.framebuffer()[0] == 0xAB);
  return 0;
}
