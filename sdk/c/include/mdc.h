/* MyDeviceCanvas C ABI — Apache-2.0
 * Opaque handles; create/destroy; send_frame; free.
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

/** Copy NUL-terminated device id into buf; returns bytes written (excl. NUL) or -1. */
int mdc_session_device_id(MdcSession* session, char* buf, size_t buflen);

/** Free a heap buffer returned by future helpers (no-op placeholder for symmetry). */
void mdc_free(void* ptr);

#ifdef __cplusplus
}
#endif

#endif /* MDC_H */
