#include "mdc_panel_init.h"
#include <stdio.h>

extern int mdc_idf_stub_anchor(void);

int main(void) {
  mdc_panel_bringup_t b = {0};
  if (mdc_panel_validate_config() != 0) {
    fprintf(stderr, "validate_config failed\n");
    return 1;
  }
  if (mdc_panel_run_to_ready(&b) != 0) {
    fprintf(stderr, "run_to_ready failed error=%d\n", b.error);
    return 2;
  }
  if (b.stage != MDC_PANEL_STAGE_READY || !b.canvas_ok) {
    fprintf(stderr, "not ready stage=%d canvas=%d\n", (int)b.stage, b.canvas_ok);
    return 3;
  }
  if (mdc_idf_stub_anchor() != 1) {
    fprintf(stderr, "stub anchor failed\n");
    return 4;
  }
  printf("panel_bringup_test ok\n");
  return 0;
}
