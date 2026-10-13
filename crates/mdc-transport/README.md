# mdc-transport

Byte-channel trait plus Memory, Serial, WebSocket, and TCP adapters.
Does not parse protocol business messages.

TCP carries Wi-Fi and gigabit Ethernet links. BLE GATT, native USB bulk and
PCIe are **reserved, not implemented**; see `docs/transport-scope.md`. A
reserved adapter is not a feature.
