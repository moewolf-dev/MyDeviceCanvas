#pragma once
/**
 * AXS15231B bring-up stages (architecture only).
 * Host CI does not link ESP-IDF QSPI drivers; stages document the required order:
 * bus → panel → Canvas wrapper → DisplayBackend bind → protocol loop.
 * Direct QSPI without Canvas is rejected (AgentDeck experience: black screen).
 */
#include "axs15231b_qspi.h"
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum mdc_panel_stage {
  MDC_PANEL_STAGE_RESET = 0,
  MDC_PANEL_STAGE_QSPI_BUS,
  MDC_PANEL_STAGE_PANEL_OPS,
  MDC_PANEL_STAGE_CANVAS,
  MDC_PANEL_STAGE_BACKLIGHT,
  MDC_PANEL_STAGE_READY
} mdc_panel_stage_t;

typedef struct mdc_panel_bringup {
  mdc_panel_stage_t stage;
  int canvas_ok;
  int error; /* 0 = ok; non-zero = halted */
} mdc_panel_bringup_t;

/** Compile-time pin / geometry sanity (no hardware I/O). */
static inline int mdc_panel_validate_config(void) {
  if (!MDC_AXS_REQUIRES_CANVAS) return -1;
  if (MDC_AXS_LOGICAL_W == 0 || MDC_AXS_LOGICAL_H == 0) return -2;
  if (MDC_AXS_PANEL_NATIVE_W == 0 || MDC_AXS_PANEL_NATIVE_H == 0) return -3;
  if (MDC_AXS_QSPI_HZ == 0) return -4;
  return 0;
}

/**
 * Advance one bring-up stage. On ESP-IDF, each stage will call real drivers;
 * in the stub, we only enforce Canvas-before-READY and config validation.
 */
static inline int mdc_panel_advance(mdc_panel_bringup_t* b) {
  if (!b) return -10;
  if (b->error) return b->error;
  if (b->stage == MDC_PANEL_STAGE_RESET) {
    int v = mdc_panel_validate_config();
    if (v != 0) {
      b->error = v;
      return v;
    }
    b->stage = MDC_PANEL_STAGE_QSPI_BUS;
    return 0;
  }
  if (b->stage == MDC_PANEL_STAGE_QSPI_BUS) {
    /* TODO(ESP-IDF): spi_bus_initialize + QSPI device add */
    b->stage = MDC_PANEL_STAGE_PANEL_OPS;
    return 0;
  }
  if (b->stage == MDC_PANEL_STAGE_PANEL_OPS) {
    /* TODO(ESP-IDF): AXS15231B init sequence */
    b->stage = MDC_PANEL_STAGE_CANVAS;
    return 0;
  }
  if (b->stage == MDC_PANEL_STAGE_CANVAS) {
    /* Canvas wrapper is mandatory for this panel class. */
    b->canvas_ok = MDC_AXS_REQUIRES_CANVAS ? 1 : 0;
    if (!b->canvas_ok) {
      b->error = -5;
      return b->error;
    }
    b->stage = MDC_PANEL_STAGE_BACKLIGHT;
    return 0;
  }
  if (b->stage == MDC_PANEL_STAGE_BACKLIGHT) {
    /* TODO(ESP-IDF): GPIO backlight on MDC_AXS_PIN_BL */
    b->stage = MDC_PANEL_STAGE_READY;
    return 0;
  }
  return 0; /* already READY */
}

static inline int mdc_panel_run_to_ready(mdc_panel_bringup_t* b) {
  if (!b) return -10;
  b->stage = MDC_PANEL_STAGE_RESET;
  b->canvas_ok = 0;
  b->error = 0;
  while (b->stage != MDC_PANEL_STAGE_READY && b->error == 0) {
    int r = mdc_panel_advance(b);
    if (r != 0) return r;
  }
  return b->error;
}

#ifdef __cplusplus
}
#endif
