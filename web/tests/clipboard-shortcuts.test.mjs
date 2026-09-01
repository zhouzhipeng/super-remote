import assert from "node:assert/strict";
import test from "node:test";

import { ClipboardShortcutRouter } from "../src/clipboard-shortcuts.ts";

test("uses the Host clipboard for copy then paste in one remote session", () => {
  const router = new ClipboardShortcutRouter();
  router.markRemoteCopy();
  assert.equal(router.beginPaste(), "host");
  assert.equal(router.endPaste(), "host");
  assert.equal(router.beginPaste(), "browser");
});

test("uses the browser clipboard after focus leaves the remote page", () => {
  const router = new ClipboardShortcutRouter();
  router.markRemoteCopy();
  router.reset();
  assert.equal(router.beginPaste(), "browser");
  assert.equal(router.endPaste(), "browser");
});

test("keeps the selected route for repeated keydown events", () => {
  const router = new ClipboardShortcutRouter();
  router.markRemoteCopy();
  assert.equal(router.beginPaste(), "host");
  assert.equal(router.beginPaste(), "host");
  assert.equal(router.endPaste(), "host");
});
