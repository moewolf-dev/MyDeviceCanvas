#include "mdc_display_adapter.hpp"
#include <cassert>
#include <vector>

int main() {
  mdc::SimDisplay sink(480, 320);
  mdc::BoardDisplayAdapter adapter(mdc::jc3248w535_info(), sink);
  assert(adapter.width() == 480);
  assert(adapter.height() == 320);
  assert(adapter.info().requires_canvas_wrapper);

  std::vector<std::uint8_t> full(mdc::rgb565_bytes(480, 320), 0x11);
  assert(adapter.flush_rect(0, 0, 480, 320, full.data(), full.size()));
  assert(adapter.present());
  assert(adapter.pixels()[0] == 0x11);

  // Detect landscape → portrait write-back
  assert(adapter.update_detected_geometry(320, 480));
  assert(adapter.width() == 320);
  assert(adapter.height() == 480);
  assert(adapter.take_geometry_dirty());
  assert(!adapter.take_geometry_dirty());
  assert(!adapter.update_detected_geometry(320, 480));  // unchanged

  // Wrong length rejected
  assert(!adapter.flush_rect(0, 0, 10, 10, full.data(), 3));
  return 0;
}
