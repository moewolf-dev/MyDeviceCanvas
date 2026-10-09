# Electron Demo

The Electron application keeps device access in the main process and exposes only DTOs and lifecycle operations through the minimal preload bridge.

## Sim path (current)

`main.js` calls `MyDeviceCanvas.create()`, which loads `sdk/node/native` when a built `.node` artifact is present. That native layer opens a **FakeDevice / MemoryLink** session — not physical USB/Wi‑Fi hardware.

```sh
# optional: build native addon (requires napi tooling)
# then from this directory:
npm install
npm start
```

Without a native artifact, `devices()` returns an empty list. This demo is **sim-verified UI/IPC**, not a physical-board proof.

License: Apache-2.0
