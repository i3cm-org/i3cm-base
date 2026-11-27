// Read-only helpers over a catalog produced by typecat (format 1).

/** Cardinality text from min/max as used in `Ref<T, "end", "card">`; max null = unbounded. */
export function card(min, max) {
  if (max === null || max === undefined) return min > 0 ? `${min}..n` : "0..n";
  if (min === max) return String(min);
  return `${min}..${max}`;
}

/** TypeScript-like text for a value type of the catalog (`vt`). */
export function valueType(vt) {
  if (!vt || !vt.kind) return "unknown";
  switch (vt.kind) {
    case "string": return "string";
    case "boolean": return "boolean";
    case "int": return "Int";
    case "decimal": return `Decimal<${vt.precision ?? "?"},${vt.scale ?? "?"}>`;
    case "text": return `Text<"${vt.format ?? "TXT"}">`;
    case "date": return "LocalDate";
    case "datetime": return "DateTime";
    case "time": return "Time";
    case "duration": return "Duration";
    case "money": return "Money";
    case "recurrence": return "Recurrence";
    case "sequence": return "Sequence";
    case "gaplessSequence": return "GaplessSequence";
    case "enum": return Array.isArray(vt.values) ? `Enum<${vt.values.map((v) => `"${v}"`).join(" | ")}>` : "Enum";
    case "formatted": return `Formatted<"${vt.format ?? "?"}">`;
    case "struct": return "Struct";
    default: return vt.kind;
  }
}

/** Index of a catalog: types by name, references by holder and by target. */
export function indexCatalog(catalog) {
  const types = new Map();
  for (const t of catalog.types ?? []) types.set(t.name, t);
  const outgoing = new Map();
  const incoming = new Map();
  for (const r of catalog.references ?? []) {
    push(outgoing, r.holder.type, r);
    push(incoming, r.target.type, r);
  }
  return { catalog, types, outgoing, incoming };
}

function push(map, key, value) {
  if (!map.has(key)) map.set(key, []);
  map.get(key).push(value);
}

/**
 * Packages for the left column: one per module, with its types, aspects, profiles and rooms.
 * `editable` names the module under development; all others are read-only.
 */
export function packages(catalog, editable) {
  const byNs = new Map();
  for (const t of catalog.types ?? []) push(byNs, t.ns, t.name);
  return (catalog.modules ?? []).map((m) => ({
    name: m.name,
    requires: m.requires ?? [],
    editable: m.name === editable,
    types: (byNs.get(m.name) ?? []).slice().sort(),
    aspects: (m.aspects ?? []).map((a) => a.name).sort(),
    profiles: (m.profiles ?? []).map((p) => p.name).sort(),
    rooms: (m.rooms ?? []).slice().sort(),
  })).sort((a, b) => (b.editable - a.editable) || a.name.localeCompare(b.name));
}

/** Types the given type points to: reference targets, supertype, required, brought and imprinted types. */
export function referenced(index, name) {
  const t = index.types.get(name);
  const out = new Set();
  for (const r of index.outgoing.get(name) ?? []) out.add(r.target.type);
  if (t) {
    if (t.extends) out.add(t.extends);
    for (const x of t.requires?.types ?? []) out.add(x);
    for (const x of t.brings ?? []) out.add(x);
    for (const x of t.imprintsOn ?? []) out.add(x);
  }
  out.delete(name);
  return [...out].filter((n) => index.types.has(n)).sort();
}

/** Types that point to the given type (the inverse of `referenced`). */
export function referencing(index, name) {
  const out = new Set();
  for (const r of index.incoming.get(name) ?? []) out.add(r.holder.type);
  for (const t of index.types.values()) {
    if (t.name === name) continue;
    if (t.extends === name || (t.requires?.types ?? []).includes(name)
      || (t.brings ?? []).includes(name) || (t.imprintsOn ?? []).includes(name)) out.add(t.name);
  }
  out.delete(name);
  return [...out].sort();
}
