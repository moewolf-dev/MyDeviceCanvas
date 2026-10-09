# H06 Sim Acceptance — install → discover → display → input → OTA

Host / FakeDevice path only. No physical board claims.

## Flow

1. **Install (provision)**  
   `SimFlasher` (`tools/flasher`) implements `mdc_provision::Flasher`: fake image write, reset, identity readout. `Installer::run` must reach `Succeeded` with matching `expected_device_id`.

2. **Discover**  
   `MockMdnsProvider` returns seeded candidates (Memory + WebSocket labels). `StaticDiscovery` remains available for explicit endpoint lists.

3. **Display**  
   `mdc simulate` / `FakeDevice` + `Session::send_frame` completes HELLO → Capabilities → Frame → Ack. Pixel buffer assertable via `pixels()`.

4. **Input**  
   `FakeDevice::emit_input` (touch capability on) → host `Session::poll` → `ManagerEvent::Input`.

5. **OTA**  
   `Session::send_ota` gated by `capabilities.ota`. Simulator accepts `begin` / `abort` / `confirm` and ACKs; disabled `ota` → `CoreError::Unsupported` / device `E_UNSUPPORTED`.

## Commands

```sh
cargo test -p mdc-flasher
cargo test -p mdc-discovery
cargo test -p mdc-simulator emit_input
cargo test -p mdc-simulator ota_gated
cargo run -p mdc -- simulate --board esp32-jc3248w535-sim

# End-to-end H06 orchestration (sim):
cargo run -p mdc -- h06-demo

# TCP peer for --address (separate terminal):
cargo run -p mdc-sim-peer -- 127.0.0.1:9876 esp32-jc3248w535-sim
cargo run -p mdc -- --address 127.0.0.1:9876 send-image ./some.png
```

## Related (H01 / I03)

```sh
cargo run -p mdc -- pair demo-device secret-token
cargo run -p mdc -- authorize demo-device ws ws://127.0.0.1/mdc
cargo run -p mdc -- authorize demo-device tcp 127.0.0.1:9876
cargo run -p mdc -- unpair demo-device
cargo run -p mdc -- switch-demo
# After handshake, TCP send-image refuses unpaired device_id:
# cargo run -p mdc -- pair sim-esp32-jc3248w535 tok
# cargo run -p mdc -- --address 127.0.0.1:9876 send-image ./x.png
```

WS/TCP require a stored pairing credential before Frame/Tile/OTA; Serial/Memory do not. After `switch_transport`, tiles are blocked until a full frame receives a displayed ACK.

## Acceptance

All five stages pass under `cargo test --workspace` / CLI simulate. Mark **sim-verified**; physical remains pending.
