#include "linux_runtime.hpp"

namespace mdc {

LinuxRuntime::LinuxRuntime(std::uint16_t width, std::uint16_t height, PeerIdentity identity)
    : display_(width, height),
      dispatcher_(display_, [this](const Packet& p) { replies_.push_back(p); }, std::move(identity)) {}

bool LinuxRuntime::handle_packet(const Packet& packet) {
  return dispatcher_.handle(packet);
}

bool LinuxRuntime::update_geometry(std::uint16_t width, std::uint16_t height) {
  if (kind_ == DisplaySessionKind::NoSession) return false;
  if (!display_.resize(width, height)) return false;
  dispatcher_.sync_geometry();
  replies_.clear();
  return true;
}

void LinuxRuntime::use_no_session() {
  kind_ = DisplaySessionKind::NoSession;
}

int LinuxRuntime::probe() const {
  if (kind_ == DisplaySessionKind::NoSession) return 2;
  return mdc_linux_probe(1, display_.width(), display_.height());
}

}  // namespace mdc
