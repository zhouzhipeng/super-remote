import assert from "node:assert/strict";
import test from "node:test";

import { ClipboardShortcutRouter } from "../src/clipboard-shortcuts.ts";

test("uses the Host clipboard for copy then paste in one remote session", () => {
  const router = new ClipboardShortcutRouter();
  router.markRemoteCopy();
  assert.equal(router.beginPaste(), "host");
  assert.equal(router.endPaste(), "host");
});

test("one copy on the Host serves every paste that follows it", () => {
  // Copying once and pasting repeatedly is ordinary. Falling back to the
  // browser route on the second paste does not merely read the wrong
  // clipboard - it pushes the browser's contents to the Host, destroying what
  // was copied there.
  const router = new ClipboardShortcutRouter();
  router.markRemoteCopy();
  for (let paste = 0; paste < 5; paste++) {
    assert.equal(router.beginPaste(), "host", `paste ${paste + 1}`);
    assert.equal(router.endPaste(), "host", `paste ${paste + 1}`);
  }
});

test("uses the browser clipboard after focus leaves the remote page", () => {
  const router = new ClipboardShortcutRouter();
  router.markRemoteCopy();
  router.reset();
  assert.equal(router.beginPaste(), "browser");
  assert.equal(router.endPaste(), "browser");
});

test("a later copy in the session takes the route back from the browser", () => {
  const router = new ClipboardShortcutRouter();
  router.reset();
  assert.equal(router.beginPaste(), "browser");
  assert.equal(router.endPaste(), "browser");
  router.markRemoteCopy();
  assert.equal(router.beginPaste(), "host");
  assert.equal(router.endPaste(), "host");
});

test("keeps the selected route for repeated keydown events", () => {
  const router = new ClipboardShortcutRouter();
  router.markRemoteCopy();
  assert.equal(router.beginPaste(), "host");
  assert.equal(router.beginPaste(), "host");
  assert.equal(router.endPaste(), "host");
});

test("a paste already in flight is not rerouted by a copy underneath it", () => {
  const router = new ClipboardShortcutRouter();
  assert.equal(router.beginPaste(), "browser");
  router.markRemoteCopy();
  assert.equal(router.beginPaste(), "browser", "the held key keeps its route");
  assert.equal(router.endPaste(), "browser");
  assert.equal(router.beginPaste(), "host", "the next paste sees the copy");
});
