#include "mdc_runtime.hpp"
#include <cassert>
#include <cstdint>
#include <vector>

static std::uint32_t checksum(const std::vector<std::uint8_t>& p) {
  std::uint32_t h = 0x811c9dc5u;
  for (auto b : p) h = (h * 16777619u) ^ b;
  return h;
}
static void put32(std::vector<std::uint8_t>& v, std::size_t at, std::uint32_t n) {
  for (int i = 0; i < 4; ++i) v[at + i] = static_cast<std::uint8_t>(n >> (8 * i));
}

int main() {
  std::vector<std::uint8_t> packet(20 + 4, 0);
  packet[0] = 'M';
  packet[1] = 'D';
  packet[2] = 1;
  packet[4] = 0x10;
  packet[8] = 7;
  put32(packet, 12, 4);
  packet[20] = 1;
  packet[21] = 2;
  packet[22] = 3;
  packet[23] = 4;
  put32(packet, 16, checksum({1, 2, 3, 4}));
  mdc::Parser parser;
  mdc::Packet output;
  assert(parser.feed(packet.data(), 5, output) == mdc::ParseResult::NeedMore);
  assert(parser.feed(packet.data() + 5, packet.size() - 5, output) == mdc::ParseResult::Packet);
  assert(output.request_id == 7 && output.payload.size() == 4);

  mdc::FrameStore store(2, 2);
  assert(store.stage(output.payload.data(), 4) == false);
  std::vector<std::uint8_t> frame(8, 9);
  assert(store.stage(frame.data(), frame.size()));
  assert(store.commit());
  assert(store.active()[0] == 9);

  mdc::FrameStore tight(2, 2);
  tight.set_heap(8, 8);
  std::vector<std::uint8_t> second(8, 0x5A);
  assert(tight.stage(second.data(), second.size()) == false);
  assert(tight.active()[0] == 0);

  mdc::SimDisplay display(2, 2);
  std::vector<mdc::Packet> replies;
  mdc::SessionDispatcher dispatcher(display, [&](const mdc::Packet& p) { replies.push_back(p); });
  auto encoded = mdc::encode_packet(mdc::MessageType::Ping, 3, {});
  std::vector<std::uint8_t> noisy = {'b', 'o', 'o', 't', ' ', 'l', 'o', 'g', '\n'};
  noisy.insert(noisy.end(), encoded.begin(), encoded.end());
  mdc::Parser noisy_parser;
  mdc::Packet recovered;
  assert(noisy_parser.feed(noisy.data(), noisy.size(), recovered) == mdc::ParseResult::Packet);
  assert(recovered.type == mdc::MessageType::Ping);
  assert(recovered.request_id == 3);

  auto bad = encoded;
  if (!bad.empty()) bad.back() ^= 0xff;
  mdc::Parser bad_parser;
  mdc::Packet rejected;
  assert(bad_parser.feed(bad.data(), bad.size(), rejected) == mdc::ParseResult::Invalid);
  assert(rejected.payload.empty());

  std::vector<std::uint8_t> reserved(20, 0);
  reserved[0] = 'M';
  reserved[1] = 'D';
  reserved[2] = 1;
  reserved[4] = 0x40;
  reserved[5] = 1;
  put32(reserved, 12, 0x00ffffffu);
  mdc::Parser reserved_parser;
  mdc::Packet reserved_out;
  assert(reserved_parser.feed(reserved.data(), reserved.size(), reserved_out) ==
         mdc::ParseResult::Invalid);
  assert(reserved_out.payload.empty());

  std::vector<std::uint8_t> flags(20, 0);
  flags[0] = 'M';
  flags[1] = 'D';
  flags[2] = 1;
  flags[4] = 0x40;
  flags[6] = 1;
  put32(flags, 12, 0x00ffffffu);
  mdc::Parser flags_parser;
  mdc::Packet flags_out;
  assert(flags_parser.feed(flags.data(), flags.size(), flags_out) == mdc::ParseResult::Invalid);
  assert(flags_out.payload.empty());

  std::vector<std::uint8_t> unknown(20, 0);
  unknown[0] = 'M';
  unknown[1] = 'D';
  unknown[2] = 1;
  unknown[4] = 0x55;  // still unknown after N303 reserved 0x60..0x63
  put32(unknown, 12, 0x00ffffffu);
  mdc::Parser unknown_parser;
  mdc::Packet unknown_out;
  assert(unknown_parser.feed(unknown.data(), unknown.size(), unknown_out) ==
         mdc::ParseResult::Invalid);
  assert(unknown_out.payload.empty());

  // N303: the reserved diagnostic types parse (no payload is decoded by the
  // device yet), so a downstream pin of the wire enum is stable.
  for (std::uint8_t reserved : {0x60, 0x61, 0x62, 0x63}) {
    auto packet = mdc::encode_packet(static_cast<mdc::MessageType>(reserved), 5, {1, 2});
    mdc::Parser reserved_parser;
    mdc::Packet reserved_out;
    assert(reserved_parser.feed(packet.data(), packet.size(), reserved_out) ==
           mdc::ParseResult::Packet);
    assert(static_cast<std::uint8_t>(reserved_out.type) == reserved);
    assert(reserved_out.payload.size() == 2);
  }

  mdc::Packet ping;
  assert(parser.feed(encoded.data(), encoded.size(), ping) == mdc::ParseResult::Packet);
  assert(dispatcher.handle(ping));
  assert(!replies.empty());
  assert(replies.back().type == mdc::MessageType::Pong);

  // Legacy full frame: phase0 + id_len + "m" + frame_id + w + h + 8 pixels
  std::vector<std::uint8_t> legacy;
  legacy.push_back(0);
  legacy.push_back(1);
  legacy.push_back('m');
  for (int i = 0; i < 8; ++i) legacy.push_back(static_cast<std::uint8_t>(1 >> (8 * i)));  // frame id 1
  legacy.push_back(2);
  legacy.push_back(0);  // w
  legacy.push_back(2);
  legacy.push_back(0);  // h
  legacy.insert(legacy.end(), 8, 0xAB);
  mdc::Packet frame_pkt;
  frame_pkt.type = mdc::MessageType::Frame;
  frame_pkt.request_id = 9;
  frame_pkt.payload = legacy;
  assert(dispatcher.handle(frame_pkt));
  assert(display.pixels()[0] == 0xAB);
  assert(replies.back().type == mdc::MessageType::Ack);

  // SIM-A01: mirror/package types must pass the byte parser, not only dispatch.
  for (const auto type : {mdc::MessageType::MirrorFrame, mdc::MessageType::MirrorCtrl,
                          mdc::MessageType::Package, mdc::MessageType::Config,
                          mdc::MessageType::Scene, mdc::MessageType::ModelParams}) {
    auto wire = mdc::encode_packet(type, 44, {1, 2, 3});
    mdc::Parser type_parser;
    mdc::Packet typed;
    assert(type_parser.feed(wire.data(), wire.size(), typed) == mdc::ParseResult::Packet);
    assert(typed.type == type);
    assert(typed.request_id == 44);
  }

  // SIM-A02: chunk coverage, duplicates, overlap and declared length.
  const auto begin_payload = [](const std::string& surface, std::uint64_t frame_id,
                                std::uint32_t total) {
    std::vector<std::uint8_t> payload{1, static_cast<std::uint8_t>(surface.size())};
    payload.insert(payload.end(), surface.begin(), surface.end());
    for (int i = 0; i < 8; ++i) payload.push_back(static_cast<std::uint8_t>(frame_id >> (8 * i)));
    payload.push_back(2);
    payload.push_back(0);
    payload.push_back(2);
    payload.push_back(0);
    for (int i = 0; i < 4; ++i) payload.push_back(static_cast<std::uint8_t>(total >> (8 * i)));
    return payload;
  };
  const auto chunk_payload = [](std::uint64_t frame_id, std::uint32_t offset,
                                const std::vector<std::uint8_t>& data) {
    std::vector<std::uint8_t> payload{2};
    for (int i = 0; i < 8; ++i) payload.push_back(static_cast<std::uint8_t>(frame_id >> (8 * i)));
    for (int i = 0; i < 4; ++i) payload.push_back(static_cast<std::uint8_t>(offset >> (8 * i)));
    payload.insert(payload.end(), data.begin(), data.end());
    return payload;
  };
  const auto commit_payload = [](std::uint64_t frame_id) {
    std::vector<std::uint8_t> payload{3};
    for (int i = 0; i < 8; ++i) payload.push_back(static_cast<std::uint8_t>(frame_id >> (8 * i)));
    return payload;
  };
  auto send = [&](mdc::MessageType type, std::uint32_t request,
                  const std::vector<std::uint8_t>& payload) {
    mdc::Packet packet;
    packet.type = type;
    packet.request_id = request;
    packet.payload = payload;
    return dispatcher.handle(packet);
  };

  // A duplicate identical chunk is idempotent but cannot cover the missing half.
  replies.clear();
  assert(send(mdc::MessageType::Frame, 30, begin_payload("main", 30, 8)));
  assert(send(mdc::MessageType::Frame, 31, chunk_payload(30, 0, {0x11, 0x11, 0x11, 0x11})));
  assert(send(mdc::MessageType::Frame, 32, chunk_payload(30, 0, {0x11, 0x11, 0x11, 0x11})));
  assert(send(mdc::MessageType::Frame, 33, commit_payload(30)));
  assert(replies.back().type == mdc::MessageType::Error);
  assert(display.pixels()[0] == 0xAB);

  // Out-of-order but complete chunks commit exactly once.
  replies.clear();
  assert(send(mdc::MessageType::Frame, 40, begin_payload("main", 40, 8)));
  assert(send(mdc::MessageType::Frame, 41, chunk_payload(40, 4, {0x22, 0x22, 0x22, 0x22})));
  assert(send(mdc::MessageType::Frame, 42, chunk_payload(40, 0, {0x33, 0x33, 0x33, 0x33})));
  assert(send(mdc::MessageType::Frame, 43, commit_payload(40)));
  assert(replies.back().type == mdc::MessageType::Ack);
  assert(display.pixels()[0] == 0x33 && display.pixels()[4] == 0x22);

  // Partial overlap is rejected instead of silently zero-filling.
  replies.clear();
  assert(send(mdc::MessageType::Frame, 50, begin_payload("main", 50, 8)));
  assert(send(mdc::MessageType::Frame, 51, chunk_payload(50, 0, {1, 1, 1, 1})));
  assert(send(mdc::MessageType::Frame, 52, chunk_payload(50, 2, {2, 2, 2, 2})));
  assert(replies.back().type == mdc::MessageType::Error);
  assert(display.pixels()[0] == 0x33);

  // Declared length must match geometry before allocation.
  replies.clear();
  assert(send(mdc::MessageType::Frame, 60, begin_payload("main", 60, 999999)));
  assert(replies.back().type == mdc::MessageType::Error);

  // Unknown surface is rejected.
  replies.clear();
  assert(send(mdc::MessageType::Frame, 70, begin_payload("other", 70, 8)));
  assert(replies.back().type == mdc::MessageType::Error);

  // SIM-R07: disjoint chunk ranges are capped; a fresh BEGIN recovers.
  {
    mdc::SimDisplay big(64, 64);
    std::vector<mdc::Packet> big_replies;
    mdc::SessionDispatcher big_dispatcher(
        big, [&](const mdc::Packet& p) { big_replies.push_back(p); });
    const auto begin_64 = [](std::uint64_t frame_id, std::uint32_t total) {
      std::vector<std::uint8_t> payload{1, 4};
      const std::string surface = "main";
      payload.insert(payload.end(), surface.begin(), surface.end());
      for (int i = 0; i < 8; ++i)
        payload.push_back(static_cast<std::uint8_t>(frame_id >> (8 * i)));
      payload.push_back(64);
      payload.push_back(0);
      payload.push_back(64);
      payload.push_back(0);
      for (int i = 0; i < 4; ++i) payload.push_back(static_cast<std::uint8_t>(total >> (8 * i)));
      return payload;
    };
    auto send_big = [&](mdc::MessageType type, std::uint32_t request,
                        const std::vector<std::uint8_t>& payload) {
      mdc::Packet packet;
      packet.type = type;
      packet.request_id = request;
      packet.payload = payload;
      return big_dispatcher.handle(packet);
    };
    big_replies.clear();
    assert(send_big(mdc::MessageType::Frame, 500, begin_64(500, 64 * 64 * 2)));
    for (std::uint32_t i = 0; i < 256; ++i) {
      assert(send_big(mdc::MessageType::Frame, 600 + i,
                      chunk_payload(500, i * 2, {0x11})));
    }
    assert(big_replies.empty());
    assert(send_big(mdc::MessageType::Frame, 900, chunk_payload(500, 512, {0x22})));
    assert(big_replies.back().type == mdc::MessageType::Error);
    // A new BEGIN clears the rejected transaction and is accepted again.
    big_replies.clear();
    assert(send_big(mdc::MessageType::Frame, 901, begin_64(901, 64 * 64 * 2)));
    assert(send_big(mdc::MessageType::Frame, 902, chunk_payload(901, 0, {0x33, 0x44})));
    assert(big_replies.empty());
  }

  // Dual-transport lock
  // Dual-transport lock
  dispatcher.set_transport_busy(true);
  assert(!dispatcher.handle(frame_pkt));
  dispatcher.set_transport_busy(false);

  // N03 Form B: MirrorCtrl start is capability-gated.
  const std::vector<std::uint8_t> start_payload = {0xa1, 0x66, 'a', 'c', 't', 'i', 'o', 'n',
                                                   0x65, 's', 't', 'a', 'r', 't'};
  mdc::Packet ctrl;
  ctrl.type = mdc::MessageType::MirrorCtrl;
  ctrl.flags = 0;
  ctrl.request_id = 21;
  ctrl.payload = start_payload;
  // A device that does not advertise mirror rejects the control.
  assert(dispatcher.handle(ctrl));
  assert(!dispatcher.mirror_active());
  assert(replies.back().type == mdc::MessageType::Error);

  mdc::PeerIdentity mirror_id;
  mirror_id.device_id = "linux-virt";
  mirror_id.mirror = true;
  mirror_id.mirror_fps = 10;
  mdc::SessionDispatcher mirror_dispatcher(
      display, [&](const mdc::Packet& p) { replies.push_back(p); }, mirror_id);
  assert(mirror_dispatcher.handle(ctrl));
  assert(mirror_dispatcher.mirror_active());
  assert(replies.back().type == mdc::MessageType::Ack);

  const std::vector<std::uint8_t> stop_payload = {0xa1, 0x66, 'a', 'c', 't', 'i', 'o', 'n',
                                                  0x64, 's', 't', 'o', 'p'};
  ctrl.payload = stop_payload;
  assert(mirror_dispatcher.handle(ctrl));
  assert(!mirror_dispatcher.mirror_active());
  assert(replies.back().type == mdc::MessageType::Ack);
  return 0;
}
