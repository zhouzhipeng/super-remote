import assert from "node:assert/strict";
import test from "node:test";
import { LatestPointer, requestInteractivePlayback } from "../src/input-latency.ts";

test("congestion keeps only the newest mouse position", () => {
  const queue = new LatestPointer();
  for (let x = 0; x < 1000; x++) assert.equal(queue.offer({ x, y: 2 }, false), null);
  assert.deepEqual(queue.take(), { x: 999, y: 2 });
  assert.equal(queue.take(), null);
});
test("a new direct send cannot be followed by an older buffered position", () => {
  const queue = new LatestPointer();
  queue.offer({ x: 1, y: 1 }, false);
  assert.deepEqual(queue.offer({ x: 2, y: 2 }, true), { x: 2, y: 2 });
  assert.equal(queue.take(), null);
});
test("interactive video does not request the old 80 ms delay or alter audio", () => {
  const video = { track: { kind: "video" }, jitterBufferTarget: 80, playoutDelayHint: .08 };
  requestInteractivePlayback(video);
  assert.equal(video.jitterBufferTarget, 0);
  assert.equal(video.playoutDelayHint, 0);
  const audio = { track: { kind: "audio" }, jitterBufferTarget: 80 };
  requestInteractivePlayback(audio);
  assert.equal(audio.jitterBufferTarget, 80);
  assert.doesNotThrow(() => requestInteractivePlayback(Object.freeze(video)));
});
