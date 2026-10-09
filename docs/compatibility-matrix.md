# Compatibility Matrix

States: `architecture` | `sim-verified` | `physical: pending`

| Surface | Host / Sim | Physical |
|---|---|---|
| Protocol v1 HELLO / Capabilities | sim-verified | physical: pending |
| Frame (legacy + chunked) | sim-verified | physical: pending |
| Tile + base_frame_id | sim-verified | physical: pending |
| MemoryLink Session | sim-verified | n/a |
| TcpTransport | sim-verified (loopback unit) | physical: pending |
| FakeDevice 480×320 baseline | sim-verified | n/a |
| CLI `mdc simulate` | sim-verified | n/a |
| Major version mismatch reject | sim-verified | n/a |
| Low max_chunk negotiation | sim-verified | n/a |
| InputEvent → ManagerEvent::Input | sim-verified | physical: pending |
| OTA command capability gate | sim-verified | physical: pending |
| Discovery (Static + Mock mDNS) | sim-verified | physical: pending |
| Provision / SimFlasher | sim-verified | physical: pending |
| Node NativeManager sim | architecture / sim when `.node` built | physical: pending |
| C ABI `mdc_c` | architecture | physical: pending |
| Linux runtime framebuffer peer | sim-verified (`mdc_linux_peer` TCP) | physical: pending |
| Scene capability bit | architecture (reserved) | physical: pending |

## G03 sim exit

Host Core + FakeDevice path demonstrates HELLO → Capabilities → Frame → Ack without physical hardware. Marked **sim-verified**; G03 physical exit remains open.
