import { app, BrowserWindow, ipcMain } from "electron";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { MyDeviceCanvas } from "../../sdk/node/index.js";

const root = path.dirname(fileURLToPath(import.meta.url));
let window;
let manager;

function createWindow() {
  window = new BrowserWindow({
    width: 720,
    height: 520,
    webPreferences: { preload: path.join(root, "preload.cjs"), contextIsolation: true, nodeIntegration: false },
  });
  window.loadFile(path.join(root, "renderer.html"));
}

ipcMain.handle("mdc:devices", async () => {
  // Prefer NativeManager simulate path when the .node artifact is built;
  // otherwise the JS facade returns [] (honest sim / empty hardware state).
  manager ??= await MyDeviceCanvas.create();
  return (await manager.devices()).map((device) => device.info);
});

ipcMain.handle("mdc:dispose", () => { manager?.dispose(); manager = undefined; });
app.whenReady().then(createWindow);
app.on("window-all-closed", () => { manager?.dispose(); if (process.platform !== "darwin") app.quit(); });
