import assert from "node:assert/strict";
import test from "node:test";
import { cursorStyle, LocalCursor } from "../src/local-cursor.ts";

test("cursor metadata cannot inject arbitrary CSS or URLs", () => {
  assert.equal(cursorStyle({ visible: true, shape: "url(https://example.com/track)" }), null);
  assert.equal(cursorStyle({ visible: true, shape: "text", image: { png: 'bad");color:red', x: 0, y: 0 } }), "text");
  assert.equal(cursorStyle({ visible: true, shape: "default", image: { png: "AAAA", x: 128, y: 0 } }), "default");
  assert.equal(cursorStyle({ visible: false, shape: "text" }), "none");
  assert.equal(cursorStyle({ visible: true, shape: "default", image: { png: "AAAA", x: 2, y: 3 } }), 'url("data:image/png;base64,AAAA") 2 3, default');
});

test("local cursor is negotiated, follows metadata, and cleans up on disconnect", () => {
  const channel = new EventTarget();
  const video = { dataset: {}, style: { cursor: "", removeProperty(key) { delete this[key]; } } };
  const cursor = new LocalCursor(video, channel);
  const send = data => channel.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(data) }));
  send({ visible: true, shape: "pointer" });
  assert.equal(video.style.cursor, "none");
  cursor.enable(true);
  assert.equal(video.style.cursor, "default");
  send({ visible: true, shape: "text" });
  assert.equal(video.style.cursor, "text");
  send({ visible: false, shape: "text" });
  assert.equal(video.style.cursor, "none");
  channel.dispatchEvent(new Event("close"));
  assert.equal(video.style.cursor, "default");
  cursor.destroy();
  send({ visible: true, shape: "wait" });
  assert.equal(video.style.cursor, undefined);
  assert.equal(video.dataset.cursorMode, undefined);
});
