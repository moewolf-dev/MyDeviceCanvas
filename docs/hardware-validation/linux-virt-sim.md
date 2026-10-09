# Hardware Validation — linux-virt

Status: **sim-verified** (not physical)

| Item | Result |
|---|---|
| Board | `boards/sim/linux-virt` |
| Surface | 800×480 RGB565 framebuffer peer |
| Runtime | `runtime/linux` + shared `SimDisplay` HAL + CBOR Capabilities/Ack |
| TCP peer | `mdc_linux_peer` (C++), host `mdc --address` |
| physical | pending |

```sh
cmake -S runtime/linux -B /tmp/mdc-linux-rt && cmake --build /tmp/mdc-linux-rt
ctest --test-dir /tmp/mdc-linux-rt --output-on-failure

# Terminal A — C++ peer (default 127.0.0.1:9877 @ 800×480)
/tmp/mdc-linux-rt/mdc_linux_peer

# Terminal B — pair then push a frame
cargo run -p mdc -- pair sim-linux-virt lab-token
cargo run -p mdc -- --address 127.0.0.1:9877 send-image ./some.png
```
