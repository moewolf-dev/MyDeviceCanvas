#include "mdc_runtime.hpp"
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

using namespace mdc;

static std::uint64_t fnv1a64(const std::vector<std::uint8_t>& b) {
  std::uint64_t h = 0xcbf29ce484222325ull;
  for (auto x : b) {
    h ^= x;
    h *= 0x100000001b3ull;
  }
  return h;
}

static std::vector<std::uint8_t> begin_payload(std::uint32_t total, std::uint64_t sum) {
  std::vector<std::uint8_t> p{1};
  p.push_back(static_cast<std::uint8_t>(total >> 24));
  p.push_back(static_cast<std::uint8_t>(total >> 16));
  p.push_back(static_cast<std::uint8_t>(total >> 8));
  p.push_back(static_cast<std::uint8_t>(total));
  for (int i = 7; i >= 0; --i) p.push_back(static_cast<std::uint8_t>(sum >> (8 * i)));
  return p;
}

static std::vector<std::uint8_t> chunk_payload(std::uint32_t offset,
                                               const std::vector<std::uint8_t>& data) {
  std::vector<std::uint8_t> p{2, static_cast<std::uint8_t>(offset >> 24),
                              static_cast<std::uint8_t>(offset >> 16),
                              static_cast<std::uint8_t>(offset >> 8),
                              static_cast<std::uint8_t>(offset)};
  p.insert(p.end(), data.begin(), data.end());
  return p;
}

int main() {
  PeerIdentity id;
  id.mirror = true;

  // A tiny "package": MDCP + 8 bytes.
  std::vector<std::uint8_t> package{'M', 'D', 'C', 'P', 1, 2, 3, 4, 5, 6, 7, 8};

  {
    SimDisplay display(2, 2);
    std::vector<Packet> replies;
    SessionDispatcher dispatcher(display, [&](const Packet& p) { replies.push_back(p); }, id);
    assert(dispatcher.handle(Packet{MessageType::Package, 0, 1,
                                    begin_payload(static_cast<std::uint32_t>(package.size()),
                                                  fnv1a64(package))}));
    std::vector<std::uint8_t> first(package.begin(), package.begin() + 4);
    std::vector<std::uint8_t> second(package.begin() + 4, package.end());
    assert(dispatcher.handle(Packet{MessageType::Package, 0, 2, chunk_payload(0, first)}));
    assert(dispatcher.handle(Packet{MessageType::Package, 0, 3, chunk_payload(4, second)}));
    assert(dispatcher.handle(Packet{MessageType::Package, 0, 4, {3}}));
    assert(dispatcher.package_ready());
    assert(dispatcher.package_bytes() == package.size());
  }

  {
    // A wrong transport checksum is rejected before the package is accepted.
    SimDisplay display(2, 2);
    std::vector<Packet> replies;
    SessionDispatcher dispatcher(display, [&](const Packet& p) { replies.push_back(p); }, id);
    assert(dispatcher.handle(
        Packet{MessageType::Package, 0, 1, begin_payload(static_cast<std::uint32_t>(package.size()), 0xdead)}));
    assert(dispatcher.handle(Packet{MessageType::Package, 0, 2, chunk_payload(0, package)}));
    assert(dispatcher.handle(Packet{MessageType::Package, 0, 3, {3}}));
    assert(!dispatcher.package_ready());
    assert(!replies.empty() && replies.back().type == MessageType::Error);
  }

  {
    // Mirror emit needs an active mirror and a committed frame.
    SimDisplay display(2, 2);
    std::vector<Packet> replies;
    SessionDispatcher dispatcher(display, [&](const Packet& p) { replies.push_back(p); }, id);
    const std::string sid = "main";
    std::vector<std::uint8_t> frame{0, static_cast<std::uint8_t>(sid.size())};
    frame.insert(frame.end(), sid.begin(), sid.end());
    for (int i = 0; i < 8; ++i) frame.push_back(0);  // frame_id 0
    frame.push_back(2);
    frame.push_back(0);
    frame.push_back(2);
    frame.push_back(0);
    for (int i = 0; i < 8; ++i) frame.push_back(0x11);
    assert(dispatcher.handle(Packet{MessageType::Frame, 0, 5, frame}));

    assert(!dispatcher.emit_mirror_frame());  // mirror not started yet
    const std::string start = "{\"action\":\"start\"}";
    dispatcher.handle(Packet{MessageType::MirrorCtrl, 0, 6,
                             std::vector<std::uint8_t>(start.begin(), start.end())});
    assert(dispatcher.mirror_active());
    assert(dispatcher.emit_mirror_frame());
    assert(dispatcher.mirror_frames_sent() == 1);
    assert(replies.back().type == MessageType::MirrorFrame);
  }

  std::printf("package_test ok\n");
  return 0;
}
