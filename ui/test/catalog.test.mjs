import { test } from "node:test";
import assert from "node:assert/strict";
import { card, valueType, indexCatalog, packages, referenced, referencing } from "../lib/catalog.js";
import { catalog } from "./fixture.js";

test("cardinality text", () => {
  assert.equal(card(1, 1), "1");
  assert.equal(card(0, 1), "0..1");
  assert.equal(card(0, null), "0..n");
  assert.equal(card(1, null), "1..n");
  assert.equal(card(2, 5), "2..5");
});

test("value types read like the notation", () => {
  assert.equal(valueType({ kind: "date" }), "LocalDate");
  assert.equal(valueType({ kind: "enum", values: ["a", "b"] }), 'Enum<"a" | "b">');
  assert.equal(valueType({ kind: "decimal", precision: 12, scale: 2 }), "Decimal<12,2>");
  assert.equal(valueType({ kind: "somethingNew" }), "somethingNew");
});

test("packages put the module under development first and list its parts", () => {
  const p = packages(catalog, "lend");
  assert.deepEqual(p.map((x) => [x.name, x.editable]), [["lend", true], ["base", false]]);
  assert.deepEqual(p[0].types, ["lend:Book", "lend:Customer", "lend:Item", "lend:Loan"]);
  assert.deepEqual(p[0].aspects, ["lend:loans"]);
  assert.deepEqual(p[0].profiles, ["lend:clerk"]);
  assert.deepEqual(p[0].rooms, ["lend"]);
});

test("referenced and referencing types", () => {
  const ix = indexCatalog(catalog);
  assert.deepEqual(referenced(ix, "lend:Loan"), ["lend:Customer", "lend:Item"]);
  assert.deepEqual(referenced(ix, "lend:Customer"), ["base:Person"]);
  assert.deepEqual(referenced(ix, "lend:Book"), ["lend:Item"]);
  assert.deepEqual(referencing(ix, "lend:Item"), ["lend:Book", "lend:Loan"]);
  assert.deepEqual(referencing(ix, "base:Person"), ["lend:Customer"]);
  assert.deepEqual(referenced(ix, "lend:Unknown"), []);
});
