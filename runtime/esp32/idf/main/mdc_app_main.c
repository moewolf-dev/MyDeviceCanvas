#include "axs15231b_qspi.h"

#if defined(ESP_PLATFORM)
#include <stdio.h>

void app_main(void) {
  printf("mdc: AXS15231B logical %dx%d canvas=%d\n", MDC_AXS_LOGICAL_W, MDC_AXS_LOGICAL_H,
         MDC_AXS_REQUIRES_CANVAS);
  /* TODO: init QSPI+Canvas, bind DisplayBackend, run protocol session loop. */
}
#else
/* Host builds may compile this TU as an anchor for the pin header. */
int mdc_idf_stub_anchor(void) { return MDC_AXS_REQUIRES_CANVAS; }
#endif
