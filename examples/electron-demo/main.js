import { app, BrowserWindow, ipcMain } from "electron";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { MyDeviceCanvas } from "../../sdk/node/index.js";
import {
  colorBarsRgb565,
  decodeMirrorFrame,
  formatError,
  mirrorPanelStatus,
  normalizeBrightness,
  normalizeClip,
  originTile,
  planConnection,
} from "./demo-logic.js";

const root = path.dirname(fileURLToPath(import.meta.url));
let window;
let manager;
let endpoint = "";
let mirrorSubscribed = false;

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
  if (!mirrorSubscribed && typeof manager.onMirrorFrame === "function") {
    mirrorSubscribed = true;
    manager.onMirrorFrame((info) => {
      window?.webContents.send("mdc:mirror-meta", info);
    });
  }
  return manager;
}

ipcMain.handle("mdc:connect", async (_event, requested) => {
  const plan = planConnection(requested);
  if (!plan.ok) {
    endpoint = "";
    return { ok: false, error: plan.error };
  }
  try {
    const active = await ensureSim();
    if (plan.mode === "tcp") {
      await active.connectTcp(plan.address);
    }
    const devices = (await active.devices()).map((device) => device.info);
    endpoint = plan.endpoint;
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

ipcMain.handle("mdc:mirror-start", async (_event, surfaceId = "main", maxFps = 15) => {
  try {
    const active = await ensureSim();
    const requestId = await active.startMirror(surfaceId, { mode: "full-frame", maxFps });
    return { ok: true, requestId };
  } catch (error) {
    return { ok: false, error: formatError(error) };
  }
});

ipcMain.handle("mdc:mirror-stop", async (_event, surfaceId = "main") => {
  try {
    const active = await ensureSim();
    return { ok: true, requestId: await active.stopMirror(surfaceId) };
  } catch (error) {
    return { ok: false, error: formatError(error) };
  }
});

/**
 * Poll the device mirror: drain the manager events (which drives onMirrorFrame)
 * and return the newest decoded frame plus a status line for the panel.
 */
ipcMain.handle("mdc:mirror-poll", async () => {
  try {
    const active = await ensureSim();
    await active.pollEvents();
    if (typeof active.emitMirrorFrame === "function") {
      try {
        await active.emitMirrorFrame();
        await active.pollEvents();
      } catch {
        // A Form A / TCP device has no local emitter; the frame simply stays.
      }
    }
    const frame = await active.takeMirrorFrame();
    const stats = await active.mirrorStats();
    let preview = null;
    if (frame) {
      const decoded = decodeMirrorFrame({
        surfaceId: frame.surfaceId,
        frameId: frame.frameId,
        width: frame.width,
        height: frame.height,
        pixels: new Uint8Array(frame.pixels),
      });
      preview = {
        width: decoded.width,
        height: decoded.height,
        frameId: decoded.frameId,
        surfaceId: decoded.surfaceId,
        rgba: Array.from(decoded.rgba),
      };
    }
    return { ok: true, frame: preview, status: mirrorPanelStatus(stats), stats };
  } catch (error) {
    return { ok: false, error: formatError(error) };
  }
});

ipcMain.handle("mdc:config-get", async (_event, key) => {
  try {
    const active = await ensureSim();
    return { ok: true, value: await active.getConfig(key) };
  } catch (error) {
    return { ok: false, error: formatError(error) };
  }
});

ipcMain.handle("mdc:config-set", async (_event, key, value) => {
  try {
    const accepted = key === "brightness" ? normalizeBrightness(value) : String(value);
    const active = await ensureSim();
    return { ok: true, value: await active.setConfig(key, accepted) };
  } catch (error) {
    return { ok: false, error: formatError(error) };
  }
});

ipcMain.handle("mdc:animation", async (_event, clip) => {
  try {
    const active = await ensureSim();
    const name = normalizeClip(clip);
    if (name === "off") {
      await active.stopAnimation();
      return { ok: true, clip: "off" };
    }
    return { ok: true, clip: await active.setAnimation(name) };
  } catch (error) {
    return { ok: false, error: formatError(error) };
  }
});

ipcMain.handle("mdc:scene", async (_event, node) => {
  try {
    const active = await ensureSim();
    await active.setScene(node);
    return { ok: true };
  } catch (error) {
    return { ok: false, error: formatError(error) };
  }
});

ipcMain.handle("mdc:dispose", () => {
  manager?.dispose();
  manager = undefined;
  mirrorSubscribed = false;
  endpoint = "";
  return { ok: true };
});

app.whenReady().then(createWindow);
app.on("window-all-closed", () => {
  manager?.dispose();
  if (process.platform !== "darwin") app.quit();
});
