// Syntax highlighting for <base-code-editor>: text in, escaped HTML with <span class="tk-…"> out.

const TS_KEYWORDS = new Set(("abstract as async await boolean break case catch class const continue declare default delete do else enum " +
  "export extends false finally for from function if implements import in instanceof interface let new null number private " +
  "protected public readonly return static string super switch this throw true try type typeof undefined var void while yield").split(" "));

export function escapeHtml(s) {
  return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
}

const span = (cls, text) => `<span class="tk-${cls}">${escapeHtml(text)}</span>`;

/** Highlight TypeScript: comments, strings, numbers, keywords, decorators and type names. */
export function highlightTs(src) {
  const re = /(\/\/[^\n]*|\/\*[\s\S]*?(?:\*\/|$))|("(?:\\.|[^"\\\n])*"?|'(?:\\.|[^'\\\n])*'?|`(?:\\.|[^`\\])*`?)|(@[A-Za-z_$][\w$]*)|(\b\d[\d_]*(?:\.\d+)?\b)|([A-Za-z_$][\w$]*)/g;
  let out = "", last = 0, m;
  while ((m = re.exec(src))) {
    out += escapeHtml(src.slice(last, m.index));
    const [t, comment, string, deco, num, ident] = m;
    if (comment) out += span("comment", t);
    else if (string) out += span("string", t);
    else if (deco) out += span("decorator", t);
    else if (num) out += span("number", t);
    else if (ident && TS_KEYWORDS.has(ident)) out += span("keyword", t);
    else if (ident && /^[A-Z]/.test(ident)) out += span("type", t);
    else out += escapeHtml(t);
    last = m.index + t.length;
  }
  return out + escapeHtml(src.slice(last));
}

/** Highlight HTML templates: comments, tags, attribute names and values. */
export function highlightHtml(src) {
  const re = /(<!--[\s\S]*?(?:-->|$))|(<\/?[A-Za-z][\w-]*)|([\w-:]+)(=)("[^"]*"?|'[^']*'?)|(\/?>)/g;
  let out = "", last = 0, m;
  while ((m = re.exec(src))) {
    const t = m[0];
    // attribute pairs only count inside a tag; outside they are plain text
    out += escapeHtml(src.slice(last, m.index));
    if (m[1]) out += span("comment", t);
    else if (m[2]) out += span("keyword", t);
    else if (m[3]) out += span("type", m[3]) + escapeHtml(m[4]) + span("string", m[5]);
    else out += span("keyword", t);
    last = m.index + t.length;
  }
  return out + escapeHtml(src.slice(last));
}

/** Language from a file name: `.ts` TypeScript, `.html` HTML, everything else plain. */
export function languageOf(path) {
  if (/\.(ts|mts|js|mjs)$/.test(path || "")) return "typescript";
  if (/\.html?$/.test(path || "")) return "html";
  return "plain";
}

export function highlight(src, language) {
  if (language === "typescript") return highlightTs(src);
  if (language === "html") return highlightHtml(src);
  return escapeHtml(src);
}
