import assert from "node:assert/strict";
import test from "node:test";
import { detectScroll, predictionOffset } from "../src/scroll-motion.ts";
const w = 160, h = 96;
function fixture(shift = 0) {
  const pixels = new Uint8Array(w * h);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    pixels[y * w + x] = x < 20 || x >= 140 || y < 12 || y >= 84 ? 40 : ((x * 731 + (y - shift) * 173 + x * (y - shift) * 31) & 255);
  }
  return pixels;
}
test("identifies scrolling content without including fixed desktop chrome", () => {
  for (const dy of [-7, -3, 4, 9]) {
    const motion = detectScroll(fixture(), fixture(dy), w, h);
    assert.ok(motion);
    assert.equal(motion.dy, dy);
    assert.ok(motion.region.x >= 20 && motion.region.y >= 12);
    assert.ok(motion.region.x + motion.region.width <= 140);
    assert.ok(motion.region.y + motion.region.height <= 84);
  }
});
test("abstains on static frames, blank areas and unrelated scene changes", () => {
  assert.equal(detectScroll(fixture(), fixture(), w, h), null);
  assert.equal(detectScroll(new Uint8Array(w*h), new Uint8Array(w*h), w, h), null);
  const changed = fixture().map(value => 255-value);
  assert.equal(detectScroll(fixture(), changed, w, h), null);
});
test("prediction has strict distance/time limits and never moves against input", () => {
  assert.equal(predictionOffset(-1, 100, 20, -1), -6);
  assert.equal(Math.abs(predictionOffset(-.1, 20, 20, -1)), 0);
  assert.equal(predictionOffset(-.1, 44, 44, -1), -2);
  assert.equal(predictionOffset(-1, 141, 20, -1), 0);
  assert.equal(predictionOffset(-1, 20, 121, -1), 0);
  assert.equal(predictionOffset(-1, 20, 20, 1), 0);
});
