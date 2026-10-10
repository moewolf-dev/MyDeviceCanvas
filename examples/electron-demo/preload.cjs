const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("mdc", {
  connect: (endpoint) => ipcRenderer.invoke("mdc:connect", endpoint),
  devices: () => ipcRenderer.invoke("mdc:devices"),
  sendBars: () => ipcRenderer.invoke("mdc:send-bars"),
  sendTile: () => ipcRenderer.invoke("mdc:send-tile"),
  dispose: () => ipcRenderer.invoke("mdc:dispose"),
});
