#!/usr/bin/env node
// typecat: reads base-erp type definitions (TypeScript) and writes catalog.json.
// Usage: typecat <typesDir> [--out catalog.json]
import ts from "typescript";
import fs from "node:fs";
import path from "node:path";

export const FORMAT = 1;

class CatError extends Error {
  constructor(id, description, params = {}) { super(description); this.id = id; this.params = params; }
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
function literalJson(node) {
  if (ts.isAsExpression(node) || ts.isSatisfiesExpression?.(node)) return literalJson(node.expression);
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
  if (ts.isNumericLiteral(node)) return Number(node.text);
  if (node.kind === ts.SyntaxKind.TrueKeyword) return true;
  if (node.kind === ts.SyntaxKind.FalseKeyword) return false;
  if (ts.isArrayLiteralExpression(node)) return node.elements.map(literalJson);
  if (ts.isObjectLiteralExpression(node)) {
    const o = {};
    for (const p of node.properties) {
      if (!ts.isPropertyAssignment(p)) throw new CatError("typecat.literal_expected", "plain property expected", { text: p.getText() });
      o[p.name.getText().replace(/^["']|["']$/g, "")] = literalJson(p.initializer);
    }
    return o;
  }
  throw new CatError("typecat.literal_expected", "literal value expected", { text: node.getText() });
}

export function buildCatalog(typesDir) {
  typesDir = path.resolve(typesDir);
  const cfgPath = path.join(typesDir, "tsconfig.json");
  const cfg = ts.readConfigFile(cfgPath, ts.sys.readFile);
  if (cfg.error) throw new CatError("typecat.tsconfig", ts.flattenDiagnosticMessageText(cfg.error.messageText, "\n"));
  const parsed = ts.parseJsonConfigFileContent(cfg.config, ts.sys, typesDir);
  const program = ts.createProgram(parsed.fileNames, parsed.options);
  const diags = ts.getPreEmitDiagnostics(program);
  if (diags.length) {
    const msgs = diags.map((d) => {
      const where = d.file ? `${path.relative(typesDir, d.file.fileName)}:${d.file.getLineAndCharacterOfPosition(d.start).line + 1}` : "";
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

  for (const dir of moduleDirs) {
    const files = program.getSourceFiles().filter((sf) => !sf.isDeclarationFile && path.dirname(sf.fileName) === dir);
    const mod = { name: null, requires: [], rooms: [], roles: [], i18n: {}, aspects: [] };
    const ownTypes = [], ownRefs = [];
    for (const sf of files) {
      if (path.basename(sf.fileName) === "module.ts") {
        for (const st of sf.statements) {
          if (!ts.isVariableStatement(st)) continue;
          for (const d of st.declarationList.declarations) {
            const v = d.initializer ? literalJson(d.initializer) : null;
            if (d.name.getText() === "module") { mod.name = v.name; mod.requires = v.requires ?? []; }
            if (d.name.getText() === "rooms") mod.rooms = v;
            if (d.name.getText() === "roles") mod.roles = v;
          }
        }
        continue;
      }
      for (const st of sf.statements) {
        if (ts.isVariableStatement(st)) {
          for (const d of st.declarationList.declarations) {
            const init = d.initializer;
            if (init && ts.isCallExpression(init) && ts.isIdentifier(init.expression) && init.expression.text === "aspect") {
              mod.aspects.push(aspectOf(literalJson(init.arguments[0]), literalJson(init.arguments[1])));
            }
          }
          continue;
        }
        if (!ts.isClassDeclaration(st) || !st.name) continue;
        const ns = nsOfClass(st);
        if (!ns) continue;
        const full = `${ns}:${st.name.text}`;
        const t = { name: full, ns, local: st.name.text, abstract: false, extends: null, attributes: [],
          requires: { mode: "any", types: [] }, imprintsOn: [], brings: [], content: null, textTypes: [] };
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
                const [name, media] = a.text.split(":");
                const tt = { name: `${ns}:${name}`, mediaType: media ?? "text/plain", owner: full };
                t.textTypes.push(tt.name); textTypes.push(tt);
              }
              break;
            default: throw new CatError("typecat.unknown_decorator", "unknown class decorator", { class: full, decorator: d.name });
          }
        }
        const ext = st.heritageClauses?.find((h) => h.token === ts.SyntaxKind.ExtendsKeyword);
        if (ext) t.extends = classFullName(ext.types[0].expression);

        for (const m of st.members) {
          if (!ts.isPropertyDeclaration(m) || !m.type) continue;
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
            const minDeco = decos.find((d) => d.name === "min");
            const targetCard = back ? cardOf(literalStrings(back)[0]) : { min: 0, max: null };
            const holderMin = list ? (minDeco ? Number(minDeco.args[0].text) : 0) : (optional ? 0 : 1);
            ownRefs.push({
              name: `${full}.${name}`,
              holder: { type: full, end: name, min: holderMin, max: list ? null : 1 },
              target: { type: classFullName(target.typeName ?? target), end: literalStrings(end)[0], ...targetCard },
              partOf: has("partOf"), ordered: has("ordered"), rightsRelevant: has("rightsRelevant"),
            });
            continue;
          }
          const vt = valueType(tn, full, name);
          for (const d of decos) {
            if (!["indexed", "unique"].includes(d.name))
              throw new CatError("typecat.unknown_decorator", "decorator not allowed on attribute", { type: full, attribute: name, decorator: d.name });
          }
          if (name.startsWith("_")) throw new CatError("typecat.reserved_name", "names starting with _ are reserved", { type: full, attribute: name });
          t.attributes.push({ name, vt, required: !optional, list, indexed: has("indexed") || has("unique"), unique: has("unique") });
        }
        ownTypes.push(t);
      }
    }
    if (!mod.name) throw new CatError("typecat.module_name", "module.ts without module.name", { dir });
    for (const t of ownTypes) moduleOf.set(t.name, mod.name);
    for (const r of ownRefs) moduleOf.set(r.name, mod.name);
    types.push(...ownTypes);
    references.push(...ownRefs);
    const i18nDir = path.join(dir, "i18n");
    if (fs.existsSync(i18nDir)) {
      for (const f of fs.readdirSync(i18nDir).filter((f) => f.endsWith(".ftl")).sort())
        mod.i18n[path.basename(f, ".ftl")] = fs.readFileSync(path.join(i18nDir, f), "utf8");
    }
    mod.roles = mod.roles.map((r) => ({ ...r, name: `${mod.name}:${r.name}`, includes: r.includes ?? [], guardian: !!r.guardian }));
    mod.aspects = mod.aspects.map((a) => ({ ...a, name: `${mod.name}:${a.name}` }));
    modules.push(mod);
  }
  checkNames(types, references, moduleOf);
  types.sort((a, b) => a.name.localeCompare(b.name));
  references.sort((a, b) => a.name.localeCompare(b.name));
  modules.sort((a, b) => a.name.localeCompare(b.name));
  return { format: FORMAT, modules, types, references, textTypes };
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
      subNodes: pairs(n.subNodes).map(([label, v]) => ({ label, path: v.path, folder: !!v.folder })),
      files: pairs(n.files).map(([name, path]) => ({ name, path })),
      tabs: pairs(n.tabs).map(([label, v]) => ({ label, path: v.path, columns: pairs(v.columns).map(([l, p]) => ({ label: l, path: p })) })),
      functions: pairs(n.functions).map(([name, v]) => ({ name, url: v.url })),
      filesFirst: !("subNodes" in n) || !("files" in n) || Object.keys(n).indexOf("files") < Object.keys(n).indexOf("subNodes"),
    };
  }
  return { name, entry: { type: def.entry.type, groupBy: def.entry.groupBy ?? [] }, nodes };
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
  const outIdx = args.indexOf("--out");
  const out = outIdx >= 0 ? args[outIdx + 1] : null;
  const dir = args.find((a, i) => !a.startsWith("--") && i !== outIdx + 1) ?? "types";
  try {
    const cat = buildCatalog(dir);
    const json = JSON.stringify(cat, null, 2) + "\n";
    if (out) fs.writeFileSync(out, json); else process.stdout.write(json);
    if (out) console.error(`typecat: ${cat.types.length} types, ${cat.references.length} references -> ${out}`);
  } catch (e) {
    if (e instanceof CatError) {
      console.error(JSON.stringify({ error_id: e.id, description: e.message, params: e.params }, null, 2));
      process.exit(2);
    }
    throw e;
  }
}
