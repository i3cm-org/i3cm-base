import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildCatalog, copyUi } from "../typecat.mjs";

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
const mod = (n) => `export const module = { name: "${n}", requires: [] as string[] };\nexport const rooms: string[] = [];\nexport const profiles = [];\n`;

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
    `import { ns, min, Ref, Struct, Enum } from "../../lib";\n` +
    `@ns("m") export class B {}\n` +
    `@ns("m") export class A { @min(2) bs!: Ref<B, "a", "0..1">[]; addr?: Struct<{ street: string; zip?: string }>; s!: Enum<"x"|"y">; }\n` });
  const cat = buildCatalog(dir);
  const r = cat.references[0];
  assert.deepEqual([r.holder.min, r.holder.max, r.target.min, r.target.max], [2, null, 0, 1]);
  const a = cat.types.find((t) => t.name === "m:A");
  assert.equal(a.attributes.find((x) => x.name === "addr").vt.kind, "struct");
  assert.deepEqual(a.attributes.find((x) => x.name === "s").vt.values, ["x", "y"]);
});

const one = (body) => tmpTypes({ "modules/m/module.ts": mod("m"), "modules/m/t.ts": `import { ns, Ref } from "../../lib";\n${body}\n` });

test("reads the cardinality of the opposite end from the third parameter of Ref", () => {
  const cat = buildCatalog(one(`@ns("m") export class B {}\n@ns("m") export class A { b!: Ref<B, "as", "1..n">; c?: Ref<B, "cs">; }`));
  const b = cat.references.find((r) => r.name === "m:A.b"), c = cat.references.find((r) => r.name === "m:A.c");
  assert.deepEqual([b.target.min, b.target.max], [1, null]);
  assert.deepEqual([c.target.min, c.target.max], [0, null]);
});

test("rejects an unknown cardinality in Ref already in the compiler", () => {
  assert.throws(() => buildCatalog(one(`@ns("m") export class B {}\n@ns("m") export class A { b!: Ref<B, "as", "1..m">; }`)), (e) => e.id === "typecat.compile_error");
});

test("rejects an opposite end that collides with an attribute of the target", () => {
  assert.throws(() => buildCatalog(one(`@ns("m") export class B { as!: string; }\n@ns("m") export class A { b!: Ref<B, "as">; }`)),
    (e) => e.id === "typecat.name_conflict" && e.params.type === "m:B" && e.params.name === "as");
});

test("rejects two opposite ends with the same name at one target", () => {
  assert.throws(() => buildCatalog(one(`@ns("m") export class B {}\n@ns("m") export class A { b!: Ref<B, "x">; }\n@ns("m") export class C { b!: Ref<B, "x">; }`)),
    (e) => e.id === "typecat.name_conflict");
});

test("rejects an opposite end that collides with a name of a supertype", () => {
  assert.throws(() => buildCatalog(one(`@ns("m") export class S { x?: string; }\n@ns("m") export class B extends S {}\n@ns("m") export class A { b!: Ref<B, "x">; }`)),
    (e) => e.id === "typecat.name_conflict" && e.params.type === "m:B");
});

test("rejects reserved names as opposite end", () => {
  assert.throws(() => buildCatalog(one(`@ns("m") export class B {}\n@ns("m") export class A { b!: Ref<B, "_id">; }`)), (e) => e.id === "typecat.reserved_name");
});

test("rejects the removed @card decorator", () => {
  assert.throws(() => buildCatalog(one(`@ns("m") export class B {}\n@ns("m") export class A { @card("1") b!: Ref<B, "as">; }`)), (e) => e.id === "typecat.compile_error");
});

test("allows equal names from unrelated modules, rejects them against the base", () => {
  const files = (baseAttr) => ({
    "base/module.ts": mod("base"), "base/t.ts": `import { ns } from "../lib";\n@ns("base") export class P { ${baseAttr} }\n`,
    "modules/a/module.ts": mod("a"), "modules/a/t.ts": `import { ns, Ref } from "../../lib";\nimport { P } from "../../base/t";\n@ns("a") export class X { p!: Ref<P, "links">; }\n`,
    "modules/b/module.ts": mod("b"), "modules/b/t.ts": `import { ns, Ref } from "../../lib";\nimport { P } from "../../base/t";\n@ns("b") export class Y { p!: Ref<P, "links">; }\n`,
  });
  const cat = buildCatalog(tmpTypes(files("name?: string;")));
  assert.equal(cat.references.filter((r) => r.target.end === "links").length, 2);
  assert.throws(() => buildCatalog(tmpTypes(files("links?: string;"))), (e) => e.id === "typecat.name_conflict");
});

test("text types are searched by grep unless named in @noGrep", () => {
  const dir = tmpTypes({
    "modules/x/module.ts": mod("x"),
    "modules/x/types.ts": `import { ns, hasContent, texts, noGrep } from "../../lib";\n@ns("x") @hasContent() @noGrep("ocr.txt") @texts("text.md:text/markdown", "ocr.txt:text/plain")\nexport class Scan { title!: string; }\n`,
  });
  const cat = buildCatalog(dir);
  assert.deepEqual(cat.textTypes.map((t) => [t.name, t.grep]), [["x:text.md", true], ["x:ocr.txt", false]]);
  const hidden = tmpTypes({ "modules/x/module.ts": mod("x"), "modules/x/types.ts": `import { ns, texts } from "../../lib";\n@ns("x") @texts("a:text/plain:noGrep")\nexport class A { t!: string; }\n` });
  assert.throws(() => buildCatalog(hidden), (e) => e.id === "typecat.bad_text");
  const unknown = tmpTypes({ "modules/x/module.ts": mod("x"), "modules/x/types.ts": `import { ns, texts, noGrep } from "../../lib";\n@ns("x") @noGrep("b") @texts("a:text/plain")\nexport class A { t!: string; }\n` });
  assert.throws(() => buildCatalog(unknown), (e) => e.id === "typecat.unknown_text");
});

// Module repos outside the kernel (DEC-136): i3cm/module.ts with types/, aspects/, i18n/, ui/ below it.
const demoRepo = path.join(here, "fixtures/demo-module");

test("reads a module repo given from outside, with @i3cm aliases", () => {
  const cat = buildCatalog(typesDir, { modules: [demoRepo] });
  const thing = cat.types.find((t) => t.name === "demo:Thing");
  assert.ok(thing, "demo:Thing");
  const owner = cat.references.find((r) => r.name === "demo:Thing.owner");
  assert.equal(owner.target.type, "base:Person");
  const demo = cat.modules.find((m) => m.name === "demo");
  assert.deepEqual(demo.requires, ["base"]);
  assert.ok(demo.i18n.en.includes("demo-Thing-name"));
  assert.equal(demo.aspects[0].name, "demo:things");
  assert.equal(demo.profiles[0].name, "demo:clerk");
  // the i3cm directory itself works as well
  assert.ok(buildCatalog(typesDir, { modules: [path.join(demoRepo, "i3cm")] }).types.some((t) => t.name === "demo:Thing"));
});

test("copies the ui templates of module repos", () => {
  const cat = buildCatalog(typesDir, { modules: [demoRepo] });
  const out = fs.mkdtempSync(path.join(os.tmpdir(), "typecat-ui-"));
  assert.deepEqual(copyUi(cat, out), ["demo.html"]);
  assert.ok(fs.readFileSync(path.join(out, "demo.html"), "utf8").includes("demo:Thing"));
});

test("rejects a module that requires a module not in the build", () => {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "typecat-mod-"));
  fs.mkdirSync(path.join(repo, "i3cm/types"), { recursive: true });
  fs.writeFileSync(path.join(repo, "i3cm/module.ts"), `export const module = { name: "late", requires: ["base", "missing"] };\nexport const rooms: string[] = [];\nexport const profiles = [];\n`);
  fs.writeFileSync(path.join(repo, "i3cm/types/a.ts"), `import { ns } from "@i3cm/lib";\n@ns("late") export class A { x!: string; }\n`);
  assert.throws(() => buildCatalog(typesDir, { modules: [repo] }), (e) => e.id === "typecat.unknown_module" && e.params.requires === "missing");
});

test("rejects a module repo without i3cm/module.ts and a module given twice", () => {
  const empty = fs.mkdtempSync(path.join(os.tmpdir(), "typecat-empty-"));
  assert.throws(() => buildCatalog(typesDir, { modules: [empty] }), (e) => e.id === "typecat.module_root");
  const copy = fs.mkdtempSync(path.join(os.tmpdir(), "typecat-copy-"));
  fs.mkdirSync(path.join(copy, "i3cm"));
  fs.copyFileSync(path.join(demoRepo, "i3cm/module.ts"), path.join(copy, "i3cm/module.ts"));
  assert.throws(() => buildCatalog(typesDir, { modules: [demoRepo, copy] }), (e) => e.id === "typecat.duplicate_module");
  // the same repo given twice counts once
  assert.equal(buildCatalog(typesDir, { modules: [demoRepo, demoRepo] }).modules.filter((m) => m.name === "demo").length, 1);
});

test("passes rights paths, view filters and dropType through to the catalog", () => {
  const dir = tmpTypes({
    "modules/x/module.ts": `import type { ProfileDef } from "../../lib";\nexport const module = { name: "x", requires: [] as string[] };\nexport const rooms: string[] = [];\nexport const profiles: ProfileDef[] = [{ name: "lead", rights: [{ sign: "+", art: ["write"], types: ["x:Req"], room: "root", when: "owner = %me" }] }];\n`,
    "modules/x/types.ts": `import { ns, hasContent, Ref } from "../../lib";\n@ns("x") export class Person { name!: string; }\n@ns("x") @hasContent() export class Doc { title!: string; req!: Ref<Req, "docs">; }\n@ns("x") export class Req { title!: string; owner?: Ref<Person, "reqs">; }\n`,
    "modules/x/aspects.ts": `import { aspect } from "../../lib";\nexport const reqs = aspect("reqs", { entry: { type: "x:Req", where: "owner = %me" }, nodes: { "x:Req": { subNodes: { docs: { path: "docs", folder: true, dropType: "x:Doc" } } } } });\n`,
  });
  const cat = buildCatalog(dir);
  const m = cat.modules.find((x) => x.name === "x");
  assert.equal(m.profiles[0].rights[0].when, "owner = %me");
  const a = m.aspects.find((x) => x.name === "x:reqs");
  assert.equal(a.entry.where, "owner = %me");
  assert.equal(a.nodes["x:Req"].subNodes[0].dropType, "x:Doc");
});

const extraRepo = path.join(here, "fixtures/demo-extra");

test("imports types of another module of the run as @i3cm/<module>; consts shared by aspects", () => {
  const cat = buildCatalog(typesDir, { modules: [demoRepo, extraRepo] });
  const tag = cat.types.find((t) => t.name === "extra:Tag");
  assert.ok(tag);
  assert.ok(cat.references.some((r) => r.name === "extra:Tag.thing" && r.target.type === "demo:Thing"));
  const extra = cat.modules.find((m) => m.name === "extra");
  assert.deepEqual(extra.aspects.map((a) => a.name), ["extra:byLabel", "extra:flat"]);
  for (const a of extra.aspects) assert.ok(a.nodes["extra:Tag"].tabs.some((t) => t.label === "thing"));
});
