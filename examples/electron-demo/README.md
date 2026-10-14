# Electron Demo

Device access stays in the main process. The page only receives DTOs and errors through the preload bridge.

Buttons: connect simulator, connect an explicit endpoint, refresh devices, send RGB565 color bars, send a 1×1 tile, start/stop Form B mirror, dispose. A non-`sim` endpoint is rejected and does not open a serial port. Color bars are also drawn locally so the page can be checked without Electron.

The mirror panel subscribes to the public `onMirrorFrame` event, polls the device for the newest mirrored frame, and draws it on a canvas via `decodeMirrorFrame` (RGB565→RGBA). It is a preview consumer only; no OBS dependency is linked.

```sh
node --test demo-logic.test.js
cargo build -p mdc-node-native
cp ../../target/debug/libmdc_node_native.dylib ../../sdk/node/mdc-node-native.node
npm install
npm start
```

Without the `.node` artifact, connect returns an error instead of an empty success. This demo is a simulator session, not a physical-board proof. Host image conversion is RGB565; the runtime does not decode JPEG.

License: Apache-2.0
