import test from "node:test";
import assert from "node:assert/strict";
import { colorBarsRgb565, formatError, originTile, planConnection, rejectNonSimEndpoint } from "./demo-logic.js";

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
  assert.equal(rejectNonSimEndpoint("tcp:127.0.0.1:9876"), null);
  assert.match(rejectNonSimEndpoint("/dev/cu.usbmodem"), /未打开/);
});

test("tcp plan forwards the address and serial plans open no port", () => {
  const tcp = planConnection("tcp:127.0.0.1:9876");
  assert.equal(tcp.ok, true);
  assert.equal(tcp.mode, "tcp");
  assert.equal(tcp.address, "127.0.0.1:9876");
  assert.equal(tcp.openedPort, false);
  const sim = planConnection("sim");
  assert.equal(sim.mode, "sim");
  assert.equal(sim.openedPort, false);
  const serial = planConnection("/dev/cu.usbmodem");
  assert.equal(serial.ok, false);
  assert.equal(serial.openedPort, false);
  assert.equal(serial.address, undefined);
});

test("formatError prefers Error.message", () => {
  assert.equal(formatError(new Error("未连接")), "未连接");
});
