#pragma once
#include "mdc_runtime.hpp"
#include <cstdint>
#include <string>
#include <vector>

namespace mdc {

/// Linux host peer: owns SimDisplay framebuffer and SessionDispatcher.
class LinuxRuntime {
 public:
  LinuxRuntime(std::uint16_t width, std::uint16_t height, PeerIdentity identity = {});
  DisplayBackend& display() { return display_; }
  const DisplayBackend& display() const { return display_; }
  bool handle_packet(const Packet& packet);
  const std::vector<std::uint8_t>& framebuffer() const { return display_.pixels(); }
  std::uint16_t width() const { return display_.width(); }
  std::uint16_t height() const { return display_.height(); }
  const std::vector<Packet>& replies() const { return replies_; }
  void clear_replies() { replies_.clear(); }
  const PeerIdentity& identity() const { return dispatcher_.identity(); }
  /// 初始化之后按探测到的真实尺寸重分配 framebuffer。0 尺寸拒绝。
  bool update_geometry(std::uint16_t width, std::uint16_t height);

 private:
  SimDisplay display_;
  SessionDispatcher dispatcher_;
  std::vector<Packet> replies_;
};

/// 无显示会话返回 2，宽或高为 0 返回 3，可打开返回 0。
inline int mdc_linux_probe(int display_present, std::uint16_t width, std::uint16_t height) {
  if (!display_present) return 2;
  if (width == 0 || height == 0) return 3;
  return 0;
}

}  // namespace mdc
