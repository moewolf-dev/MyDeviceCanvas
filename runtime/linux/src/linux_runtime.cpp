#include "linux_runtime.hpp"

namespace mdc {

LinuxRuntime::LinuxRuntime(std::uint16_t width, std::uint16_t height)
    : display_(width, height),
      dispatcher_(display_, [this](const Packet& p) { replies_.push_back(p); }) {}

bool LinuxRuntime::handle_packet(const Packet& packet) {
  return dispatcher_.handle(packet);
}

}  // namespace mdc
