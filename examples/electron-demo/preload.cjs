const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("mdc", {
  connect: (endpoint) => ipcRenderer.invoke("mdc:connect", endpoint),
  devices: () => ipcRenderer.invoke("mdc:devices"),
  sendBars: () => ipcRenderer.invoke("mdc:send-bars"),
  sendTile: () => ipcRenderer.invoke("mdc:send-tile"),
  mirrorStart: (surfaceId, maxFps) => ipcRenderer.invoke("mdc:mirror-start", surfaceId, maxFps),
  mirrorStop: (surfaceId) => ipcRenderer.invoke("mdc:mirror-stop", surfaceId),
  mirrorPoll: () => ipcRenderer.invoke("mdc:mirror-poll"),
  onMirrorMeta: (listener) => {
    const wrapped = (_event, info) => listener(info);
    ipcRenderer.on("mdc:mirror-meta", wrapped);
    return () => ipcRenderer.removeListener("mdc:mirror-meta", wrapped);
  },
  dispose: () => ipcRenderer.invoke("mdc:dispose"),
});
