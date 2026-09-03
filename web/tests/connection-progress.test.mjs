import assert from "node:assert/strict";
import test from "node:test";

import { CONNECTION_STEPS, connectionProgressSnapshot } from "../src/connection-progress.ts";

test("connection phases advance monotonically through the five visible steps", () => {
  const phases = ["signaling", "session", "candidates", "secure", "video", "ready"];
  const snapshots = phases.map((phase) => connectionProgressSnapshot(phase));
  assert.deepEqual(snapshots.map((item) => item.activeStep), [0, 1, 2, 3, 4, 5]);
  assert.equal(CONNECTION_STEPS.length, 5);
  assert.deepEqual(snapshots.map((item) => item.percent), [10, 28, 52, 76, 92, 100]);
});

test("runtime diagnostics can replace the default phase description", () => {
  const snapshot = connectionProgressSnapshot("candidates", "已发现 3 条网络路径");
  assert.equal(snapshot.description, "已发现 3 条网络路径");
  assert.equal(snapshot.title, "正在寻找最快路径");
});
