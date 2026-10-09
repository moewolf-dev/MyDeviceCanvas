# Hardware Validation — linux-virt

Status: **sim-verified** (not physical)

| Item | Result |
|---|---|
| Board | `boards/sim/linux-virt` |
| Surface | 800×480 RGB565 LE framebuffer peer |
| Runtime | `runtime/linux` + shared `SimDisplay` HAL |
| physical | pending |

```sh
c++ -std=c++17 -I runtime/linux/include -I runtime/esp32/include \
  runtime/esp32/src/*.cpp runtime/linux/src/linux_runtime.cpp \
  runtime/linux/tests/linux_runtime_test.cpp -o /tmp/mdc_linux_runtime_tests
/tmp/mdc_linux_runtime_tests
```
