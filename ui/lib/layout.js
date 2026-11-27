// Layered layout for the UML boxes, without external libraries.

export const CHAR_W = 7;
export const LINE_H = 18;
export const PAD = 10;
export const GAP_X = 60;
export const GAP_Y = 50;

/** Width and height of a box from its title and attribute lines. */
export function boxSize(box) {
  const lines = [box.title, ...box.attributes.map((a) => `${a.name}${a.optional ? "?" : ""}: ${a.type}`)];
  const w = Math.max(120, Math.max(...lines.map((l) => l.length)) * CHAR_W + 2 * PAD);
  const h = LINE_H * (1 + Math.max(1, box.attributes.length)) + 2 * PAD;
  return { w, h };
}

/**
 * Positions for all boxes. Edges point from the referencing type to the referenced one; the
 * referenced types end up in upper layers. Boxes in `pinned` ({id: {x, y}}) keep their place,
 * the others are moved right until they overlap no other box.
 */
export function layout(model, pinned = {}) {
  const ids = model.boxes.map((b) => b.id);
  const layer = layers(ids, model.edges);
  const rows = new Map();
  for (const id of ids) {
    const l = layer.get(id);
    if (!rows.has(l)) rows.set(l, []);
    rows.get(l).push(id);
  }
  const sizes = new Map(model.boxes.map((b) => [b.id, boxSize(b)]));
  const pos = {};
  let y = 0;
  for (const l of [...rows.keys()].sort((a, b) => a - b)) {
    let x = 0;
    let rowH = 0;
    for (const id of rows.get(l).sort()) {
      const { w, h } = sizes.get(id);
      pos[id] = { x, y, w, h };
      x += w + GAP_X;
      rowH = Math.max(rowH, h);
    }
    y += rowH + GAP_Y;
  }
  for (const [id, p] of Object.entries(pinned)) if (pos[id]) Object.assign(pos[id], { x: p.x, y: p.y });
  const fixed = Object.keys(pinned).filter((id) => pos[id]);
  const placed = [...fixed];
  for (const id of ids) {
    if (fixed.includes(id)) continue;
    while (placed.some((o) => overlaps(pos[id], pos[o]))) pos[id].x += GAP_X;
    placed.push(id);
  }
  return pos;
}

export function overlaps(a, b) {
  return a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
}

/** Layer per box: longest path to a box without outgoing edges; cycles are broken by visit order. */
function layers(ids, edges) {
  const out = new Map(ids.map((id) => [id, []]));
  for (const e of edges) if (e.from !== e.to && out.has(e.from) && out.has(e.to)) out.get(e.from).push(e.to);
  const depth = new Map();
  const visiting = new Set();
  const visit = (id) => {
    if (depth.has(id)) return depth.get(id);
    if (visiting.has(id)) return 0;
    visiting.add(id);
    let d = 0;
    for (const to of out.get(id)) d = Math.max(d, visit(to) + 1);
    visiting.delete(id);
    depth.set(id, d);
    return d;
  };
  for (const id of [...ids].sort()) visit(id);
  return depth;
}
