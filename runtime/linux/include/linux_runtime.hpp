#pragma once
#include "mdc_runtime.hpp"
#include <cstdint>
#include <string>
#include <vector>

namespace mdc {

/// Linux host peer: owns SimDisplay framebuffer and SessionDispatcher.
class LinuxRuntime {
 public:
  LinuxRuntime(std::uint16_t width, std::uint16_t height);
  DisplayBackend& display() { return display_; }
  const DisplayBackend& display() const { return display_; }
  bool handle_packet(const Packet& packet);
  const std::vector<std::uint8_t>& framebuffer() const { return display_.pixels(); }
  std::uint16_t width() const { return display_.width(); }
  std::uint16_t height() const { return display_.height(); }
  const std::vector<Packet>& replies() const { return replies_; }
  void clear_replies() { replies_.clear(); }

 private:
  SimDisplay display_;
  SessionDispatcher dispatcher_;
  std::vector<Packet> replies_;
};

}  // namespace mdc
