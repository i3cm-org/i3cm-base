import { test } from "node:test";
import assert from "node:assert/strict";
import { splitOp, fillParams } from "../lib/actions.js";

test("operation names", () => {
  assert.deepEqual(splitOp("tasks/complete"), ["tasks", "complete"]);
  assert.deepEqual(splitOp("tasks:complete"), ["tasks", "complete"]);
  assert.equal(splitOp("complete"), null);
  assert.equal(splitOp("a/b/c"), null);
});

test("parameters take values of the node", () => {
  const ctx = { id: "42", attrs: { count: 3, subject: "Call", tags: ["a", "b"] } };
  assert.deepEqual(fillParams({ id: "{id}", n: "{count}", note: "Re: {subject} #{count}", t: "{tags}", miss: "{nope}", fixed: true }, ctx),
    { id: "42", n: 3, note: "Re: Call #3", t: ["a", "b"], miss: null, fixed: true });
  assert.deepEqual(fillParams([{ x: ["{id}"] }], ctx), [{ x: ["42"] }]);
});
