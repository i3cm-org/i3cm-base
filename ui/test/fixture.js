// A small catalog in the format typecat writes (format 1).
export const catalog = {
  format: 1,
  modules: [
    { name: "base", requires: [], rooms: [], profiles: [], aspects: [] },
    { name: "lend", requires: ["base"], rooms: ["lend"], profiles: [{ name: "lend:clerk" }], aspects: [{ name: "lend:loans" }] },
  ],
  types: [
    { name: "base:Person", ns: "base", local: "Person", abstract: false, extends: null,
      attributes: [{ name: "lastName", vt: { kind: "string" }, required: true, list: false }],
      requires: { mode: "any", types: [] }, imprintsOn: [], brings: [] },
    { name: "lend:Customer", ns: "lend", local: "Customer", abstract: false, extends: null,
      attributes: [{ name: "number", vt: { kind: "string" }, required: true, list: false }],
      requires: { mode: "any", types: ["base:Person"] }, imprintsOn: [], brings: [] },
    { name: "lend:Item", ns: "lend", local: "Item", abstract: true, extends: null,
      attributes: [{ name: "tags", vt: { kind: "string" }, required: false, list: true }],
      requires: { mode: "any", types: [] }, imprintsOn: [], brings: [] },
    { name: "lend:Book", ns: "lend", local: "Book", abstract: false, extends: "lend:Item",
      attributes: [], requires: { mode: "any", types: [] }, imprintsOn: [], brings: [] },
    { name: "lend:Loan", ns: "lend", local: "Loan", abstract: false, extends: null,
      attributes: [{ name: "due", vt: { kind: "date" }, required: false, list: false }],
      requires: { mode: "any", types: [] }, imprintsOn: [], brings: [] },
  ],
  references: [
    { name: "lend:Loan.customer", holder: { type: "lend:Loan", end: "customer", min: 1, max: 1 },
      target: { type: "lend:Customer", end: "loans", min: 0, max: null }, partOf: false },
    { name: "lend:Loan.item", holder: { type: "lend:Loan", end: "item", min: 1, max: 1 },
      target: { type: "lend:Item", end: "loans", min: 0, max: 1 }, partOf: true },
  ],
};
