import { test } from "node:test";
import assert from "node:assert/strict";
import { locate, lineColumn, offsetOf, baseName } from "../lib/sources.js";
import { highlight, highlightTs, languageOf, escapeHtml } from "../lib/highlight.js";

const files = [
  { path: "types/loan.ts", text: 'import { ns } from "@i3cm/lib";\n\n@ns("lend")\nexport class Loan {\n  due?: LocalDate;\n}\n' },
  { path: "aspects/loans.ts", text: 'export const loans = aspect("loans", {\n  entry: { type: "lend:Loan" },\n});\n' },
  { path: "module.ts", text: 'export const rooms: string[] = ["lend"];\nexport const profiles = [\n  { name: "clerk", rights: [] },\n];\n' },
];

test("locating declarations", () => {
  assert.deepEqual(locate(files, "type", "lend:Loan"), { file: "types/loan.ts", line: 4, column: 8 });
  assert.deepEqual(locate(files, "aspect", "lend:loans"), { file: "aspects/loans.ts", line: 1, column: 22 });
  assert.deepEqual(locate(files, "profile", "lend:clerk"), { file: "module.ts", line: 3, column: 5 });
  assert.deepEqual(locate(files, "room", "lend"), { file: "module.ts", line: 1, column: 34 });
  assert.equal(locate(files, "type", "lend:Missing"), null);
});

test("offsets, lines and columns", () => {
  const t = "ab\ncde\nf";
  assert.deepEqual(lineColumn(t, 4), { line: 2, column: 2 });
  assert.equal(offsetOf(t, 2, 2), 4);
  assert.equal(offsetOf(t, 9, 1), t.length);
  assert.equal(baseName("types/loan.ts"), "loan.ts");
});

test("highlighting escapes and marks tokens", () => {
  const h = highlightTs('@ns("x") class A { n!: Ref<B, "e">; } // <c>');
  assert.match(h, /<span class="tk-decorator">@ns<\/span>/);
  assert.match(h, /<span class="tk-keyword">class<\/span>/);
  assert.match(h, /<span class="tk-type">Ref<\/span>/);
  assert.match(h, /<span class="tk-comment">\/\/ &lt;c&gt;<\/span>/);
  assert.equal(highlight("<b>", "plain"), "&lt;b&gt;");
  assert.equal(languageOf("ui/x.html"), "html");
  assert.equal(languageOf("README"), "plain");
  assert.equal(escapeHtml('"&'), "&quot;&amp;");
  // highlighting never changes the text
  const src = 'const s = `a${b}`; /* x */ 12.5 "u\\"v"';
  assert.equal(highlightTs(src).replace(/<[^>]+>/g, "").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&amp;/g, "&"), src);
});

test("HTML highlighting keeps the text", () => {
  const src = '<template data-aspect="x"><!-- c --><span data-attr="a"></span></template>';
  const h = highlight(src, "html");
  assert.equal(h.replace(/<[^>]+>/g, "").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&amp;/g, "&"), src);
});
