#pragma once
#include "mdc_display_budget.hpp"
#include "mdc_runtime.hpp"
#include <cstdint>
#include <string>

namespace mdc {

/// Board display adapter metadata (pins from AgentDeck board_35_ips.h, re-expressed).
struct DisplayAdapterInfo {
  const char* board_id;
  const char* controller;  // e.g. "axs15231b"
  std::uint16_t native_width;
  std::uint16_t native_height;
  std::uint16_t logical_width;
  std::uint16_t logical_height;
  std::uint16_t rotation;
  bool requires_canvas_wrapper;  // AgentDeck: direct QSPI without Canvas → black screen
  std::uint32_t qspi_hz_default;
};

inline DisplayAdapterInfo jc3248w535_info() {
  return DisplayAdapterInfo{
      "jc3248w535",
      "axs15231b",
      320,
      480,
      480,
      320,
      1,
      true,
      32000000u,
  };
}

/// Host-testable adapter: uses SimDisplay as the pixel sink while enforcing
/// board geometry / Canvas requirement flags. Hardware QSPI init stays behind
/// ESP-IDF (not linked in host tests).
class BoardDisplayAdapter : public DisplayBackend {
 public:
  BoardDisplayAdapter(DisplayAdapterInfo info, DisplayBackend& sink)
      : info_(info), sink_(sink), reported_w_(info.logical_width), reported_h_(info.logical_height) {}

  std::uint16_t width() const override { return reported_w_; }
  std::uint16_t height() const override { return reported_h_; }

  bool flush_rect(std::uint16_t x, std::uint16_t y, std::uint16_t w, std::uint16_t h,
                  const std::uint8_t* rgb565, std::size_t length) override {
    if (!info_.requires_canvas_wrapper) {
      // Hardware path without Canvas is unsupported for AXS15231B class.
      return false;
    }
    const std::size_t expect = rgb565_bytes(w, h);
    if (expect == 0 || length != expect) return false;
    return sink_.flush_rect(x, y, w, h, rgb565, length);
  }

  bool present() override { return sink_.present(); }
  const std::vector<std::uint8_t>& pixels() const override { return sink_.pixels(); }

  /// Runtime geometry detect write-back (FrameOS display_detect idea, own code).
  bool update_detected_geometry(std::uint16_t w, std::uint16_t h) {
    if (w == 0 || h == 0) return false;
    if (w == reported_w_ && h == reported_h_) return false;
    if (rgb565_bytes(w, h) == 0) return false;
    reported_w_ = w;
    reported_h_ = h;
    geometry_dirty_ = true;
    return true;
  }

  bool take_geometry_dirty() {
    bool d = geometry_dirty_;
    geometry_dirty_ = false;
    return d;
  }

  const DisplayAdapterInfo& info() const { return info_; }

 private:
  DisplayAdapterInfo info_;
  DisplayBackend& sink_;
  std::uint16_t reported_w_;
  std::uint16_t reported_h_;
  bool geometry_dirty_ = false;
};

}  // namespace mdc
