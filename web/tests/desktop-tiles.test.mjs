import assert from "node:assert/strict";
import test from "node:test";
import { parseTiles, parseCopies } from "../src/desktop-tiles.ts";

function tile(x = 0, y = 0, width = 128, height = 128) {
  const bytes = new Uint8Array(20);
  const view = new DataView(bytes.buffer);
  [x, y, width, height].forEach((value, i) => view.setUint16(i * 2, value, true));
  view.setUint32(8, 8, true);
  return bytes;
}
test("damage packets retain exact region coordinates including screen edges", () => {
  const result = parseTiles(tile(128, 128, 2, 1), 1, 130, 129);
  assert.deepEqual([result[0].x, result[0].y, result[0].width, result[0].height], [128,128,2,1]);
});
test("rejects truncated, trailing and out-of-bounds damage before drawing", () => {
  assert.throws(() => parseTiles(tile().subarray(0, 19), 1, 128, 128));
  assert.throws(() => parseTiles(new Uint8Array(21), 1, 128, 128));
  assert.throws(() => parseTiles(tile(1), 1, 128, 128));
  assert.throws(() => parseTiles(tile(), 0, 128, 128));
  assert.throws(() => parseTiles(tile(), 3000, 128, 128));
});
test("scroll reuse validates both source and destination rectangles", () => {
  const copy = {x:0,y:0,width:128,height:128,source_y:37};
  assert.deepEqual(parseCopies([copy],128,256),[copy]);
  assert.throws(() => parseCopies([{...copy,source_y:-1}],128,256));
  assert.throws(() => parseCopies([{...copy,source_y:200}],128,256));
  assert.throws(() => parseCopies([{...copy,y:200}],128,256));
  assert.throws(() => parseCopies([{...copy,x:0.5}],128,256));
});
