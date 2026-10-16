#include "mdc_cbor.hpp"

namespace mdc {
namespace {

void push_u8(std::vector<std::uint8_t>& o, std::uint8_t v) { o.push_back(v); }

void push_uint(std::vector<std::uint8_t>& o, std::uint64_t v) {
  if (v < 24) {
    push_u8(o, static_cast<std::uint8_t>(v));
  } else if (v <= 0xff) {
    push_u8(o, 0x18);
    push_u8(o, static_cast<std::uint8_t>(v));
  } else if (v <= 0xffff) {
    push_u8(o, 0x19);
    push_u8(o, static_cast<std::uint8_t>(v >> 8));
    push_u8(o, static_cast<std::uint8_t>(v));
  } else if (v <= 0xffffffffull) {
    push_u8(o, 0x1a);
    push_u8(o, static_cast<std::uint8_t>(v >> 24));
    push_u8(o, static_cast<std::uint8_t>(v >> 16));
    push_u8(o, static_cast<std::uint8_t>(v >> 8));
    push_u8(o, static_cast<std::uint8_t>(v));
  } else {
    push_u8(o, 0x1b);
    for (int i = 7; i >= 0; --i) {
      push_u8(o, static_cast<std::uint8_t>(v >> (8 * i)));
    }
  }
}

void push_text(std::vector<std::uint8_t>& o, const std::string& s) {
  const auto n = s.size();
  if (n < 24) {
    push_u8(o, static_cast<std::uint8_t>(0x60 + n));
  } else if (n <= 0xff) {
    push_u8(o, 0x78);
    push_u8(o, static_cast<std::uint8_t>(n));
  } else {
    push_u8(o, 0x79);
    push_u8(o, static_cast<std::uint8_t>(n >> 8));
    push_u8(o, static_cast<std::uint8_t>(n));
  }
  o.insert(o.end(), s.begin(), s.end());
}

void push_bool(std::vector<std::uint8_t>& o, bool v) { push_u8(o, v ? 0xf5 : 0xf4); }

/// N331: IEEE-754 double as a CBOR float64 (0xfb), matching serde_cbor.
void push_double(std::vector<std::uint8_t>& o, double v) {
  std::uint64_t bits = 0;
  std::memcpy(&bits, &v, sizeof(bits));
  push_u8(o, 0xfb);
  for (int i = 7; i >= 0; --i) {
    push_u8(o, static_cast<std::uint8_t>(bits >> (8 * i)));
  }
}

void push_null(std::vector<std::uint8_t>& o) { push_u8(o, 0xf6); }

void push_array_header(std::vector<std::uint8_t>& o, std::size_t n) {
  if (n < 24) {
    push_u8(o, static_cast<std::uint8_t>(0x80 + n));
  } else if (n <= 0xff) {
    push_u8(o, 0x98);
    push_u8(o, static_cast<std::uint8_t>(n));
  } else {
    push_u8(o, 0x99);
    push_u8(o, static_cast<std::uint8_t>(n >> 8));
    push_u8(o, static_cast<std::uint8_t>(n));
  }
}

void push_kv_text(std::vector<std::uint8_t>& o, const char* key, const std::string& val) {
  push_text(o, key);
  push_text(o, val);
}

void push_kv_uint(std::vector<std::uint8_t>& o, const char* key, std::uint64_t val) {
  push_text(o, key);
  push_uint(o, val);
}

void push_kv_bool(std::vector<std::uint8_t>& o, const char* key, bool val) {
  push_text(o, key);
  push_bool(o, val);
}

void push_kv_null(std::vector<std::uint8_t>& o, const char* key) {
  push_text(o, key);
  push_null(o);
}

}  // namespace

std::vector<std::uint8_t> encode_capabilities_cbor(const PeerIdentity& id,
                                                   const SurfaceDesc& surface) {
  std::vector<std::uint8_t> o;
  o.reserve(384);
  // map(16 with renderer, 15 without) — field order matches serde struct order
  push_u8(o, id.renderer ? 0xb0 : 0xaf);
  push_kv_text(o, "device_id", id.device_id);
  push_kv_text(o, "firmware", id.firmware);

  push_text(o, "surfaces");
  push_u8(o, 0x81);  // array(1)
  push_u8(o, 0xa6);  // map(6) Surface
  const auto stride = surface.stride == 0
                          ? static_cast<std::uint32_t>(surface.width) * 2u
                          : surface.stride;
  push_kv_text(o, "id", surface.id);
  push_kv_uint(o, "width", surface.width);
  push_kv_uint(o, "height", surface.height);
  push_kv_text(o, "pixel_format", surface.pixel_format);
  push_kv_uint(o, "stride", stride);
  push_kv_uint(o, "rotation", surface.rotation);

  push_kv_bool(o, "frame", true);
  push_kv_bool(o, "tile", true);
  push_kv_bool(o, "touch", id.touch);
  push_kv_bool(o, "ota", id.ota);
  push_kv_uint(o, "max_message", id.max_message);
  push_kv_uint(o, "max_chunk", id.max_chunk);
  push_kv_uint(o, "max_in_flight", id.max_in_flight);
  push_kv_uint(o, "max_fps", id.max_fps);

  // mirror: map(6) MirrorCapability (N03 Form B).
  push_text(o, "mirror");
  push_u8(o, 0xa6);
  push_text(o, "mode");
  push_array_header(o, id.mirror ? 1u : 0u);
  if (id.mirror) push_text(o, "full-frame");
  push_kv_uint(o, "max_fps", id.mirror ? id.mirror_fps : 0);
  push_kv_uint(o, "max_width", id.mirror ? surface.width : 0);
  push_kv_uint(o, "max_height", id.mirror ? surface.height : 0);
  push_text(o, "pixel_formats");
  push_array_header(o, id.mirror ? 1u : 0u);
  if (id.mirror) push_text(o, "RGB565");
  push_kv_null(o, "recommended_transport");

  push_kv_uint(o, "local_display_fps", id.local_display_fps);
  push_kv_uint(o, "mirror_fps", id.mirror ? id.mirror_fps : 0);
  push_kv_uint(o, "mirror_bandwidth", id.mirror_bandwidth);

  // renderer: map(6) RendererCapability (N17). Only when advertised.
  if (id.renderer) {
    push_text(o, "renderer");
    push_u8(o, 0xa6);
    push_kv_bool(o, "sprite_2d", id.renderer_sprite_2d);
    push_kv_bool(o, "mesh2d", id.renderer_mesh2d);
    push_kv_bool(o, "mesh3d", id.renderer_mesh3d);
    push_kv_bool(o, "skeleton", id.renderer_skeleton);
    push_kv_uint(o, "max_texture", id.renderer_max_texture);
    push_kv_uint(o, "max_vertices", id.renderer_max_vertices);
  }
  return o;
}

std::vector<std::uint8_t> encode_ack_cbor(bool received, bool displayed, std::uint64_t frame_id) {
  std::vector<std::uint8_t> o;
  o.reserve(32);
  push_u8(o, 0xa4);  // map(4)
  push_kv_bool(o, "received", received);
  push_kv_bool(o, "displayed", displayed);
  push_kv_uint(o, "frame_id", frame_id);
  push_text(o, "error");
  push_null(o);
  return o;
}

std::vector<std::uint8_t> encode_error_cbor(const std::string& code, const std::string& message) {
  std::vector<std::uint8_t> o;
  o.reserve(64);
  push_u8(o, 0xa2);  // map(2): code, message
  push_kv_text(o, "code", code);
  push_kv_text(o, "message", message);
  return o;
}

std::vector<std::uint8_t> encode_metric_cbor(const MetricReport& metric) {
  std::vector<std::uint8_t> o;
  o.reserve(160);
  // Field order matches the Rust struct; optional fields are omitted, so the map
  // count is computed from what is present.
  std::size_t count = 2;  // name, value
  if (metric.has_unit) count++;
  if (metric.boot_id) count++;
  if (metric.run_id) count++;
  if (metric.event_seq) count++;
  push_u8(o, static_cast<std::uint8_t>(0xa0 + count));
  push_kv_text(o, "name", metric.name);
  push_text(o, "value");
  push_double(o, metric.value);
  if (metric.has_unit) push_kv_text(o, "unit", metric.unit);
  if (metric.boot_id) push_kv_uint(o, "boot_id", metric.boot_id);
  if (metric.run_id) push_kv_uint(o, "run_id", metric.run_id);
  if (metric.event_seq) push_kv_uint(o, "event_seq", metric.event_seq);
  return o;
}

std::vector<std::uint8_t> encode_boot_info_cbor(const BootInfo& boot) {
  std::vector<std::uint8_t> o;
  o.reserve(128);
  std::size_t count = 1;  // firmware
  if (boot.boot_id) count++;
  if (boot.run_id) count++;
  if (boot.has_reset_reason) count++;
  push_u8(o, static_cast<std::uint8_t>(0xa0 + count));
  push_kv_text(o, "firmware", boot.firmware);
  if (boot.boot_id) push_kv_uint(o, "boot_id", boot.boot_id);
  if (boot.run_id) push_kv_uint(o, "run_id", boot.run_id);
  if (boot.has_reset_reason) push_kv_text(o, "reset_reason", boot.reset_reason);
  return o;
}

}  // namespace mdc
