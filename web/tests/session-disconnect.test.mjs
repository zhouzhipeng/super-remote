import assert from "node:assert/strict";
import test from "node:test";

import { disconnectMessage } from "../src/session-close.ts";

test("explains when a newer browser has taken control", () => {
  assert.equal(disconnectMessage("replaced_by_new_connection"), "此连接已被新的远程连接替代。");
  assert.equal(disconnectMessage("session_unavailable"), "此连接已失效，已有更新的连接获得了控制权。");
});

test("preserves an explicit unknown close reason", () => {
  assert.equal(disconnectMessage("maintenance"), "maintenance");
  assert.equal(disconnectMessage(""), "主机已结束远程桌面连接。");
});
