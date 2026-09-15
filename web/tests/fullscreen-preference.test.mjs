import assert from "node:assert/strict";
import test from "node:test";
import { fullscreenRemembered, rememberFullscreen } from "../src/fullscreen-preference.ts";

function withStorage(storage, body) {
  const previous = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", { value: storage, configurable: true });
  try { body(); } finally {
    if (previous) Object.defineProperty(globalThis, "localStorage", previous);
    else delete globalThis.localStorage;
  }
}

function memoryStorage(initial = {}) {
  const map = new Map(Object.entries(initial));
  return {
    getItem: (key) => (map.has(key) ? map.get(key) : null),
    setItem: (key, value) => map.set(key, value),
    read: (key) => map.get(key),
  };
}

test("a session starts fullscreen only after the user chose it", () => {
  withStorage(memoryStorage(), () => {
    assert.equal(fullscreenRemembered(), false, "never chosen");
    rememberFullscreen(true);
    assert.equal(fullscreenRemembered(), true);
    // Leaving fullscreen - with the button, Escape or the window chrome - is
    // just as much a choice as entering it.
    rememberFullscreen(false);
    assert.equal(fullscreenRemembered(), false);
  });
  // Only the exact stored string counts; anything else must not open fullscreen.
  withStorage(memoryStorage({ "remote-fullscreen": "TRUE" }), () => {
    assert.equal(fullscreenRemembered(), false);
  });
  withStorage(memoryStorage({ "remote-fullscreen": "true" }), () => {
    assert.equal(fullscreenRemembered(), true);
  });
});

test("blocked storage degrades to windowed instead of breaking the session", () => {
  const blocked = {
    getItem() { throw new Error("storage is disabled"); },
    setItem() { throw new Error("storage is disabled"); },
  };
  withStorage(blocked, () => {
    assert.equal(fullscreenRemembered(), false);
    // The writer runs inside the fullscreenchange handler that also schedules
    // reconnection; it must never be the reason that handler stops.
    assert.doesNotThrow(() => rememberFullscreen(true));
  });
});

test("the stored value is the one the session reads back", () => {
  const storage = memoryStorage();
  withStorage(storage, () => {
    rememberFullscreen(true);
    assert.equal(storage.read("remote-fullscreen"), "true");
    rememberFullscreen(false);
    assert.equal(storage.read("remote-fullscreen"), "false");
  });
});
