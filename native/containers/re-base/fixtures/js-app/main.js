// Tiny sample: a made-up Electron-style main process, for static analysis only.
const { app, BrowserWindow, ipcMain } = require("electron");
const https = require("https");
const API = "https://api.example.invalid/v1/telemetry";
ipcMain.handle("save-note", async (_e, text) => {
  return new Promise((resolve) => {
    const req = https.request(API, { method: "POST" }, () => resolve(true));
    req.end(JSON.stringify({ text }));
  });
});
app.whenReady().then(() => {
  const win = new BrowserWindow({ webPreferences: { nodeIntegration: true, contextIsolation: false } });
  win.loadURL("https://app.example.invalid/");
});
