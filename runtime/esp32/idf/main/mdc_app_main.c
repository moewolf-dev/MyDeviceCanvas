#include "axs15231b_qspi.h"
#include "mdc_panel_init.h"

#if defined(ESP_PLATFORM)
#include <stdio.h>

void app_main(void) {
  mdc_panel_bringup_t bringup = {0};
  int rc = mdc_panel_run_to_ready(&bringup);
  printf(
      "mdc: AXS15231B logical %dx%d canvas=%d stage=%d rc=%d\n",
      MDC_AXS_LOGICAL_W, MDC_AXS_LOGICAL_H, MDC_AXS_REQUIRES_CANVAS,
      (int)bringup.stage, rc);
  if (rc != 0) {
    printf("mdc: panel bring-up halted; refusing protocol loop\n");
    return;
  }
  /* TODO: bind BoardDisplayAdapter / DisplayBackend, run protocol session loop. */
}
#else
/* Host builds compile this TU as an anchor for pin + bring-up headers. */
int mdc_idf_stub_anchor(void) {
  mdc_panel_bringup_t bringup = {0};
  int rc = mdc_panel_run_to_ready(&bringup);
  if (rc != 0) return rc;
  if (bringup.stage != MDC_PANEL_STAGE_READY) return -20;
  if (!bringup.canvas_ok) return -21;
  return MDC_AXS_REQUIRES_CANVAS;
}
#endif
