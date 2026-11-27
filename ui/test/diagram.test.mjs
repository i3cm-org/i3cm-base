import { test } from "node:test";
import assert from "node:assert/strict";
import { indexCatalog } from "../lib/catalog.js";
import { diagramModel, referenceSnippet } from "../lib/diagram.js";
import { layout, overlaps, boxSize } from "../lib/layout.js";
import { catalog } from "./fixture.js";

const ix = indexCatalog(catalog);

test("boxes and edges only for visible types", () => {
  const m = diagramModel(ix, ["lend:Loan", "lend:Customer", "nope:Missing"]);
  assert.deepEqual(m.boxes.map((b) => b.id), ["lend:Customer", "lend:Loan"]);
  assert.deepEqual(m.edges, [{ kind: "reference", from: "lend:Loan", to: "lend:Customer", fromEnd: "customer",
    toEnd: "loans", fromCard: "1", toCard: "0..n", partOf: false }]);
  assert.deepEqual(m.boxes[1].attributes, [{ name: "due", type: "LocalDate", optional: true }]);
});

test("generalisation, prerequisites and part-of", () => {
  const m = diagramModel(ix, ["lend:Book", "lend:Item", "lend:Loan", "lend:Customer", "base:Person"]);
  const kinds = m.edges.map((e) => `${e.kind}:${e.from}->${e.to}${e.partOf ? ":partOf" : ""}`);
  assert.ok(kinds.includes("extends:lend:Book->lend:Item"));
  assert.ok(kinds.includes("requires:lend:Customer->base:Person"));
  assert.ok(kinds.includes("reference:lend:Loan->lend:Item:partOf"));
  assert.equal(m.boxes.find((b) => b.id === "lend:Item").abstract, true);
  assert.equal(m.boxes.find((b) => b.id === "lend:Item").attributes[0].type, "string[]");
});

test("reference snippet in the notation", () => {
  assert.equal(referenceSnippet("Customer", "customer", "loans"), 'customer!: Ref<Customer, "loans">;');
});

test("layout: referenced types above, no overlaps", () => {
  const m = diagramModel(ix, ["lend:Book", "lend:Item", "lend:Loan", "lend:Customer", "base:Person"]);
  const pos = layout(m);
  assert.ok(pos["lend:Loan"].y > pos["lend:Customer"].y);
  assert.ok(pos["lend:Customer"].y > pos["base:Person"].y);
  const ids = Object.keys(pos);
  for (const a of ids) for (const b of ids) if (a < b) assert.ok(!overlaps(pos[a], pos[b]), `${a} overlaps ${b}`);
});

test("layout keeps pinned boxes and moves the others out of the way", () => {
  const m = diagramModel(ix, ["lend:Loan", "lend:Customer", "lend:Item"]);
  const free = layout(m);
  const pinned = { "lend:Loan": { x: free["lend:Customer"].x, y: free["lend:Customer"].y } };
  const pos = layout(m, pinned);
  assert.equal(pos["lend:Loan"].x, pinned["lend:Loan"].x);
  assert.equal(pos["lend:Loan"].y, pinned["lend:Loan"].y);
  const ids = Object.keys(pos);
  for (const a of ids) for (const b of ids) if (a < b) assert.ok(!overlaps(pos[a], pos[b]), `${a} overlaps ${b}`);
});

test("layout survives cycles", () => {
  const m = { boxes: [{ id: "a", title: "A", attributes: [] }, { id: "b", title: "B", attributes: [] }],
    edges: [{ kind: "reference", from: "a", to: "b" }, { kind: "reference", from: "b", to: "a" }] };
  const pos = layout(m);
  assert.ok(!overlaps(pos.a, pos.b));
  assert.ok(boxSize(m.boxes[0]).w >= 120);
});
