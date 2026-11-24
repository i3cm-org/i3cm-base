import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildCatalog } from "../typecat.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const typesDir = path.resolve(here, "../../../types");

function tmpTypes(files) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "typecat-"));
  fs.mkdirSync(path.join(dir, "lib"));
  fs.copyFileSync(path.join(typesDir, "lib/index.ts"), path.join(dir, "lib/index.ts"));
  fs.copyFileSync(path.join(typesDir, "tsconfig.json"), path.join(dir, "tsconfig.json"));
  for (const [name, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), content);
  }
  return dir;
}
const mod = (n) => `export const module = { name: "${n}", requires: [] as string[] };\nexport const rooms: string[] = [];\nexport const roles = [];\n`;

test("builds the catalog of the shipped types", () => {
  const cat = buildCatalog(typesDir);
  assert.equal(cat.format, 1);
  const names = cat.types.map((t) => t.name);
  for (const n of ["base:Person", "base:PersonAccount", "base:Mail", "fibu:JournalEntry"]) assert.ok(names.includes(n), n);
  const pa = cat.types.find((t) => t.name === "base:PersonAccount");
  assert.equal(pa.extends, "base:Account");
  assert.deepEqual(pa.requires, { mode: "any", types: ["base:Person"] });
  const line = cat.references.find((r) => r.name === "fibu:JournalLine.entry");
  assert.equal(line.partOf, true);
  assert.deepEqual([line.target.end, line.target.min, line.target.max], ["lines", 1, null]);
  assert.ok(cat.modules.find((m) => m.name === "base").i18n.de.includes("base-Mail-name"));
  assert.ok(cat.textTypes.some((t) => t.name === "base:mail.md"));
});

test("rejects plain number attributes", () => {
  const dir = tmpTypes({ "modules/m/module.ts": mod("m"), "modules/m/t.ts": `import { ns } from "../../lib";\n@ns("m") export class A { x!: number; }\n` });
  assert.throws(() => buildCatalog(dir), (e) => e.id === "typecat.plain_number");
});

test("reports TypeScript errors", () => {
  const dir = tmpTypes({ "modules/m/module.ts": mod("m"), "modules/m/t.ts": `import { ns, Ref } from "../../lib";\n@ns("m") export class A { r!: Ref<Missing, "x">; }\n` });
  assert.throws(() => buildCatalog(dir), (e) => e.id === "typecat.compile_error");
});

test("reads list references, struct and enum attributes", () => {
  const dir = tmpTypes({ "modules/m/module.ts": mod("m"), "modules/m/t.ts":
    `import { ns, card, min, Ref, Struct, Enum } from "../../lib";\n` +
    `@ns("m") export class B {}\n` +
    `@ns("m") export class A { @min(2) @card("0..1") bs!: Ref<B, "a">[]; addr?: Struct<{ street: string; zip?: string }>; s!: Enum<"x"|"y">; }\n` });
  const cat = buildCatalog(dir);
  const r = cat.references[0];
  assert.deepEqual([r.holder.min, r.holder.max, r.target.min, r.target.max], [2, null, 0, 1]);
  const a = cat.types.find((t) => t.name === "m:A");
  assert.equal(a.attributes.find((x) => x.name === "addr").vt.kind, "struct");
  assert.deepEqual(a.attributes.find((x) => x.name === "s").vt.values, ["x", "y"]);
});
