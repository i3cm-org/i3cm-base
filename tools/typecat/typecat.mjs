#!/usr/bin/env node
// typecat: reads i3cm type definitions (TypeScript) and writes catalog.json.
// Usage: typecat <typesDir> [--module <repo>]... [--out catalog.json] [--ui-out <dir>]
//        typecat check <typesDir> [--module <repo>]...   all issues with positions as JSON (exit 1 on an error)
//   --module (repeatable) or I3CM_MODULES (colon-separated): module repos outside the kernel, laid out
//   as <repo>/i3cm/module.ts, i3cm/types/, i3cm/aspects/, i3cm/i18n/, i3cm/ui/. Their sources import
//   the kernel through "@i3cm/lib" and "@i3cm/base".
import ts from "typescript";
import fs from "node:fs";
import path from "node:path";

export const FORMAT = 1;

class CatError extends Error {
  constructor(id, description, params = {}) { super(description); this.id = id; this.params = params; }
}

// Where a node sits: absolute file, 1-based line and column.
function posOf(node) {
  if (!node || !node.getSourceFile) return null;
  const sf = node.getSourceFile();
  const lc = sf.getLineAndCharacterOfPosition(node.getStart(sf));
  return { file: sf.fileName, line: lc.line + 1, column: lc.character + 1 };
}

// Run fn; in collecting mode (opts.issues is an array) a CatError becomes an issue at node and
// the build goes on with the next declaration, otherwise it is thrown.
function guarded(issues, node, fn, fallbackFile) {
  try { return fn(); } catch (e) {
    if (!(e instanceof CatError) || !issues) throw e;
    const p = posOf(node) ?? (fallbackFile ? { file: fallbackFile, line: 1, column: 1 } : {});
    issues.push({ severity: "error", source: "typecat", error_id: e.id, description: e.message, params: e.params, ...p });
    return undefined;
  }
}

function decoratorsOf(node) {
  const ds = ts.canHaveDecorators(node) ? ts.getDecorators(node) ?? [] : [];
  return ds.map((d) => {
    const e = d.expression;
    if (ts.isIdentifier(e)) return { name: e.text, args: [] };
    if (ts.isCallExpression(e) && ts.isIdentifier(e.expression)) return { name: e.expression.text, args: [...e.arguments] };
    return { name: "?", args: [] };
  });
}

function literalStrings(node) {
  // "a" | "b" | "c"  or  "a"
  if (ts.isLiteralTypeNode(node) && ts.isStringLiteral(node.literal)) return [node.literal.text];
  if (ts.isUnionTypeNode(node)) return node.types.flatMap(literalStrings);
  throw new CatError("typecat.literal_expected", "string literal type expected", { text: node.getText() });
}

function cardOf(text) {
  switch (text) {
    case "1": return { min: 1, max: 1 };
    case "0..1": return { min: 0, max: 1 };
    case "1..n": return { min: 1, max: null };
    case "0..n": return { min: 0, max: null };
    default: throw new CatError("typecat.bad_cardinality", "unknown cardinality", { card: text });
  }
}

// Convert a literal initializer (object/array/string/number/boolean) to JSON.
// `consts`: module-level consts of the same file (name -> initializer), so that a literal held in a
// const can be referenced, e.g. one node definition shared by several aspects. Static only.
function literalJson(node, consts = new Map(), seen = new Set()) {
  if (ts.isAsExpression(node) || ts.isSatisfiesExpression?.(node)) return literalJson(node.expression, consts, seen);
  if (ts.isIdentifier(node) && consts.has(node.text)) {
    if (seen.has(node.text)) throw new CatError("typecat.literal_cycle", "const refers to itself", { name: node.text });
    return literalJson(consts.get(node.text), consts, new Set([...seen, node.text]));
  }
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
  if (ts.isNumericLiteral(node)) return Number(node.text);
  if (node.kind === ts.SyntaxKind.TrueKeyword) return true;
  if (node.kind === ts.SyntaxKind.FalseKeyword) return false;
  if (ts.isArrayLiteralExpression(node)) return node.elements.map((e) => literalJson(e, consts, seen));
  if (ts.isObjectLiteralExpression(node)) {
    const o = {};
    for (const p of node.properties) {
      if (ts.isSpreadAssignment(p)) { Object.assign(o, literalJson(p.expression, consts, seen)); continue; }
      if (ts.isShorthandPropertyAssignment(p)) { o[p.name.text] = literalJson(p.name, consts, seen); continue; }
      if (!ts.isPropertyAssignment(p)) throw new CatError("typecat.literal_expected", "plain property expected", { text: p.getText() });
      o[p.name.getText().replace(/^["']|["']$/g, "")] = literalJson(p.initializer, consts, seen);
    }
    return o;
  }
  throw new CatError("typecat.literal_expected", "literal value expected", { text: node.getText() });
}

// A module root given from outside: the repo (with i3cm/module.ts) or the i3cm directory itself.
export function moduleRoot(p) {
  const abs = path.resolve(p);
  for (const d of [path.join(abs, "i3cm"), abs]) if (fs.existsSync(path.join(d, "module.ts"))) return d;
  throw new CatError("typecat.module_root", "no i3cm/module.ts in module repo", { path: abs });
}

function tsFilesUnder(dir) {
  const out = [];
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (e.name === "node_modules" || e.name.startsWith(".")) continue;
    const f = path.join(dir, e.name);
    if (e.isDirectory()) out.push(...tsFilesUnder(f));
    else if (e.name.endsWith(".ts") && !e.name.endsWith(".d.ts")) out.push(f);
  }
  return out;
}

export function modulesFromEnv() {
  return (process.env.I3CM_MODULES ?? "").split(":").filter(Boolean);
}

export function buildCatalog(typesDir, opts = {}) {
  typesDir = path.resolve(typesDir);
  const extRoots = [...new Set((opts.modules ?? []).map(moduleRoot))];
  const cfgPath = path.join(typesDir, "tsconfig.json");
  const cfg = ts.readConfigFile(cfgPath, ts.sys.readFile);
  if (cfg.error) throw new CatError("typecat.tsconfig", ts.flattenDiagnosticMessageText(cfg.error.messageText, "\n"));
  const parsed = ts.parseJsonConfigFileContent(cfg.config, ts.sys, typesDir);
  // Stable import aliases for module sources: the type library and the types of the base.
  const paths = { ...(parsed.options.paths ?? {}), "@i3cm/lib": [path.join(typesDir, "lib/index.ts")] };
  if (fs.existsSync(path.join(typesDir, "base/index.ts"))) paths["@i3cm/base"] = [path.join(typesDir, "base/index.ts")];
  // "@i3cm/<module>" for every module of the run that has an index.ts (in i3cm/ or i3cm/types/)
  for (const root of [...extRoots, ...fs.readdirSync(typesDir, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => path.join(typesDir, e.name))]) {
    const mt = path.join(root, "module.ts");
    if (!fs.existsSync(mt)) continue;
    const name = /name\s*:\s*["']([\w-]+)["']/.exec(fs.readFileSync(mt, "utf8"))?.[1];
    const index = [path.join(root, "index.ts"), path.join(root, "types/index.ts")].find((f) => fs.existsSync(f));
    if (name && index && !paths[`@i3cm/${name}`]) paths[`@i3cm/${name}`] = [index];
  }
  const files = [...parsed.fileNames, ...extRoots.flatMap(tsFilesUnder)];
  const program = ts.createProgram(files, { ...parsed.options, paths });
  const diags = ts.getPreEmitDiagnostics(program);
  const issues = opts.issues;
  const positions = {};
  if (diags.length && issues) {
    for (const d of diags) {
      const p = d.file ? d.file.getLineAndCharacterOfPosition(d.start) : null;
      issues.push({ severity: d.category === ts.DiagnosticCategory.Error ? "error" : "warning", source: "typescript",
        error_id: `ts.${d.code}`, description: ts.flattenDiagnosticMessageText(d.messageText, "\n"), params: {},
        ...(d.file ? { file: d.file.fileName, line: p.line + 1, column: p.character + 1 } : {}) });
    }
  } else if (diags.length) {
    const msgs = diags.map((d) => {
      const where = d.file ? `${d.file.fileName}:${d.file.getLineAndCharacterOfPosition(d.start).line + 1}` : "";
      return `${where} ${ts.flattenDiagnosticMessageText(d.messageText, "\n")}`;
    });
    throw new CatError("typecat.compile_error", "TypeScript reported errors", { errors: msgs });
  }
  const checker = program.getTypeChecker();

  // module directories: those containing module.ts
  const moduleDirs = [];
  for (const sf of program.getSourceFiles()) {
    if (sf.isDeclarationFile) continue;
    if (path.basename(sf.fileName) === "module.ts") moduleDirs.push(path.dirname(sf.fileName));
  }

  const nsOfClass = (decl) => {
    const d = decoratorsOf(decl).find((x) => x.name === "ns");
    return d && ts.isStringLiteral(d.args[0]) ? d.args[0].text : null;
  };
  const classFullName = (exprOrType) => {
    let sym = checker.getSymbolAtLocation(exprOrType);
    if (!sym) throw new CatError("typecat.unknown_type", "unknown type", { text: exprOrType.getText() });
    if (sym.flags & ts.SymbolFlags.Alias) sym = checker.getAliasedSymbol(sym);
    const decl = sym.declarations?.find(ts.isClassDeclaration);
    if (!decl) throw new CatError("typecat.unknown_type", "not a class", { text: exprOrType.getText() });
    if (decl.name?.text === "AnyObject") return "*";
    const n = nsOfClass(decl);
    if (!n) throw new CatError("typecat.missing_ns", "class without @ns", { class: decl.name?.text });
    return `${n}:${decl.name.text}`;
  };

  const types = [];
  const references = [];
  const textTypes = [];
  const modules = [];
  const moduleOf = new Map(); // type or reference name -> module name

  // A source file belongs to the nearest enclosing module directory (files may sit in subdirectories,
  // as in a module repo's i3cm/types/ and i3cm/aspects/).
  const ownerOf = (f) => {
    let best = null;
    for (const d of moduleDirs) if ((f === d || f.startsWith(d + path.sep)) && (!best || d.length > best.length)) best = d;
    return best;
  };
  const uiDirs = new Map(); // module name -> ui directory
  for (const dir of moduleDirs) {
    const files = program.getSourceFiles().filter((sf) => !sf.isDeclarationFile && ownerOf(path.dirname(sf.fileName)) === dir);
    const mod = { name: null, requires: [], rooms: [], profiles: [], i18n: {}, aspects: [], operations: [], hooks: [], schedules: [], carriers: [], filters: [], mandatory: false, developmentOnly: false };
    const ownTypes = [], ownRefs = [];
    for (const sf of files) {
      if (path.basename(sf.fileName) === "module.ts") {
        for (const st of sf.statements) {
          if (!ts.isVariableStatement(st)) continue;
          for (const d of st.declarationList.declarations) {
            const v = d.initializer ? literalJson(d.initializer) : null;
            if (d.name.getText() === "module") { mod.name = v.name; mod.requires = v.requires ?? []; mod.mandatory = !!v.mandatory; mod.developmentOnly = !!v.developmentOnly; }
            if (d.name.getText() === "rooms") mod.rooms = v;
            if (d.name.getText() === "profiles") mod.profiles = v;
            if (d.name.getText() === "operations") mod.operations = v;
            if (d.name.getText() === "hooks") mod.hooks = v;
            if (d.name.getText() === "schedules") mod.schedules = v;
            if (d.name.getText() === "carriers") mod.carriers = v;
            if (d.name.getText() === "filters") mod.filters = v;
          }
        }
        continue;
      }
      const consts = new Map();
      for (const st of sf.statements)
        if (ts.isVariableStatement(st))
          for (const d of st.declarationList.declarations)
            if (d.initializer && ts.isIdentifier(d.name) && !ts.isCallExpression(d.initializer)) consts.set(d.name.text, d.initializer);
      for (const st of sf.statements) {
        if (ts.isVariableStatement(st)) {
          for (const d of st.declarationList.declarations) {
            const init = d.initializer;
            if (init && ts.isCallExpression(init) && ts.isIdentifier(init.expression) && init.expression.text === "aspect") {
              guarded(issues, d, () => {
                const a = aspectOf(literalJson(init.arguments[0], consts), literalJson(init.arguments[1], consts));
                mod.aspects.push(a);
                positions[`aspect:${a.name}`] = posOf(d);
              });
            }
          }
          continue;
        }
        if (!ts.isClassDeclaration(st) || !st.name) continue;
        guarded(issues, st, () => {
          const ns = nsOfClass(st);
          if (!ns) return;
          const full = `${ns}:${st.name.text}`;
          const t = { name: full, ns, local: st.name.text, abstract: false, extends: null, attributes: [],
            requires: { mode: "any", types: [] }, imprintsOn: [], brings: [], content: null, textTypes: [] };
          const ownTexts = [], noGrep = [];
          for (const d of decoratorsOf(st)) {
            const typeArgs = () => d.args.map((a) => classFullName(a));
            switch (d.name) {
              case "ns": break;
              case "abstractType": t.abstract = true; break;
              case "requires": t.requires = { mode: "any", types: typeArgs() }; break;
              case "requiresAll": t.requires = { mode: "all", types: typeArgs() }; break;
              case "imprintsOn": t.imprintsOn = typeArgs(); break;
              case "brings": t.brings = typeArgs(); break;
              case "hasContent": t.content = { mediaType: d.args[0] ? d.args[0].text : null }; break;
              case "texts":
                for (const a of d.args) {
                  const parts = a.text.split(":");
                  if (parts.length > 2)
                    throw new CatError("typecat.bad_text", "a text type is \"name:mediaType\"", { class: full, text: a.text });
                  const [name, media] = parts;
                  const tt = { name: `${ns}:${name}`, mediaType: media ?? "text/plain", owner: full, grep: true };
                  t.textTypes.push(tt.name); textTypes.push(tt); ownTexts.push(tt);
                }
                break;
              case "noGrep": noGrep.push(...d.args.map((a) => a.text)); break;
              default: throw new CatError("typecat.unknown_decorator", "unknown class decorator", { class: full, decorator: d.name });
            }
          }
          for (const n of noGrep) {
            const tt = ownTexts.find((x) => x.name === `${ns}:${n}`);
            if (!tt) throw new CatError("typecat.unknown_text", "@noGrep names a text type the class does not declare", { class: full, text: n });
            tt.grep = false;
          }
          const ext = st.heritageClauses?.find((h) => h.token === ts.SyntaxKind.ExtendsKeyword);
          if (ext) t.extends = classFullName(ext.types[0].expression);

          for (const m of st.members) {
            if (!ts.isPropertyDeclaration(m) || !m.type) continue;
            guarded(issues, m, () => {
            const name = m.name.getText();
            const optional = !!m.questionToken;
            const decos = decoratorsOf(m);
            const has = (n) => decos.some((d) => d.name === n);
            let tn = m.type, list = false;
            if (ts.isArrayTypeNode(tn)) { list = true; tn = tn.elementType; }
            if (ts.isTypeReferenceNode(tn) && tn.typeName.getText() === "Ref") {
              const [target, end, back] = tn.typeArguments ?? [];
              if (!target || !end) throw new CatError("typecat.ref_args", "Ref<T, End> needs two arguments", { type: full, attribute: name });
              for (const d of decos) {
                if (!["partOf", "ordered", "rightsRelevant", "min"].includes(d.name))
                  throw new CatError("typecat.unknown_decorator", "decorator not allowed on reference", { type: full, attribute: name, decorator: d.name });
              }
              if (has("rightsRelevant") && issues) {
                // no longer needed: a reference is rights-relevant when a profile's rights path runs over it
                issues.push({ severity: "warning", source: "typecat", error_id: "ref.rights_relevant_ignored",
                  description: "@rightsRelevant is ignored: rights paths may run over any reference", params: { type: full, attribute: name }, ...(posOf(m) ?? {}) });
              }
              const minDeco = decos.find((d) => d.name === "min");
              const targetCard = back ? cardOf(literalStrings(back)[0]) : { min: 0, max: null };
              const holderMin = list ? (minDeco ? Number(minDeco.args[0].text) : 0) : (optional ? 0 : 1);
              return ownRefs.push({
                name: `${full}.${name}`,
                holder: { type: full, end: name, min: holderMin, max: list ? null : 1 },
                target: { type: classFullName(target.typeName ?? target), end: literalStrings(end)[0], ...targetCard },
                partOf: has("partOf"), ordered: has("ordered"),
              });
            }
            const vt = valueType(tn, full, name);
            for (const d of decos) {
              if (!["indexed", "unique"].includes(d.name))
                throw new CatError("typecat.unknown_decorator", "decorator not allowed on attribute", { type: full, attribute: name, decorator: d.name });
            }
            if (name.startsWith("_")) throw new CatError("typecat.reserved_name", "names starting with _ are reserved", { type: full, attribute: name });
            t.attributes.push({ name, vt, required: !optional, list, indexed: has("indexed") || has("unique"), unique: has("unique") });
            });
          }
          ownTypes.push(t);
          positions[`type:${full}`] = posOf(st);
        });
      }
    }
    if (!mod.name) { guarded(issues, null, () => { throw new CatError("typecat.module_name", "module.ts without module.name", { dir }); }, path.join(dir, "module.ts")); continue; }
    for (const t of ownTypes) moduleOf.set(t.name, mod.name);
    for (const r of ownRefs) moduleOf.set(r.name, mod.name);
    types.push(...ownTypes);
    references.push(...ownRefs);
    const i18nDir = path.join(dir, "i18n");
    if (fs.existsSync(i18nDir)) {
      for (const f of fs.readdirSync(i18nDir).filter((f) => f.endsWith(".ftl")).sort())
        mod.i18n[path.basename(f, ".ftl")] = fs.readFileSync(path.join(i18nDir, f), "utf8");
    }
    if (fs.existsSync(path.join(dir, "ui"))) uiDirs.set(mod.name, path.join(dir, "ui"));
    mod.profiles = mod.profiles.map((r) => ({ ...r, name: `${mod.name}:${r.name}`, includes: r.includes ?? [], guardian: !!r.guardian }));
    mod.aspects = mod.aspects.map((a) => ({ ...a, name: `${mod.name}:${a.name}` }));
    const q = (n) => (n.includes(":") ? n : `${mod.name}:${n}`);
    mod.carriers = mod.carriers.map((c) => ({ name: q(c.name), profiles: (c.profiles ?? []).map(q) }));
    // named filters: `{ name: { type, where } }` (or a list with `name`), qualified with the module
    const flist = Array.isArray(mod.filters) ? mod.filters : Object.entries(mod.filters ?? {}).map(([name, f]) => ({ name, ...f }));
    mod.filters = flist.map((f) => ({ name: q(f.name), type: f.type, where: f.where }));
    for (const f of mod.filters)
      if (f.name.split(":")[0] !== mod.name)
        throw new CatError("typecat.foreign_filter_name", "a named filter belongs to the namespace of its module", { module: mod.name, filter: f.name });
    for (const c of mod.carriers)
      if (c.name.split(":")[0] !== mod.name)
        throw new CatError("typecat.foreign_carrier_name", "a carrier belongs to the namespace of its module", { module: mod.name, carrier: c.name });
    guarded(issues, null, () => {
      const opNames = new Set(mod.operations.map((o) => o.name));
      mod.operations = mod.operations.map((o) => {
        const imp = (o.impersonates ?? []).map(q);
        for (const c of imp)
          if (!mod.carriers.some((x) => x.name === c))
            throw new CatError("typecat.foreign_carrier", "an operation may only impersonate carriers of its own module", { module: mod.name, operation: o.name, carrier: c });
        return { name: o.name, impersonates: imp };
      });
      for (const h of mod.hooks) {
        if (!opNames.has(h.op)) throw new CatError("typecat.unknown_operation", "hook names an operation the module does not declare", { module: mod.name, op: h.op });
        if (!["create", "update", "delete"].includes(h.on) || !["before", "after"].includes(h.phase))
          throw new CatError("typecat.bad_hook", "hook needs on: create|update|delete and phase: before|after", { module: mod.name, op: h.op });
      }
      for (const sc of mod.schedules) {
        if (!opNames.has(sc.op)) throw new CatError("typecat.unknown_operation", "schedule names an operation the module does not declare", { module: mod.name, op: sc.op });
        if (!/^FREQ=/.test(sc.rrule)) throw new CatError("typecat.bad_rrule", "schedule needs an RRULE starting with FREQ=", { module: mod.name, op: sc.op });
      }
    }, path.join(dir, "module.ts"));
    modules.push(mod);
  }
  const known = new Map();
  for (const m of modules) {
    guarded(issues, null, () => {
      if (known.has(m.name)) throw new CatError("typecat.duplicate_module", "module name given twice", { module: m.name });
      known.set(m.name, m);
    });
  }
  for (const m of modules)
    for (const r of m.requires)
      guarded(issues, null, () => {
        if (!known.has(r))
          throw new CatError("typecat.unknown_module", "module requires a module that is not part of the build", { module: m.name, requires: r });
      });
  guarded(issues, null, () => checkNames(types, references, moduleOf));
  types.sort((a, b) => a.name.localeCompare(b.name));
  references.sort((a, b) => a.name.localeCompare(b.name));
  modules.sort((a, b) => a.name.localeCompare(b.name));
  const catalog = { format: FORMAT, modules, types, references, textTypes };
  Object.defineProperty(catalog, "uiDirs", { value: uiDirs, enumerable: false });
  Object.defineProperty(catalog, "positions", { value: positions, enumerable: false });
  return catalog;
}

// Copy the UI template files of all modules (i3cm/ui/*.html) into the frame's modules directory.
export function copyUi(catalog, outDir) {
  fs.mkdirSync(outDir, { recursive: true });
  const copied = [];
  for (const [, dir] of catalog.uiDirs ?? []) {
    for (const f of fs.readdirSync(dir).filter((f) => f.endsWith(".html")).sort()) {
      fs.copyFileSync(path.join(dir, f), path.join(outDir, f));
      copied.push(f);
    }
  }
  return copied;
}

// Code inside the template snippets of a module's UI files (i3cm/ui/*.html): <script>, on… handlers,
// javascript: links. Allowed but strongly discouraged (snippets are cloned per object; code belongs in
// the module's ui/*.js components), so these are warnings, never errors.
export function checkUi(catalog) {
  const issues = [];
  for (const [module, dir] of catalog.uiDirs ?? []) {
    for (const f of fs.readdirSync(dir).filter((f) => f.endsWith(".html")).sort()) {
      const text = fs.readFileSync(path.join(dir, f), "utf8");
      const rules = [[/<script\b/gi, "script element"], [/\son[a-z]+\s*=/gi, "event handler attribute"], [/javascript:/gi, "javascript: link"]];
      for (const [re, what] of rules) {
        for (const m of text.matchAll(re)) {
          const line = text.slice(0, m.index).split("\n").length;
          issues.push({ severity: "warning", source: "typecat", error_id: "ui.inline_code",
            description: `${what} in a UI template snippet; put code into a component (ui/*.js)`,
            params: { module, what }, file: `i3cm/ui/${f}`, line });
        }
      }
    }
  }
  return issues;
}

// Names at a type: attributes, own reference ends and opposite ends of references pointing to it,
// including those of its supertypes. Two equal names conflict when they come from the same module
// or one of them from the base; names from unrelated modules are told apart by qualification.
function checkNames(types, references, moduleOf) {
  const own = new Map(types.map((t) => [t.name, []]));
  const add = (type, entry) => { if (own.has(type)) own.get(type).push(entry); };
  for (const t of types)
    for (const a of t.attributes) add(t.name, { name: a.name, what: `attribute ${t.name}.${a.name}`, module: moduleOf.get(t.name) });
  for (const r of references) {
    const m = moduleOf.get(r.name);
    if (r.target.end.startsWith("_"))
      throw new CatError("typecat.reserved_name", "names starting with _ are reserved", { reference: r.name, end: r.target.end });
    add(r.holder.type, { name: r.holder.end, what: `reference ${r.name}`, module: m });
    const back = { name: r.target.end, what: `opposite end of ${r.name}`, module: m };
    if (r.target.type === "*") for (const t of types) add(t.name, back);
    else add(r.target.type, back);
  }
  const byName = new Map(types.map((t) => [t.name, t]));
  for (const t of types) {
    const all = [];
    for (let cur = t, depth = 0; cur && depth < 64; cur = byName.get(cur.extends), depth++) all.push(...own.get(cur.name));
    const seen = new Map();
    for (const e of all) {
      for (const prev of seen.get(e.name) ?? []) {
        if (prev.what === e.what) continue;
        if (prev.module === e.module || prev.module === "base" || e.module === "base")
          throw new CatError("typecat.name_conflict", "name used twice at a type", { type: t.name, name: e.name, first: prev.what, second: e.what });
      }
      seen.set(e.name, [...(seen.get(e.name) ?? []), e]);
    }
  }
}

// Aspect: object literals become ordered lists (the order of keys is the display order).
function aspectOf(name, def) {
  const pairs = (o) => Object.entries(o ?? {});
  const nodes = {};
  for (const [t, n] of pairs(def.nodes)) {
    nodes[t] = {
      attributes: pairs(n.attributes).map(([label, path]) => ({ label, path })),
      subNodes: pairs(n.subNodes).map(([label, v]) => ({ label, path: v.path, folder: !!v.folder, ...(v.dropType ? { dropType: v.dropType } : {}) })),
      // a list of sources: the first present is read, writing goes to the first
      files: pairs(n.files).map(([name, path]) => {
        if (!Array.isArray(path)) return { name, path };
        if (path.length === 0) throw new CatError("aspect.empty_sources", "a file needs at least one source", { file: name });
        return { name, path: path[0], ...(path.length > 1 ? { fallback: path.slice(1) } : {}) };
      }),
      tabs: pairs(n.tabs).map(([label, v]) => ({ label, path: v.path, columns: pairs(v.columns).map(([l, p]) => ({ label: l, path: p })) })),
      functions: pairs(n.functions).map(([name, v]) => ({ name, url: v.url })),
      filesFirst: !("subNodes" in n) || !("files" in n) || Object.keys(n).indexOf("files") < Object.keys(n).indexOf("subNodes"),
    };
  }
  return { name, entry: { type: def.entry.type, groupBy: def.entry.groupBy ?? [], ...(def.entry.where ? { where: def.entry.where } : {}) }, nodes };
}

function valueType(tn, type, attr) {
  if (tn.kind === ts.SyntaxKind.StringKeyword) return { kind: "string" };
  if (tn.kind === ts.SyntaxKind.BooleanKeyword) return { kind: "boolean" };
  if (tn.kind === ts.SyntaxKind.NumberKeyword)
    throw new CatError("typecat.plain_number", "use Int or Decimal<P,S> instead of number", { type, attribute: attr });
  if (ts.isTypeReferenceNode(tn)) {
    const n = tn.typeName.getText();
    const a = tn.typeArguments ?? [];
    const num = (x) => Number(x.literal.text);
    switch (n) {
      case "Int": return { kind: "int" };
      case "Decimal": return { kind: "decimal", precision: num(a[0]), scale: num(a[1]) };
      case "Text": return { kind: "text", formats: literalStrings(a[0]) };
      case "LocalDate": return { kind: "date" };
      case "DateTime": return { kind: "datetime" };
      case "Time": return { kind: "time" };
      case "Duration": return { kind: "duration" };
      case "Enum": return { kind: "enum", values: literalStrings(a[0]) };
      case "Money": return { kind: "money" };
      case "Formatted": return { kind: "formatted", format: literalStrings(a[0])[0] };
      case "Recurrence": return { kind: "recurrence" };
      case "Struct": {
        if (!a[0] || !ts.isTypeLiteralNode(a[0])) throw new CatError("typecat.struct", "Struct<{...}> expected", { type, attribute: attr });
        const fields = a[0].members.filter(ts.isPropertySignature).map((p) => {
          const f = valueType(p.type, type, `${attr}.${p.name.getText()}`);
          if (f.kind === "struct") throw new CatError("typecat.struct_nested", "Struct fields must not be Struct", { type, attribute: attr });
          return { name: p.name.getText(), vt: f, required: !p.questionToken };
        });
        return { kind: "struct", fields };
      }
    }
  }
  throw new CatError("typecat.unknown_value_type", "unknown value type", { type, attribute: attr, text: tn.getText() });
}

// CLI
if (import.meta.url === `file://${process.argv[1]}` || process.argv[1]?.endsWith("typecat.mjs")) {
  const args = process.argv.slice(2);
  // `typecat check <dir> [--module <repo>]...`: collect all issues instead of stopping at the
  // first; prints {catalog, issues, positions} as JSON, exit 1 if there is an error.
  const checkMode = args[0] === "check";
  if (checkMode) args.shift();
  let out = null, uiOut = null, dir = null;
  const modules = modulesFromEnv();
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === "--out") out = args[++i];
    else if (a === "--ui-out") uiOut = args[++i];
    else if (a === "--module") modules.push(args[++i]);
    else if (a.startsWith("--")) { console.error(`typecat: unknown option ${a}`); process.exit(2); }
    else dir = a;
  }
  dir ??= "types";
  if (checkMode) {
    const issues = [];
    let cat = null;
    try { cat = buildCatalog(dir, { modules, issues }); issues.push(...checkUi(cat)); } catch (e) {
      if (e instanceof CatError) issues.push({ severity: "error", source: "typecat", error_id: e.id, description: e.message, params: e.params });
      else issues.push({ severity: "error", source: "typecat", error_id: "typecat.internal", description: String(e?.message ?? e), params: {} });
    }
    const failed = issues.some((i) => i.severity === "error");
    // exitCode, not exit(): a piped stdout must be flushed completely
    process.stdout.write(JSON.stringify({ catalog: failed ? null : cat, issues, positions: cat?.positions ?? {} }) + "\n");
    process.exitCode = failed ? 1 : 0;
  } else {
  try {
    const cat = buildCatalog(dir, { modules });
    const json = JSON.stringify(cat, null, 2) + "\n";
    if (out) fs.writeFileSync(out, json); else process.stdout.write(json);
    if (out) console.error(`typecat: ${cat.modules.length} modules, ${cat.types.length} types, ${cat.references.length} references -> ${out}`);
    if (uiOut) console.error(`typecat: ui templates ${copyUi(cat, uiOut).join(", ") || "(none)"} -> ${uiOut}`);
  } catch (e) {
    if (e instanceof CatError) {
      console.error(JSON.stringify({ error_id: e.id, description: e.message, params: e.params }, null, 2));
      process.exit(2);
    }
    throw e;
  }
  }
}
