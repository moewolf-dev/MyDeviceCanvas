import { app, BrowserWindow, ipcMain } from "electron";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { MyDeviceCanvas } from "../../sdk/node/index.js";
import { colorBarsRgb565, formatError, originTile, rejectNonSimEndpoint } from "./demo-logic.js";

const root = path.dirname(fileURLToPath(import.meta.url));
let window;
let manager;
let endpoint = "";

function createWindow() {
  window = new BrowserWindow({
    width: 860,
    height: 640,
    webPreferences: {
      preload: path.join(root, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  window.loadFile(path.join(root, "renderer.html"));
}

async function ensureSim() {
  manager ??= await MyDeviceCanvas.create();
  if (!manager.nativeLoaded()) {
    throw new Error("未加载 Node 原生 SDK（缺少 mdc-node-native.node）。请先 cargo build -p mdc-node-native 并复制为 sdk/node/mdc-node-native.node");
  }
  return manager;
}

ipcMain.handle("mdc:connect", async (_event, requested) => {
  const rejected = rejectNonSimEndpoint(requested);
  if (rejected) {
    endpoint = "";
    return { ok: false, error: rejected };
  }
  try {
    const active = await ensureSim();
    const devices = (await active.devices()).map((device) => device.info);
    endpoint = "sim";
    return { ok: true, endpoint, devices };
  } catch (error) {
    endpoint = "";
    return { ok: false, error: formatError(error) };
  }
});

ipcMain.handle("mdc:devices", async () => {
  const active = await ensureSim();
  return {
    endpoint,
    devices: (await active.devices()).map((device) => device.info),
  };
});

ipcMain.handle("mdc:send-bars", async () => {
  try {
    const active = await ensureSim();
    const device = (await active.devices())[0];
    const surface = device?.surface("main") ?? [...(device?.surfaces.values() ?? [])][0];
    if (!surface) return { ok: false, error: "没有可写的 Surface" };
    const { width, height } = surface.info;
    const frame = colorBarsRgb565(width, height);
    const requestId = await surface.sendFrame(frame);
    return { ok: true, requestId, width, height, bytes: frame.length };
  } catch (error) {
    return { ok: false, error: formatError(error) };
  }
});

ipcMain.handle("mdc:send-tile", async () => {
  try {
    const active = await ensureSim();
    const device = (await active.devices())[0];
    const surface = device?.surface("main") ?? [...(device?.surfaces.values() ?? [])][0];
    if (!surface) return { ok: false, error: "没有可写的 Surface" };
    const frameId = await active.currentFrameId();
    if (!frameId) return { ok: false, error: "还没有已显示的全帧，不能发送 Tile" };
    const tileBytes = originTile(colorBarsRgb565(surface.info.width, 1), surface.info.width);
    const requestId = await surface.sendTile({
      x: 0,
      y: 0,
      width: 1,
      height: 1,
      baseFrameId: frameId,
      bytes: tileBytes,
    });
    return { ok: true, requestId, baseFrameId: frameId };
  } catch (error) {
    return { ok: false, error: formatError(error) };
  }
});

ipcMain.handle("mdc:dispose", () => {
  manager?.dispose();
  manager = undefined;
  endpoint = "";
  return { ok: true };
});

app.whenReady().then(createWindow);
app.on("window-all-closed", () => {
  manager?.dispose();
  if (process.platform !== "darwin") app.quit();
});
