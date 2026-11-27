// Simplified UML model: boxes for the visible types, edges only between visible types.
import { card, valueType } from "./catalog.js";

/**
 * @param index result of indexCatalog
 * @param visible iterable of type names to show
 * @returns {{boxes: object[], edges: object[]}}
 */
export function diagramModel(index, visible) {
  const shown = new Set([...visible].filter((n) => index.types.has(n)));
  const boxes = [...shown].sort().map((name) => {
    const t = index.types.get(name);
    return {
      id: name,
      ns: t.ns,
      title: t.local,
      abstract: !!t.abstract,
      attributes: (t.attributes ?? []).map((a) => ({
        name: a.name,
        type: valueType(a.vt) + (a.list ? "[]" : ""),
        optional: !a.required,
      })),
    };
  });
  const edges = [];
  for (const name of shown) {
    const t = index.types.get(name);
    for (const r of index.outgoing.get(name) ?? []) {
      if (!shown.has(r.target.type)) continue;
      edges.push({
        kind: "reference",
        from: name,
        to: r.target.type,
        fromEnd: r.holder.end,
        toEnd: r.target.end,
        fromCard: card(r.holder.min, r.holder.max),
        toCard: card(r.target.min, r.target.max),
        partOf: !!r.partOf,
      });
    }
    if (t.extends && shown.has(t.extends)) edges.push({ kind: "extends", from: name, to: t.extends });
    const rel = [["requires", t.requires?.types ?? []], ["brings", t.brings ?? []], ["imprintsOn", t.imprintsOn ?? []]];
    for (const [kind, list] of rel) for (const to of list) if (shown.has(to)) edges.push({ kind, from: name, to });
  }
  edges.sort((a, b) => (a.from + a.to + a.kind + (a.fromEnd ?? "")).localeCompare(b.from + b.to + b.kind + (b.fromEnd ?? "")));
  return { boxes, edges };
}

/** Text inserted by "set reference": `name!: Ref<Target, "end">;` with placeholders for both names. */
export function referenceSnippet(targetLocal, ownEnd = "target", oppositeEnd = "sources") {
  return `${ownEnd}!: Ref<${targetLocal}, "${oppositeEnd}">;`;
}
