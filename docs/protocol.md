# MyDeviceCanvas Protocol v1

The wire format is a byte stream. Every packet starts with the 20-byte little-endian header:

| offset | size | field |
|---:|---:|---|
| 0 | 2 | magic `MD` |
| 2 | 1 | major |
| 3 | 1 | minor |
| 4 | 1 | message type |
| 5 | 1 | reserved, zero |
| 6 | 2 | flags |
| 8 | 4 | request ID |
| 12 | 4 | payload length |
| 16 | 4 | FNV-1a-style integrity checksum |

Integers are little-endian. Control payloads are CBOR and are limited to 4096 bytes. Frame and tile payloads are raw RGB565 little-endian bytes, never base64. The default chunk limit is 16 KiB. A receiver must reject invalid magic, unsupported major versions, unknown message types, lengths above its negotiated limit, truncation and checksum failures without allocating the advertised payload first.

Supported types are HELLO, CAPABILITIES, FRAME, TILE, INPUT, OTA, PING, PONG, ACK and ERROR. Input and OTA are reserved in 0.1 and return `Unsupported` unless capabilities explicitly enable them.
