import { el, svg } from "../lib/dom.js";
import { indexCatalog, referenced, referencing } from "../lib/catalog.js";
import { diagramModel } from "../lib/diagram.js";
import { layout, LINE_H, PAD } from "../lib/layout.js";

/**
 * <base-diagram>: a simplified UML class diagram of catalog types.
 *
 * Property `catalog` (as from `GET /v1/catalog`); methods `show(...names)`, `hide(name)`,
 * `expand(name, "referenced" | "referencing")`, `pick()` (a promise for the next type the user
 * clicks, for "set reference"; Escape cancels with null).
 * Attribute `storage-key`: remembers shown types and moved boxes in localStorage under that key.
 * Events: `open-type` with `{name}` (context menu "Open"), `change` when the shown set changes.
 * Boxes can be dragged; the context menu (right click or the menu key) offers referenced and
 * referencing types, hide and open.
 */
export class BaseDiagram extends HTMLElement {
  #index = null;
  #shown = new Set();
  #pinned = {};
  #picking = null;

  connectedCallback() {
    if (this.canvas) return;
    this.canvas = svg("svg", { class: "dg-svg", role: "img", "aria-label": this.getAttribute("label") || "Type diagram" });
    this.menu = el("div", { class: "dg-menu", role: "menu", hidden: true });
    this.hint = el("div", { class: "dg-hint", hidden: true });
    this.replaceChildren(el("div", { class: "dg" }, this.canvas, this.menu, this.hint));
    document.addEventListener("click", (ev) => { if (!this.menu.contains(ev.target)) this.menu.hidden = true; });
    document.addEventListener("keydown", (ev) => {
      if (ev.key !== "Escape") return;
      this.menu.hidden = true;
      if (this.#picking) { const p = this.#picking; this.#picking = null; this.hint.hidden = true; p(null); }
    });
    this.restore();
    this.render();
  }

  set catalog(c) { this.#index = indexCatalog(c || { types: [], references: [] }); this.render(); }
  get shown() { return [...this.#shown]; }

  show(...names) { for (const n of names) this.#shown.add(n); this.changed(); }
  hide(name) { this.#shown.delete(name); delete this.#pinned[name]; this.changed(); }
  expand(name, direction) {
    if (!this.#index) return;
    this.show(name, ...(direction === "referencing" ? referencing(this.#index, name) : referenced(this.#index, name)));
  }

  /** Resolve with the next clicked type name, or null on Escape. */
  pick(prompt = "Click the target type in the diagram (Escape cancels).") {
    this.hint.textContent = prompt;
    this.hint.hidden = false;
    return new Promise((resolve) => (this.#picking = resolve));
  }

  changed() { this.persist(); this.render(); this.dispatchEvent(new Event("change")); }

  restore() {
    const key = this.getAttribute("storage-key");
    if (!key) return;
    try {
      const s = JSON.parse(localStorage.getItem(key) || "{}");
      this.#shown = new Set(s.shown || []);
      this.#pinned = s.pinned || {};
    } catch {}
  }
  persist() {
    const key = this.getAttribute("storage-key");
    if (!key) return;
    try { localStorage.setItem(key, JSON.stringify({ shown: [...this.#shown], pinned: this.#pinned })); } catch {}
  }

  render() {
    if (!this.canvas || !this.#index) return;
    const model = diagramModel(this.#index, this.#shown);
    const pos = layout(model, this.#pinned);
    const w = Math.max(200, ...Object.values(pos).map((p) => p.x + p.w)) + 20;
    const h = Math.max(80, ...Object.values(pos).map((p) => p.y + p.h)) + 20;
    this.canvas.setAttribute("viewBox", `-10 -10 ${w} ${h}`);
    this.canvas.setAttribute("width", w);
    this.canvas.setAttribute("height", h);
    const defs = svg("defs", {},
      svg("marker", { id: "dg-tri", viewBox: "0 0 12 12", refX: "11", refY: "6", markerWidth: "12", markerHeight: "12", orient: "auto-start-reverse" },
        svg("path", { d: "M1 1 L11 6 L1 11 z", class: "dg-hollow" })),
      svg("marker", { id: "dg-dia", viewBox: "0 0 14 10", refX: "1", refY: "5", markerWidth: "14", markerHeight: "10", orient: "auto-start-reverse" },
        svg("path", { d: "M1 5 L7 1 L13 5 L7 9 z", class: "dg-filled" })),
      svg("marker", { id: "dg-arrow", viewBox: "0 0 10 10", refX: "9", refY: "5", markerWidth: "8", markerHeight: "8", orient: "auto-start-reverse" },
        svg("path", { d: "M1 1 L9 5 L1 9", class: "dg-open" })));
    const edges = model.edges.map((e) => this.edge(e, pos[e.from], pos[e.to]));
    const boxes = model.boxes.map((b) => this.box(b, pos[b.id]));
    this.canvas.replaceChildren(defs, ...edges, ...boxes);
    if (!model.boxes.length) this.canvas.append(svg("text", { x: 0, y: 20, class: "dg-empty" }, "No types shown."));
  }

  box(b, p) {
    const g = svg("g", { class: `dg-box${b.abstract ? " dg-abstract" : ""}`, transform: `translate(${p.x},${p.y})`, tabindex: "0", role: "button", "aria-label": b.id });
    g.append(svg("rect", { width: p.w, height: p.h, rx: 4 }),
      svg("text", { x: PAD, y: PAD + LINE_H - 5, class: "dg-title" }, b.abstract ? `«abstract» ${b.title}` : b.title),
      svg("line", { x1: 0, x2: p.w, y1: PAD + LINE_H, y2: PAD + LINE_H }),
      ...b.attributes.map((a, i) => svg("text", { x: PAD, y: PAD + LINE_H * (i + 2) - 3, class: "dg-attr" }, `${a.name}${a.optional ? "?" : ""}: ${a.type}`)),
      svg("title", {}, b.id));
    g.addEventListener("contextmenu", (ev) => { ev.preventDefault(); this.openMenu(b.id, ev.clientX, ev.clientY); });
    g.addEventListener("keydown", (ev) => {
      if (ev.key === "ContextMenu" || (ev.shiftKey && ev.key === "F10")) { ev.preventDefault(); const r = g.getBoundingClientRect(); this.openMenu(b.id, r.left + 10, r.top + 10); }
      if (ev.key === "Enter" && this.#picking) this.picked(b.id);
    });
    g.addEventListener("pointerdown", (ev) => this.drag(ev, b.id, p, g));
    return g;
  }

  picked(name) {
    const p = this.#picking;
    this.#picking = null;
    this.hint.hidden = true;
    p?.(name);
  }

  drag(ev, id, p, g) {
    if (ev.button !== 0) return;
    if (this.#picking) { ev.preventDefault(); this.picked(id); return; }
    const scale = this.canvas.viewBox.baseVal.width / this.canvas.getBoundingClientRect().width || 1;
    const sx = ev.clientX, sy = ev.clientY, ox = p.x, oy = p.y;
    let moved = false;
    const move = (e) => {
      const dx = (e.clientX - sx) * scale, dy = (e.clientY - sy) * scale;
      if (Math.abs(dx) + Math.abs(dy) > 3) moved = true;
      g.setAttribute("transform", `translate(${ox + dx},${oy + dy})`);
      this.#pinned[id] = { x: Math.round(ox + dx), y: Math.round(oy + dy) };
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      if (moved) { this.persist(); this.render(); }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  openMenu(name, x, y) {
    const item = (label, fn) => el("button", { role: "menuitem", class: "dg-item", onclick: () => { this.menu.hidden = true; fn(); } }, label);
    this.menu.replaceChildren(
      item("Show referenced types", () => this.expand(name, "referenced")),
      item("Show referencing types", () => this.expand(name, "referencing")),
      item("Hide", () => this.hide(name)),
      item("Open", () => this.dispatchEvent(new CustomEvent("open-type", { detail: { name } }))));
    const r = this.getBoundingClientRect();
    Object.assign(this.menu.style, { left: `${x - r.left}px`, top: `${y - r.top}px` });
    this.menu.hidden = false;
    this.menu.querySelector("button")?.focus();
  }

  edge(e, a, b) {
    const [x1, y1] = border(a, center(b));
    const [x2, y2] = border(b, center(a));
    const g = svg("g", { class: `dg-edge dg-${e.kind}` });
    const line = svg("line", { x1, y1, x2, y2 });
    if (e.kind === "extends") line.setAttribute("marker-end", "url(#dg-tri)");
    else if (e.kind === "reference") {
      line.setAttribute("marker-end", "url(#dg-arrow)");
      if (e.partOf) line.setAttribute("marker-end", "url(#dg-dia)");
    } else line.setAttribute("marker-end", "url(#dg-arrow)");
    g.append(line);
    const label = (x, y, text, anchor) => svg("text", { x, y, class: "dg-label", "text-anchor": anchor }, text);
    const along = (t, off) => [x1 + (x2 - x1) * t + off[0], y1 + (y2 - y1) * t + off[1]];
    const normal = (() => { const dx = x2 - x1, dy = y2 - y1, l = Math.hypot(dx, dy) || 1; return [(-dy / l) * 10, (dx / l) * 10]; })();
    if (e.kind === "reference") {
      // own end name sits at the target (that is how the holder reaches it), the opposite end at the holder
      const [tx, ty] = along(0.85, normal), [hx, hy] = along(0.15, normal);
      g.append(label(tx, ty, `${e.fromEnd} ${e.fromCard}`, "middle"), label(hx, hy, `${e.toEnd} ${e.toCard}`, "middle"));
    } else if (e.kind !== "extends") {
      const [mx, my] = along(0.5, normal);
      g.append(label(mx, my, `«${e.kind}»`, "middle"));
    }
    return g;
  }
}

function center(p) { return [p.x + p.w / 2, p.y + p.h / 2]; }
/** Point where the line from the box centre towards (tx, ty) leaves the box. */
function border(p, [tx, ty]) {
  const [cx, cy] = center(p), dx = tx - cx, dy = ty - cy;
  if (!dx && !dy) return [cx, cy];
  const s = Math.min(dx ? (p.w / 2) / Math.abs(dx) : Infinity, dy ? (p.h / 2) / Math.abs(dy) : Infinity);
  return [cx + dx * s, cy + dy * s];
}

customElements.define("base-diagram", BaseDiagram);
