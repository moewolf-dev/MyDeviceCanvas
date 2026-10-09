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
```

## Acceptance

All five stages pass under `cargo test --workspace` / CLI simulate. Mark **sim-verified**; physical remains pending.
