import { el } from "../lib/dom.js";
import { highlight } from "../lib/highlight.js";
import { offsetOf, lineColumn } from "../lib/sources.js";

/**
 * <base-code-editor>: a plain text editor with highlighting, line numbers and markers.
 *
 * Attributes: `language` (typescript, html, plain), `readonly`, `mode` (a free word the frame's
 * stylesheet maps to a background, e.g. type, aspect, view).
 * Properties: `value` (text), `markers` ([{line, severity, message}]), `dirty` (changed since the
 * last `value` assignment or `markSaved()`).
 * Methods: `reveal(line, column)`, `insert(text)`, `markSaved()`, `focus()`.
 * Events: `change` (after typing, debounced 300 ms), `save` (Ctrl+S / Cmd+S).
 * The text is a textarea under a highlighted copy, so the browser's own editing, undo and IME work.
 */
export class BaseCodeEditor extends HTMLElement {
  static observedAttributes = ["language", "readonly", "mode"];
  #markers = [];
  #saved = "";
  #timer = 0;

  connectedCallback() {
    if (this.area) return;
    this.gutter = el("div", { class: "ce-gutter", "aria-hidden": "true" });
    this.hl = el("pre", { class: "ce-hl", "aria-hidden": "true" });
    this.area = el("textarea", { class: "ce-input", spellcheck: "false", autocapitalize: "off", autocomplete: "off",
      "aria-label": this.getAttribute("label") || "Source", wrap: "off" });
    this.replaceChildren(el("div", { class: "ce" }, this.gutter, el("div", { class: "ce-body" }, this.hl, this.area)));
    this.area.addEventListener("input", () => { this.render(); clearTimeout(this.#timer); this.#timer = setTimeout(() => this.dispatchEvent(new Event("change")), 300); });
    this.area.addEventListener("scroll", () => this.sync());
    this.area.addEventListener("keydown", (ev) => this.key(ev));
    this.attributeChangedCallback();
    this.render();
  }

  attributeChangedCallback() {
    if (!this.area) return;
    this.area.readOnly = this.hasAttribute("readonly");
    this.render();
  }

  get value() { return this.area ? this.area.value : ""; }
  set value(text) {
    if (!this.area) this.connectedCallback();
    this.area.value = text ?? "";
    this.#saved = this.area.value;
    this.area.scrollTop = 0;
    this.render();
  }
  get dirty() { return this.value !== this.#saved; }
  markSaved() { this.#saved = this.value; }

  get markers() { return this.#markers; }
  set markers(list) { this.#markers = list || []; this.render(); }

  focus() { this.area?.focus(); }

  /** Move the caret to a 1-based line and column and scroll it into view. */
  reveal(line, column = 1) {
    const at = offsetOf(this.value, line, column);
    this.area.focus();
    this.area.setSelectionRange(at, at);
    const lh = parseFloat(getComputedStyle(this.area).lineHeight) || 18;
    this.area.scrollTop = Math.max(0, (line - 3) * lh);
    this.sync();
  }

  /** Insert text at the caret (replacing a selection) and select the inserted text. */
  insert(text) {
    const a = this.area, s = a.selectionStart, e = a.selectionEnd;
    a.setRangeText(text, s, e, "select");
    a.focus();
    a.dispatchEvent(new Event("input"));
  }

  caret() { return lineColumn(this.value, this.area.selectionStart); }

  key(ev) {
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "s") { ev.preventDefault(); this.dispatchEvent(new Event("save")); return; }
    if (ev.key === "Tab" && !ev.shiftKey && !this.area.readOnly) { ev.preventDefault(); this.insert("  "); this.area.setSelectionRange(this.area.selectionEnd, this.area.selectionEnd); }
  }

  render() {
    if (!this.area) return;
    const text = this.area.value;
    // a trailing newline needs a visible line in the copy, or the last line jumps
    this.hl.innerHTML = highlight(text, this.getAttribute("language") || "plain") + "\n";
    const lines = text.split("\n").length;
    const byLine = new Map();
    for (const m of this.#markers) if (m.line) byLine.set(m.line, byLine.get(m.line) === "error" ? "error" : m.severity || "error");
    const titles = new Map();
    for (const m of this.#markers) if (m.line) titles.set(m.line, [titles.get(m.line), m.message].filter(Boolean).join("\n"));
    this.gutter.replaceChildren(...Array.from({ length: lines }, (_, i) => {
      const n = i + 1, sev = byLine.get(n);
      return el("div", { class: sev ? `ce-line ce-${sev}` : "ce-line", title: titles.get(n) || null }, String(n));
    }));
    this.sync();
  }

  sync() {
    this.hl.style.transform = `translate(${-this.area.scrollLeft}px, ${-this.area.scrollTop}px)`;
    this.gutter.style.transform = `translateY(${-this.area.scrollTop}px)`;
  }
}
customElements.define("base-code-editor", BaseCodeEditor);
