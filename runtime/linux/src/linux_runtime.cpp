#include "linux_runtime.hpp"

namespace mdc {

LinuxRuntime::LinuxRuntime(std::uint16_t width, std::uint16_t height, PeerIdentity identity)
    : display_(width, height),
      dispatcher_(display_, [this](const Packet& p) { replies_.push_back(p); }, std::move(identity)) {}

bool LinuxRuntime::handle_packet(const Packet& packet) {
  return dispatcher_.handle(packet);
}

bool LinuxRuntime::update_geometry(std::uint16_t width, std::uint16_t height) {
  if (!display_.resize(width, height)) return false;
  dispatcher_.sync_geometry();
  replies_.clear();
  return true;
}

}  // namespace mdc
