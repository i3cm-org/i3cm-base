import { test } from "node:test";
import assert from "node:assert/strict";
import { fromCheckApi, fromTypecat, relativeFile, sortIssues, summary, modeOf } from "../lib/issues.js";

test("TypeScript errors from typecat carry file and line", () => {
  const list = fromTypecat({ error_id: "typecat.compile_error", description: "TypeScript reported errors",
    params: { errors: ["/work/lend/i3cm/types/loan.ts:25 Cannot find name 'Strin'."] } }, "/work/lend/i3cm");
  assert.deepEqual(list, [{ severity: "error", source: "typescript", errorId: "typescript",
    description: "Cannot find name 'Strin'.", params: {}, file: "types/loan.ts", line: 25, column: null }]);
});

test("other typecat errors become one issue", () => {
  const [i] = fromTypecat({ error_id: "typecat.unknown_module", description: "unknown module", params: { module: "x" } });
  assert.equal(i.source, "typecat");
  assert.equal(i.errorId, "typecat.unknown_module");
  assert.deepEqual(i.params, { module: "x" });
  assert.equal(i.file, null);
  assert.deepEqual(fromTypecat(null), []);
});

test("issues of the check API", () => {
  const i = fromCheckApi({ severity: "warning", source: "checker", error_id: "types.no_backfill", description: "d",
    params: { attr: "a" }, file: "types/loan.ts", line: 3, column: 5 });
  assert.equal(i.errorId, "types.no_backfill");
  assert.equal(i.line, 3);
  assert.equal(i.column, 5);
  assert.equal(fromCheckApi({}).severity, "error");
});

test("file names relative to i3cm/", () => {
  assert.equal(relativeFile("/a/b/i3cm/aspects/x.ts", ""), "aspects/x.ts");
  assert.equal(relativeFile("C:\\r\\i3cm\\module.ts", "C:\\r\\i3cm"), "module.ts");
  assert.equal(relativeFile("types/x.ts", "/r/i3cm"), "types/x.ts");
  assert.equal(relativeFile(null, ""), null);
});

test("sorting and summary", () => {
  const list = sortIssues([
    { severity: "warning", file: "a.ts", line: 1 },
    { severity: "error", file: null, line: null },
    { severity: "error", file: "b.ts", line: 9 },
    { severity: "error", file: "b.ts", line: 2 },
  ]);
  assert.deepEqual(list.map((i) => `${i.severity}:${i.file}:${i.line}`),
    ["error:b.ts:2", "error:b.ts:9", "error:null:null", "warning:a.ts:1"]);
  assert.deepEqual(summary(list), { error: 3, warning: 1, info: 0, green: false });
  assert.equal(summary([]).green, true);
});

test("editor mode by folder", () => {
  assert.equal(modeOf("types/loan.ts"), "type");
  assert.equal(modeOf("aspects/loans.ts"), "aspect");
  assert.equal(modeOf("ui/lend.html"), "view");
  assert.equal(modeOf("module.ts"), "type");
});
