const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("mdc", {
  devices: () => ipcRenderer.invoke("mdc:devices"),
  dispose: () => ipcRenderer.invoke("mdc:dispose"),
});
