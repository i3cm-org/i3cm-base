import { el } from "../lib/dom.js";
import { packages } from "../lib/catalog.js";
import { fromCheckApi, modeOf, positionOf } from "../lib/issues.js";
import { languageOf } from "../lib/highlight.js";
import { locate, contentOp } from "../lib/sources.js";
import { referenceSnippet } from "../lib/diagram.js";
import "./code-editor.js";
import "./diagram.js";
import "./issue-list.js";

/**
 * <base-source-workbench>: edit source files that are stored as objects with content, check them
 * with the server, and show the catalog as packages and as a diagram. Built from
 * <base-diagram>, <base-code-editor> and <base-issue-list>; it knows no module.
 *
 * Attributes (all required unless noted):
 * - `container-type`: type of the object that holds the files (one per module), e.g. `studio:ModuleSource`;
 * - `name-attr`: attribute of the container with the module name;
 * - `file-type`, `path-attr`: type of a file object and its attribute with the relative path;
 * - `file-end`: end at the container that lists its files; `file-reference`: the reference to link a new file;
 * - `room` (optional): name of the module room for new containers (`GET /v1/rooms`), else `root`;
 * - `check` (optional): URL of the check (`POST {modules: [{name, files: [{path, text}]}]}` answering
 *   `{catalog, issues, positions}`, e.g. `/v1/catalog/check`); without it, or when the server lacks
 *   it, the installed catalog (`GET /v1/catalog`) is shown and no issues. While the sources have
 *   errors the check returns no catalog, and the last good one stays.
 * Property `api`: the frame's authenticated fetch (`(path, options) => Response`), set by the frame.
 */
export class BaseSourceWorkbench extends HTMLElement {
  files = [];          // [{id, path, text, saved}]
  container = null;    // {id, name, room}
  catalog = null;
  positions = {};
  mode = "type";
  current = null;      // file in the editor
  checkAvailable = true;
  #seq = 0;
  #timer = 0;

  a(name) { return this.getAttribute(name); }

  connectedCallback() {
    if (this.diagram) return;
    this.select = el("select", { class: "wb-select", "aria-label": "Module", onchange: () => this.open(this.select.value) });
    this.diagram = el("base-diagram", { label: "Types" });
    this.packagesBox = el("div", { class: "wb-packages" });
    this.fileSelect = el("select", { class: "wb-select", "aria-label": "File", onchange: () => this.edit(this.files.find((f) => f.id === this.fileSelect.value)) });
    this.modes = el("fluent-tablist", { class: "wb-modes", size: "small" },
      el("fluent-tab", { id: "wb-type" }, "Type"), el("fluent-tab", { id: "wb-aspect" }, "Aspect"), el("fluent-tab", { id: "wb-view" }, "View"));
    this.modes.addEventListener("change", () => this.setMode(this.modes.activeid?.slice(3)));
    this.editor = el("base-code-editor", { label: "Source", mode: "type" });
    this.state = el("span", { class: "wb-state" });
    this.issues = el("base-issue-list", { label: "Issues" });
    const button = (label, fn, title) => el("fluent-button", { size: "small", onclick: fn, title }, label);
    this.replaceChildren(el("div", { class: "wb" },
      el("div", { class: "wb-bar" }, el("strong", {}, "Module"), this.select, button("New module", () => this.newModule()),
        el("span", { class: "spacer" }), this.state),
      el("div", { class: "wb-top" }, this.diagram),
      el("div", { class: "wb-cols" },
        el("div", { class: "wb-left" }, this.packagesBox),
        el("div", { class: "wb-mid" },
          el("div", { class: "wb-bar" }, this.modes, this.fileSelect,
            button("Save", () => this.save(), "Ctrl+S"), button("New file", () => this.newFile()), button("Delete file", () => this.deleteFile()),
            button("Set reference", () => this.setReference(), "Show a type above and insert a reference to it")),
          this.editor),
        el("div", { class: "wb-right" }, this.issues))));
    this.editor.addEventListener("change", () => { this.bufferChanged(); });
    this.editor.addEventListener("save", () => this.save());
    this.issues.addEventListener("reveal", (ev) => this.reveal(ev.detail));
    this.diagram.addEventListener("open-type", (ev) => this.goTo("type", ev.detail.name));
    window.addEventListener("beforeunload", (ev) => { if (this.dirtyFiles().length) { ev.preventDefault(); ev.returnValue = ""; } });
    if (this.api) this.start();
  }

  set api(fn) { this._api = fn; if (this.diagram && !this.started) this.start(); }
  get api() { return this._api; }

  async json(path, opts) { return (await this._api(path, opts)).json(); }
  order(ops) { return this.json("/v1/orders", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ops }) }); }

  async start() {
    this.started = true;
    try {
      const list = await this.json(`/v1/types/${encodeURIComponent(this.a("container-type"))}/objects?limit=200`);
      this.select.replaceChildren(...list.map((o) => el("option", { value: o.id }, o.displayName)));
      if (list.length) await this.open(list[0].id);
      else { this.state.textContent = "No module yet: create one with New module."; await this.check(); }
    } catch (e) { this.state.textContent = e.message; }
  }

  /** Attribute value of a type part from `GET /v1/objects/{id}`. */
  attr(obj, type, name) { return obj.parts?.find((p) => p.type === type)?.attributes?.[name]; }

  async open(id) {
    if (this.dirtyFiles().length && !confirm("Discard unsaved changes?")) { this.select.value = this.container?.id; return; }
    this.state.textContent = "Loading…";
    const obj = await this.json(`/v1/objects/${id}`);
    this.container = { id, name: this.attr(obj, this.a("container-type"), this.a("name-attr")), room: obj.room };
    this.diagram.setAttribute("storage-key", `wb:${this.container.name}`);
    this.diagram.restore();
    const ends = await this.json(`/v1/objects/${id}/ends/${encodeURIComponent(this.a("file-end"))}?limit=1000`);
    this.files = await Promise.all(ends.map(async (s) => {
      const [o, text] = await Promise.all([this.json(`/v1/objects/${s.id}`), this._api(`/v1/objects/${s.id}/content`).then((r) => r.text()).catch(() => "")]);
      return { id: s.id, path: this.attr(o, this.a("file-type"), this.a("path-attr")) || s.displayName, text, saved: text };
    }));
    this.files.sort((x, y) => x.path.localeCompare(y.path));
    this.edit(this.files.find((f) => f.path === "module.ts") || this.files[0] || null);
    this.state.textContent = "";
    await this.check();
  }

  dirtyFiles() { this.capture(); return this.files.filter((f) => f.text !== f.saved); }
  capture() { if (this.current) this.current.text = this.editor.value; }

  edit(file) {
    this.capture();
    this.current = file;
    if (file) this.setMode(modeOf(file.path), false);
    this.fillFiles();
    this.editor.setAttribute("language", languageOf(file?.path));
    this.editor.value = file?.text ?? "";
    this.editor.markSaved();
    if (file) this.editor.toggleAttribute("readonly", false);
    else this.editor.setAttribute("readonly", "");
    this.markers();
  }

  setMode(mode, refill = true) {
    this.mode = mode || "type";
    this.editor.setAttribute("mode", this.mode);
    if (this.modes.activeid !== `wb-${this.mode}`) this.modes.activeid = `wb-${this.mode}`;
    if (refill) {
      this.fillFiles();
      if (this.current && modeOf(this.current.path) !== this.mode) this.edit(this.files.find((f) => modeOf(f.path) === this.mode) || null);
    }
  }

  fillFiles() {
    const list = this.files.filter((f) => modeOf(f.path) === this.mode);
    this.fileSelect.replaceChildren(...list.map((f) => el("option", { value: f.id, selected: f === this.current }, f.text !== f.saved ? `● ${f.path}` : f.path)));
  }

  bufferChanged() {
    this.capture();
    this.fillFiles();
    clearTimeout(this.#timer);
    this.#timer = setTimeout(() => this.check(), 500);
  }

  /** Check the unsaved state of all files; show catalog, packages, diagram and issues. */
  async check() {
    const seq = ++this.#seq;
    this.capture();
    let result = null;
    const url = this.a("check");
    if (url && this.checkAvailable && this.container) {
      this.issues.note = "Checking…";
      try {
        const r = await this._api(url, { method: "POST", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ modules: [{ name: this.container.name, files: this.files.map((f) => ({ path: f.path, text: f.text })) }] }) });
        result = await r.json();
      } catch (e) {
        if (/\((404|405)\)$/.test(e.message)) this.checkAvailable = false;
        else { this.issues.note = `Check failed: ${e.message}`; }
      }
    }
    if (seq !== this.#seq) return;
    if (result) {
      this.catalog = result.catalog || this.catalog;
      this.positions = result.positions || {};
      this.issues.issues = (result.issues || []).map((i) => fromCheckApi(i, this.container.name));
    } else {
      if (!this.catalog) this.catalog = await this.json("/v1/catalog").catch(() => ({ modules: [], types: [], references: [] }));
      this.positions = {};
      if (!url || !this.checkAvailable) this.issues.note = "The server offers no check yet: the diagram shows the installed catalog.";
    }
    this.diagram.catalog = this.catalog;
    if (!this.diagram.shown.length) {
      const own = (this.catalog.types || []).filter((t) => t.ns === this.container?.name).map((t) => t.name);
      if (own.length) this.diagram.show(...own.slice(0, 6));
    }
    this.renderPackages();
    this.markers();
  }

  markers() {
    const list = this.issues.issues || [];
    this.editor.markers = this.current ? list.filter((i) => i.file === this.current.path).map((i) => ({ line: i.line, severity: i.severity, message: i.description })) : [];
  }

  renderPackages() {
    const groups = [["Types", "types", "type"], ["Aspects", "aspects", "aspect"], ["Profiles", "profiles", "profile"], ["Rooms", "rooms", "room"]];
    this.packagesBox.replaceChildren(...packages(this.catalog || {}, this.container?.name).map((p) => {
      const d = el("details", { class: "wb-pkg", open: p.editable });
      d.append(el("summary", {}, p.name, p.editable ? el("span", { class: "count" }, "editing") : el("span", { class: "count", title: "read only" }, "🔒")));
      for (const [label, key, kind] of groups) {
        if (!p[key].length) continue;
        d.append(el("div", { class: "wb-group" }, label), el("ul", { class: "wb-items" }, p[key].map((name) =>
          el("li", {}, el("a", { class: "link", onclick: () => this.goTo(kind, name, p.editable) }, name.includes(":") ? name.split(":")[1] : name)))));
      }
      return d;
    }));
  }

  /** Open the file of a catalog element at its declaration and show a type in the diagram. */
  async goTo(kind, name, editable = true) {
    if (kind === "type") this.diagram.show(name);
    const module = name.includes(":") ? name.split(":")[0] : null;
    if (!editable && module && module !== this.container?.name) return this.showInstalled(module, kind, name);
    const pos = positionOf(this.positions, kind, name, this.container?.name) || locate(this.files, kind, name);
    if (!pos) { this.state.textContent = `${name}: not in the sources of this module`; return; }
    this.reveal(pos);
  }

  /** Sources of an installed module (`GET /v1/modules/{name}/sources`), shown read only. */
  async showInstalled(module, kind, name) {
    this.installed ??= {};
    try {
      this.installed[module] ??= (await this.json(`/v1/modules/${encodeURIComponent(module)}/sources`)).files
        .map((f) => ({ path: f.path.replace(/^i3cm\//, ""), text: f.text }));
    } catch (e) { this.state.textContent = `${module}: no sources (${e.message})`; return; }
    const pos = locate(this.installed[module], kind, name);
    const f = pos && this.installed[module].find((x) => x.path === pos.file);
    if (!f) { this.state.textContent = `${name}: not found in the sources of ${module}`; return; }
    this.capture();
    this.current = null;
    this.fillFiles();
    this.editor.setAttribute("language", languageOf(f.path));
    this.editor.setAttribute("readonly", "");
    this.editor.value = f.text;
    this.editor.markers = [];
    this.state.textContent = `${module}:${f.path} (read only)`;
    this.editor.reveal(pos.line, pos.column);
  }

  reveal(issue) {
    if (!issue.file) return;
    const f = this.files.find((x) => x.path === issue.file);
    if (!f) return;
    if (f !== this.current) this.edit(f);
    if (issue.line) this.editor.reveal(issue.line, issue.column || 1);
  }

  /** Save all changed files in one order (content operations): all or nothing. */
  async save() {
    this.capture();
    const dirty = this.files.filter((f) => f.text !== f.saved);
    if (!dirty.length) { this.state.textContent = "Nothing to save."; return; }
    this.state.textContent = "Saving…";
    try {
      await this.order(dirty.map((f) => contentOp(f.id, f.path, f.text)));
      for (const f of dirty) f.saved = f.text;
      this.state.textContent = `Saved ${dirty.length} file${dirty.length === 1 ? "" : "s"}.`;
      this.editor.markSaved();
      this.fillFiles();
    } catch (e) { this.state.textContent = `Save failed, nothing saved: ${e.message}`; }
  }

  async newModule() {
    const name = prompt("Name of the module (as in module.ts):");
    if (!name) return;
    try {
      const rooms = await this.json("/v1/rooms");
      const room = rooms[this.a("room") || "root"];
      const r = await this.order([{ op: "create", ref: "m", ...(room ? { room } : {}), types: { [this.a("container-type")]: { [this.a("name-attr")]: name } } }]);
      this.select.append(el("option", { value: r.created.m }, name));
      this.select.value = r.created.m;
      await this.open(r.created.m);
      await this.createFile("module.ts", `import type { ProfileDef } from "@i3cm/lib";\n\nexport const module = { name: "${name}", requires: ["base"] };\nexport const rooms: string[] = ["${name}"];\nexport const profiles: ProfileDef[] = [];\n`);
    } catch (e) { this.state.textContent = e.message; }
  }

  async newFile() {
    if (!this.container) return;
    const dir = { type: "types/", aspect: "aspects/", view: "ui/" }[this.mode];
    const path = prompt("Path of the new file, relative to i3cm/:", this.mode === "view" ? `ui/${this.container.name}.html` : `${dir}new.ts`);
    if (!path) return;
    // the same rule as the server and i3cm studio: relative, no "..", no hidden or empty parts
    if (path.startsWith("/") || path.includes("\\") || path.split("/").some((c) => !c || c === "." || c === ".." || c.startsWith("."))) {
      this.state.textContent = `${path}: not a path inside the module.`; return;
    }
    if (this.files.some((f) => f.path === path)) { this.state.textContent = `${path} exists.`; return; }
    try { await this.createFile(path, ""); } catch (e) { this.state.textContent = e.message; }
  }

  async createFile(path, text) {
    const r = await this.order([
      { op: "create", ref: "f", room: this.container.room, types: { [this.a("file-type")]: { [this.a("path-attr")]: path } } },
      { op: "link", reference: this.a("file-reference"), from: "f", to: this.container.id },
      contentOp("f", path, text)]);
    const id = r.created.f;
    const f = { id, path, text, saved: text };
    this.files.push(f);
    this.files.sort((x, y) => x.path.localeCompare(y.path));
    this.edit(f);
    await this.check();
  }

  async deleteFile() {
    if (!this.current || !confirm(`Delete ${this.current.path}?`)) return;
    try {
      await this.order([{ op: "delete", id: this.current.id }]);
      this.files = this.files.filter((f) => f !== this.current);
      this.current = null;
      this.edit(this.files.find((f) => modeOf(f.path) === this.mode) || null);
      await this.check();
    } catch (e) { this.state.textContent = e.message; }
  }

  /** Let the user pick a target in the diagram and insert `name!: Ref<Target, "end">;`. */
  async setReference() {
    if (!this.current || this.editor.hasAttribute("readonly")) return;
    const name = await this.diagram.pick();
    if (!name) return;
    const local = name.split(":").pop();
    const own = local.charAt(0).toLowerCase() + local.slice(1);
    this.editor.insert(referenceSnippet(local, own, "sources"));
  }
}
customElements.define("base-source-workbench", BaseSourceWorkbench);
