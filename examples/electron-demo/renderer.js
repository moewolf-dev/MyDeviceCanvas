import { colorBarsRgb565, rgb565ToRgba } from "./demo-logic.js";

const output = document.querySelector("#output");
const canvas = document.querySelector("#preview");
const endpoint = document.querySelector("#endpoint");

function show(value) {
  output.textContent = typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

function drawPreview(width, height) {
  const frame = colorBarsRgb565(width, height);
  const rgba = rgb565ToRgba(frame, width, height);
  const ctx = canvas.getContext("2d");
  const image = new ImageData(rgba, width, height);
  canvas.width = width;
  canvas.height = height;
  ctx.putImageData(image, 0, 0);
  return frame;
}

function bridge() {
  return window.mdc;
}

document.querySelector("#connect").onclick = async () => {
  drawPreview(240, 120);
  if (!bridge()) {
    show("当前页面没有 Electron preload。本地已画出 RGB565 色条，但没有连接设备。");
    return;
  }
  show(await bridge().connect("sim"));
};

document.querySelector("#connect-endpoint").onclick = async () => {
  if (!bridge()) {
    show(`没有 preload，无法连接端点 ${endpoint.value}`);
    return;
  }
  show(await bridge().connect(endpoint.value));
};

document.querySelector("#refresh").onclick = async () => {
  if (!bridge()) {
    show("没有 preload，设备列表不可用。");
    return;
  }
  try {
    show(await bridge().devices());
  } catch (error) {
    show(error.message);
  }
};

document.querySelector("#bars").onclick = async () => {
  drawPreview(240, 120);
  if (!bridge()) {
    show("已在页面预览 240×120 色条。发送需要 Electron 主进程。");
    return;
  }
  show(await bridge().sendBars());
};

document.querySelector("#tile").onclick = async () => {
  if (!bridge()) {
    show("没有 preload，不能发送 Tile。");
    return;
  }
  show(await bridge().sendTile());
};

document.querySelector("#dispose").onclick = async () => {
  if (!bridge()) {
    show("没有可释放的主进程会话。");
    return;
  }
  show(await bridge().dispose());
};
