import test from "node:test";
import assert from "node:assert/strict";
import { colorBarsRgb565, formatError, originTile, rejectNonSimEndpoint } from "./demo-logic.js";

test("color bars are RGB565 little-endian and start with red", () => {
  const frame = colorBarsRgb565(8, 1);
  assert.equal(frame.length, 16);
  assert.equal(frame[0], 0x00);
  assert.equal(frame[1], 0xf8);
});

test("origin tile copies the first pixel only", () => {
  const frame = colorBarsRgb565(4, 2);
  const tile = originTile(frame, 4);
  frame[0] = 9;
  assert.deepEqual([...tile], [0x00, 0xf8]);
});

test("non-sim endpoints are refused before any port is opened", () => {
  assert.equal(rejectNonSimEndpoint("sim"), null);
  assert.match(rejectNonSimEndpoint("/dev/cu.usbmodem"), /未打开/);
});

test("formatError prefers Error.message", () => {
  assert.equal(formatError(new Error("未连接")), "未连接");
});
