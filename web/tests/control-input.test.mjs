import assert from "node:assert/strict";
import test from "node:test";
import { ControlInputChannel } from "../src/control-input.ts";
class Socket extends EventTarget {
  readyState = "open";
  bufferedAmount = 0;
  sent = [];
  send(value) { this.sent.push(value); }
}
test("input uses its authenticated session's non-media socket and exact view bytes", () => {
  const socket = new Socket();
  const input = new ControlInputChannel(socket, "session");
  input.send(new Uint8Array([99, 1, 2, 99]).subarray(1, 3));
  assert.deepEqual(socket.sent, [{ type: "input_packet", session_id: "session", data: [1, 2] }]);
  let count = 0;
  input.addEventListener("message", event => { count++; assert.deepEqual([...new Uint8Array(event.data)], [1, 2]); });
  socket.dispatchEvent(new CustomEvent("signal", { detail: { type: "input_ack", session_id: "other", data: [3] } }));
  assert.equal(count, 0);
  socket.dispatchEvent(new CustomEvent("signal", { detail: { type: "input_ack", session_id: "session", data: [1, 2] } }));
  assert.equal(count, 1);
  input.close(); input.send(new Uint8Array([4]));
  assert.equal(socket.sent.length, 1);
});
test("mouse drain notification follows socket backpressure without a permanent timer", async () => {
  const socket = new Socket(); socket.bufferedAmount = 120;
  const input = new ControlInputChannel(socket, "session");
  assert.equal(input.bufferedAmount, 120);
  const drained = new Promise(resolve => input.addEventListener("bufferedamountlow", resolve, { once: true }));
  socket.bufferedAmount = 0;
  await drained;
  input.close();
});
