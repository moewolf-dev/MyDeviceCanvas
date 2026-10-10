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

/** Non-zero when tiles are blocked until a full frame ACK (after reconnect/switch). */
int mdc_session_needs_full_frame(const MdcSession* session);

/** Copy NUL-terminated device id into buf; returns bytes written (excl. NUL) or negative. */
int mdc_session_device_id(MdcSession* session, char* buf, size_t buflen);

/**
 * v1 Scene API. Always returns -5 (unsupported). Does not write a Scene packet.
 * `bytes` may be NULL when `len` is 0.
 */
int mdc_session_send_scene(MdcSession* session, const uint8_t* bytes, size_t len);

/** Free a heap buffer returned by future helpers (no-op placeholder for symmetry). */
void mdc_free(void* ptr);

#ifdef __cplusplus
}
#endif

#endif /* MDC_H */
