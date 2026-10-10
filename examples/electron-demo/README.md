# Electron Demo

Device access stays in the main process. The page only receives DTOs and errors through the preload bridge.

Buttons: connect simulator, connect an explicit endpoint, refresh devices, send RGB565 color bars, send a 1×1 tile, dispose. A non-`sim` endpoint is rejected and does not open a serial port. Color bars are also drawn locally so the page can be checked without Electron.

```sh
node --test demo-logic.test.js
cargo build -p mdc-node-native
cp ../../target/debug/libmdc_node_native.dylib ../../sdk/node/mdc-node-native.node
npm install
npm start
```

Without the `.node` artifact, connect returns an error instead of an empty success. This demo is a simulator session, not a physical-board proof. Host image conversion is RGB565; the runtime does not decode JPEG.

License: Apache-2.0
