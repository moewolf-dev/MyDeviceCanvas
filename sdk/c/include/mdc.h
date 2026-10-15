/* MyDeviceCanvas C ABI — Apache-2.0
 * Opaque handles; create/destroy; send_frame / send_tile; free.
 */
#ifndef MDC_H
#define MDC_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct MdcSession MdcSession;

/** Create a simulator-backed session (FakeDevice / MemoryLink). Returns NULL on failure. */
MdcSession* mdc_session_create(void);

/** Destroy session and release peer. Safe on NULL. */
void mdc_session_destroy(MdcSession* session);

/**
 * Send RGB565 frame to surface "main" (default board geometry).
 * Returns 0 on success, negative on error.
 */
int mdc_session_send_frame(MdcSession* session, const uint8_t* bytes, size_t len);

/**
 * Send RGB565 tile to surface "main".
 * Returns 0 on success; -4 if NeedFullFrame; other negatives on error.
 */
int mdc_session_send_tile(MdcSession* session, uint64_t base_frame_id, uint16_t x, uint16_t y,
                          uint16_t width, uint16_t height, const uint8_t* bytes, size_t len);

/** Peer current frame id after a successful displayed frame (for tile base). */
uint64_t mdc_session_current_frame_id(const MdcSession* session);

/** Write surface "main" width/height; returns 0 or -1. */
int mdc_session_surface_size(const MdcSession* session, uint16_t* width, uint16_t* height);

/** Number of surfaces advertised by the current device. 0 if not ready. */
uint32_t mdc_session_surface_count(const MdcSession* session);

/**
 * Write one surface id and size. index 0 is main on the default board.
 * Returns 0, -1 on a bad pointer, or -2 when the index is out of range.
 */
int mdc_session_surface_at(const MdcSession* session, uint32_t index, char* id, size_t id_len,
                           uint16_t* width, uint16_t* height);

/** Simulator session for a board id such as "dual-sim". NULL uses the default board. */
MdcSession* mdc_session_create_board(const char* board_id);

/** Non-zero when tiles are blocked until a full frame ACK (after reconnect/switch). */
int mdc_session_needs_full_frame(const MdcSession* session);

/** Copy NUL-terminated device id into buf; returns bytes written (excl. NUL) or negative. */
int mdc_session_device_id(MdcSession* session, char* buf, size_t buflen);

/**
 * v1 Scene API. Always returns -5 (unsupported). Does not write a Scene packet.
 * `bytes` may be NULL when `len` is 0.
 */
int mdc_session_send_scene(MdcSession* session, const uint8_t* bytes, size_t len);

/** Connect to a TCP simulator peer (`host:port`). Returns NULL on failure. */
MdcSession* mdc_session_connect_tcp(const char* addr);

/**
 * Write one queued event.
 * kind: 1 input, 2 error, 3 connected, 4 disconnected, 5 endpoint switch.
 * Returns 1 when an event was written, 0 when empty, negative on error.
 */
int mdc_session_poll_event(MdcSession* session, int* kind, char* device_id, size_t device_id_len,
                           char* phase, size_t phase_len, uint16_t* x, uint16_t* y);

/** Same as poll_event, and writes the input pointer id when the event is a touch. */
int mdc_session_poll_event_with_pointer(MdcSession* session, int* kind, char* device_id,
                                        size_t device_id_len, char* phase, size_t phase_len,
                                        uint16_t* pointer_id, uint16_t* x, uint16_t* y);

/** Simulator-only touch injection. Returns 0, or negative when the session is not a FakeDevice. */
int mdc_session_simulate_input(MdcSession* session, const char* phase, uint16_t x, uint16_t y);

/** Form B: ask the device to start mirroring `surface_id` (NULL = "main"). 0 or negative. */
int mdc_start_mirror(MdcSession* session, const char* surface_id, uint16_t max_fps);

/** Form B: stop mirroring. 0 or negative. */
int mdc_stop_mirror(MdcSession* session, const char* surface_id);

/** Simulator-only: emit one mirrored frame from the FakeDevice. 0 or negative. */
int mdc_emit_mirror_frame(MdcSession* session);

/**
 * Copy the newest complete mirrored frame (RGB565). Writes width/height/frame_id
 * when the pointers are non-NULL. With `out == NULL`, returns the byte count;
 * otherwise copies up to `cap` bytes and returns the byte count, or -2 if `cap`
 * is too small, or -1 when no frame is available.
 */
int64_t mdc_take_mirror_frame(MdcSession* session, uint16_t* width, uint16_t* height,
                              uint64_t* frame_id, uint8_t* out, size_t cap);

/** Form B: mirror counters. Writes received/dropped; returns 0 or -1. */
int mdc_mirror_stats(const MdcSession* session, uint64_t* received, uint64_t* dropped);

/**
 * Set a vector scene from a NUL-terminated JSON SceneNode.
 * Returns 0, -1 on a bad pointer/session, or -2 on malformed JSON.
 */
int mdc_set_scene(MdcSession* session, const char* json);

/** Clear the vector scene. Returns 0 or -1. */
int mdc_clear_scene(MdcSession* session);

/**
 * N243: upload a runtime package (`.mdcpkg` bytes).
 * Returns 0 on send, -1 bad session, or -2 bad bytes. Acceptance is async:
 * poll `mdc_package_ready` before treating the renderer as ready.
 */
int mdc_install_package(MdcSession* session, const uint8_t* bytes, size_t len);

/** N243: 1 when the last accepted package is render-ready, 0 otherwise, -1 bad session. */
int mdc_package_ready(MdcSession* session);

/**
 * N244: read a config value into `out` (NUL-terminated). Returns the length, or
 * -1 bad session, -2 bad args, -3 unknown key, -4 buffer too small.
 */
int mdc_get_config(MdcSession* session, const char* key, char* out, size_t out_len);

/** N244: write a config value. Returns 0, -1 bad session, -2 bad args, -3 rejected. */
int mdc_set_config(MdcSession* session, const char* key, const char* value);

/** Free a heap buffer returned by future helpers (no-op placeholder for symmetry). */
void mdc_free(void* ptr);

#ifdef __cplusplus
}
#endif

#endif /* MDC_H */
