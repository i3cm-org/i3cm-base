import { el } from "../lib/dom.js";
import { sortIssues, summary } from "../lib/issues.js";

/**
 * <base-issue-list>: problems reported by a check, sorted by severity, file and line.
 * Property `issues`: [{severity, source, errorId, description, params, file, line, column}]
 * (see lib/issues.js for the conversion from API answers). Property `note`: a line shown
 * instead of the list (e.g. "checking…" or why no check is possible).
 * Event `reveal` with the issue when one is chosen (click or Enter).
 */
export class BaseIssueList extends HTMLElement {
  #issues = [];
  #note = "";
  set issues(list) { this.#issues = sortIssues(list || []); this.#note = ""; this.render(); }
  get issues() { return this.#issues; }
  set note(text) { this.#note = text || ""; this.render(); }
  connectedCallback() { this.render(); }

  render() {
    const s = summary(this.#issues);
    const head = el("div", { class: "il-head" },
      el("span", { class: s.green ? "il-green" : "il-red" }, s.green ? "No errors" : `${s.error} error${s.error === 1 ? "" : "s"}`),
      s.warning ? el("span", { class: "il-count" }, `${s.warning} warning${s.warning === 1 ? "" : "s"}`) : null);
    if (this.#note) return this.replaceChildren(head, el("p", { class: "empty" }, this.#note));
    const ul = el("ul", { class: "il", role: "listbox", "aria-label": this.getAttribute("label") || "Issues" });
    for (const i of this.#issues) {
      const where = i.file ? `${i.file}${i.line ? `:${i.line}` : ""}` : "";
      const li = el("li", { role: "option", tabindex: "0", class: `il-${i.severity}` },
        el("span", { class: "il-sev", "aria-label": i.severity }, i.severity === "error" ? "✕" : i.severity === "warning" ? "!" : "i"),
        el("span", { class: "il-text" }, i.description || i.errorId),
        el("span", { class: "il-meta" }, [i.source, i.errorId !== i.source ? i.errorId : null, where].filter(Boolean).join(" · ")));
      const open = () => this.dispatchEvent(new CustomEvent("reveal", { detail: i }));
      li.addEventListener("click", open);
      li.addEventListener("keydown", (ev) => { if (ev.key === "Enter") open(); });
      ul.append(li);
    }
    this.replaceChildren(head, this.#issues.length ? ul : el("p", { class: "empty" }, "Nothing to report."));
  }
}
customElements.define("base-issue-list", BaseIssueList);
