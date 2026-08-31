import assert from "node:assert/strict";
import test from "node:test";

import { keyboardProfile, remoteScanCode } from "../src/keymap.ts";

test("detects Apple browser platforms", () => {
  assert.equal(keyboardProfile("MacIntel"), "mac");
  assert.equal(keyboardProfile("iPad"), "mac");
  assert.equal(keyboardProfile("Win32"), "standard");
});

test("maps both Mac Command keys to the matching remote Control keys", () => {
  assert.deepEqual(remoteScanCode("MetaLeft", "mac"), [0x1d, false]);
  assert.deepEqual(remoteScanCode("MetaRight", "mac"), [0x1d, true]);
});

test("keeps Windows Meta keys and ordinary keys unchanged", () => {
  assert.deepEqual(remoteScanCode("MetaLeft", "standard"), [0x5b, true]);
  assert.deepEqual(remoteScanCode("KeyC", "mac"), [0x2e, false]);
});
