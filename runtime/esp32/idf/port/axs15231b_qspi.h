#pragma once
/**
 * AXS15231B QSPI pin map — parameters from AgentDeck board_35_ips.h (MIT),
 * re-expressed for MyDeviceCanvas. Not a copy of upstream Arduino_GFX sources.
 *
 * Hardware init (SPI bus, panel ops) belongs in an ESP-IDF component; host
 * builds use SimDisplay + BoardDisplayAdapter instead.
 */

#define MDC_AXS_PANEL_NATIVE_W 320
#define MDC_AXS_PANEL_NATIVE_H 480
#define MDC_AXS_LOGICAL_W 480
#define MDC_AXS_LOGICAL_H 320
#define MDC_AXS_ROTATION 1
#define MDC_AXS_REQUIRES_CANVAS 1
#define MDC_AXS_QSPI_HZ 32000000

#define MDC_AXS_PIN_QSPI_CS 45
#define MDC_AXS_PIN_QSPI_CLK 47
#define MDC_AXS_PIN_QSPI_D0 21
#define MDC_AXS_PIN_QSPI_D1 48
#define MDC_AXS_PIN_QSPI_D2 40
#define MDC_AXS_PIN_QSPI_D3 39
#define MDC_AXS_PIN_BL 1
#define MDC_AXS_PIN_TE 38

#define MDC_AXS_TOUCH_ADDR 0x3B
#define MDC_AXS_PIN_TOUCH_SDA 4
#define MDC_AXS_PIN_TOUCH_SCL 8
#define MDC_AXS_PIN_TOUCH_INT 11
#define MDC_AXS_PIN_TOUCH_RST 12
