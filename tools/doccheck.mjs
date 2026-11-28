#!/usr/bin/env node
// Documentation against code: every endpoint, environment variable and command named in the
// documentation (README.md, docs/**/*.md) must exist in the code. Exit 1 with a list otherwise.
//
// Usage: node tools/doccheck.mjs [--list]   (--list also prints everything that was checked)

import fs from "node:fs";
import path from "node:path";

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");
const list = process.argv.includes("--list");

function walk(dir, pred, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (["node_modules", "target", ".git", ".pgdata"].includes(e.name)) continue;
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, pred, out);
    else if (pred(p)) out.push(p);
  }
  return out;
}
const read = (p) => fs.readFileSync(p, "utf8");
const rel = (p) => path.relative(root, p);

const docs = [path.join(root, "README.md"), ...walk(path.join(root, "docs"), (p) => p.endsWith(".md"))];
const code = [
  ...walk(path.join(root, "crates"), (p) => p.endsWith(".rs")),
  ...walk(path.join(root, "tools"), (p) => /\.(mjs|js|sh)$/.test(p) && !p.endsWith("doccheck.mjs")),
  ...walk(path.join(root, "deploy"), (p) => /(Dockerfile|\.ya?ml|\.conf|\.py|\.sh)$/.test(p)),
  ...walk(path.join(root, "examples"), (p) => /\.(ya?ml|rs|ts)$/.test(p)),
  ...walk(path.join(root, "ui"), (p) => /\.(js|html)$/.test(p)),
  path.join(root, "Makefile"),
];
const codeText = code.map(read).join("\n");

// --- endpoints -------------------------------------------------------------------------------
const norm = (p) => p.replace(/\{[^}]*\}/g, "{}").replace(/\/+$/, "") || "/";
const routes = new Set();
for (const m of codeText.matchAll(/\.route\(\s*"([^"]+)"/g)) routes.add(norm(m[1]));
for (const m of codeText.matchAll(/"(\/v1\/operator\/[^"]+)"/g)) routes.add(norm(m[1]));

// --- commands --------------------------------------------------------------------------------
const serverMain = read(path.join(root, "crates/i3cm-server/src/main.rs"));
const kebab = (s) => s.replace(/([a-z])([A-Z])/g, "$1-$2").toLowerCase();
const enumVariants = (name) => {
  const m = serverMain.match(new RegExp(`enum ${name} \\{([\\s\\S]*?)\\n\\}`));
  return m ? [...m[1].matchAll(/^\s{4}([A-Z][A-Za-z]+)\s*[{,]/gm)].map((x) => kebab(x[1])) : [];
};
const serverCmds = new Set([...enumVariants("Cmd"), "--version", "--help"]);
const serverSub = { catalog: new Set(enumVariants("CatalogCmd")), module: new Set(enumVariants("ModuleCmd")) };
const cliMain = read(path.join(root, "crates/i3cm-cli/src/main.rs"));
const cliCmds = new Set([...cliMain.matchAll(/Some\("([a-z-]+)"\)/g)].map((m) => m[1]));
const cliNames = new Set([...cliMain.matchAll(/"(i3cm-[a-z]+)"\s*=>/g)].map((m) => m[1]));
const tools = new Set(["ls", "cat", "head", "tail", "wc", "stat", "grep", "find", "tree", "sed", "sort", "uniq", "cut"]);

// --- scan the documentation ----------------------------------------------------------------
const problems = [];
const checked = { endpoints: new Set(), variables: new Set(), commands: new Set() };
for (const d of docs) {
  const text = read(d);
  const where = rel(d);
  // endpoints: "GET /v1/…", and paths in backticks starting with /v1/ or one of the open pages
  const eps = new Set();
  for (const m of text.matchAll(/\b(?:GET|POST|PUT|PATCH|DELETE)(?:`|\s|\/)+`?(\/[A-Za-z0-9_{}\/.:-]*)/g)) eps.add(m[1]);
  for (const m of text.matchAll(/`(\/v1\/[A-Za-z0-9_{}\/.:-]*)/g)) eps.add(m[1]);
  for (const m of text.matchAll(/`((?:GET|POST|PUT|PATCH|DELETE)[^`]*?)`/g)) {
    for (const p of m[1].matchAll(/(\/[A-Za-z0-9_{}\/.:-]*)/g)) eps.add(p[1]);
  }
  for (let e of eps) {
    e = e.replace(/[.:]$/, "");
    if (!e.startsWith("/v1") && !["/", "/health", "/guide/agents", "/openapi.json", "/skill.zip"].includes(e)) continue;
    const n = norm(e);
    checked.endpoints.add(n);
    // a documented prefix such as /v1/rooms/{id}/children is a route; "/objects" alone after a comma is not checked
    const re = (r) => new RegExp("^" + r.replace(/[.*+?^$()|[\]\\]/g, "\\$&").replace(/\\?\{\\?\}/g, "[^/]+") + "$");
    if (!routes.has(n) && ![...routes].some((r) => re(r).test(n))) problems.push(`${where}: endpoint ${e} has no route`);
  }
  for (const m of text.matchAll(/\bI3CM_[A-Z_]+\b/g)) {
    checked.variables.add(m[0]);
    if (!new RegExp(`\\b${m[0]}\\b`).test(codeText)) problems.push(`${where}: variable ${m[0]} is not used in the code`);
  }
  for (const m of text.matchAll(/(-p\s+)?i3cm-server\s+([a-z-]+)(?:\s+([a-z-]+))?/g)) {
    const [, cargo, cmd, sub] = m;
    if (cargo || (cmd.startsWith("-") && !["--version", "--help"].includes(cmd))) continue;
    checked.commands.add(`i3cm-server ${cmd}`);
    if (!serverCmds.has(cmd)) problems.push(`${where}: i3cm-server ${cmd} does not exist`);
    else if (serverSub[cmd] && sub && !serverSub[cmd].has(sub)) problems.push(`${where}: i3cm-server ${cmd} ${sub} does not exist`);
  }
  for (const m of text.matchAll(/`i3cm\s+([a-z-]+)/g)) {
    checked.commands.add(`i3cm ${m[1]}`);
    if (!cliCmds.has(m[1]) && !tools.has(m[1])) problems.push(`${where}: i3cm ${m[1]} does not exist`);
  }
  for (const m of text.matchAll(/(^|[^.\w\/-])(i3cm-[a-z]+)(?![\w\/-])/gm)) {
    const n = m[2];
    if (n === "i3cm-bin") continue; // directory name in examples
    if (/^i3cm-(base|org|server|core|store|api|client|tools|cli|wasm|loadtest|tasks|dms|mail|studio|example|registry|ui|module)$/.test(n)) continue;
    checked.commands.add(n);
    const tool = n.slice(5);
    if (!cliNames.has(n) && !tools.has(tool) && tool !== "links") problems.push(`${where}: program name ${n} is not handled`);
  }
}

// --- limits: every kernel constant MAX_…, …_TIMEOUT, …_TTL… is listed in a section "## Limits" ---
const limitsText = docs
  .map((d) => {
    const t = read(d);
    const i = t.indexOf("\n## Limits\n");
    if (i < 0) return "";
    const rest = t.slice(i + 1);
    const j = rest.indexOf("\n## ", 3);
    return j < 0 ? rest : rest.slice(0, j);
  })
  .join("\n");
const consts = new Set();
for (const f of walk(path.join(root, "crates"), (p) => p.endsWith(".rs") && !p.includes("/tests/"))) {
  for (const m of read(f).matchAll(/^pub const ([A-Z][A-Z0-9_]*)\s*:/gm)) {
    if (/^MAX_|_TIMEOUT$|_TTL(_[A-Z]+)*$/.test(m[1])) consts.add(m[1]);
  }
}
checked.limits = consts;
for (const c of consts) if (!new RegExp(`\\b${c}\\b`).test(limitsText)) problems.push(`limit ${c} is not listed in a "## Limits" section`);

if (list) {
  for (const [k, v] of Object.entries(checked)) console.log(`${k} (${v.size}): ${[...v].sort().join(" ")}`);
}
if (problems.length) {
  console.error([...new Set(problems)].join("\n"));
  console.error(`doccheck: ${new Set(problems).size} problem(s)`);
  process.exit(1);
}
console.log(`doccheck: ok (${checked.endpoints.size} endpoints, ${checked.variables.size} variables, ${checked.commands.size} commands)`);
