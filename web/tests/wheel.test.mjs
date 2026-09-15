import assert from "node:assert/strict";
import test from "node:test";
import { WheelDelta } from "../src/wheel.ts";

test("pixel and line mouse wheels produce a full Windows notch immediately", () => {
  assert.deepEqual(new WheelDelta().convert(0, 100, 0), {x: 0, y: -120});
  assert.deepEqual(new WheelDelta().convert(0, 3, 1), {x: 0, y: -120});
  assert.deepEqual(new WheelDelta().convert(0, -1, 2), {x: 0, y: 120});
});
test("fractional trackpad deltas are accumulated without inflating every event", () => {
  const wheel = new WheelDelta();
  assert.deepEqual(wheel.convert(.25, .25, 0), {x: 0, y: -0});
  let x = 0, y = 0;
  for (let i = 0; i < 9; i++) { const delta = wheel.convert(.25, .25, 0); x += delta.x; y += delta.y; }
  assert.ok(x >= 2 && x <= 3);
  assert.equal(y, -x);
});
test("reversing direction does not spend the previous direction's remainder", () => {
  const wheel = new WheelDelta();
  // Leaves +0.6 of a unit owed downward.
  assert.deepEqual(wheel.convert(0, -0.5, 0), {x: 0, y: 0});
  // The first upward unit must arrive on the event that earns it, not one later.
  assert.deepEqual(wheel.convert(0, 1, 0), {x: 0, y: -1});
  // A same-direction remainder is still carried.
  const same = new WheelDelta();
  assert.deepEqual(same.convert(0, 0.5, 0), {x: 0, y: -0});
  assert.deepEqual(same.convert(0, 0.5, 0), {x: 0, y: -1});
});
test("horizontal scrolling retains Windows rightward sign and rejects invalid values", () => {
  assert.deepEqual(new WheelDelta().convert(100, 0, 0), {x: 120, y: 0});
  assert.deepEqual(new WheelDelta().convert(NaN, Infinity, 0), {x: 0, y: 0});
});
