// Issues for the right column, from the check API of the kernel or from typecat's error output.

const SEVERITY_RANK = { error: 0, warning: 1, info: 2 };

/**
 * A file name of the check API: `<module>:<path>` (kernel types: `kernel:<path>`), the path with
 * or without a leading `i3cm/`. Returns the module and the path relative to `i3cm/`.
 */
export function splitFile(file) {
  if (!file) return { module: null, path: null };
  const m = /^([A-Za-z0-9_-]+):(.*)$/.exec(file);
  const module = m ? m[1] : null;
  const path = (m ? m[2] : file).replace(/^i3cm\//, "");
  return { module, path };
}

/**
 * Normalises one issue of the check API (`{severity, source, error_id, description, params, file,
 * line, column}`). Files of `ownModule` become paths relative to `i3cm/`; files of other modules
 * keep their `<module>:` prefix, so they are shown but not opened.
 */
export function fromCheckApi(issue, ownModule = null) {
  const { module, path } = splitFile(issue.file ?? null);
  return {
    severity: issue.severity ?? "error",
    source: issue.source ?? "checker",
    errorId: issue.error_id ?? "",
    description: issue.description ?? "",
    params: issue.params ?? {},
    file: path === null ? null : (module === null || module === ownModule ? path : `${module}:${path}`),
    line: issue.line ?? null,
    column: issue.column ?? null,
  };
}

/** Position of a declaration from the check API's `positions` (keys `type:<name>`, `aspect:<name>`). */
export function positionOf(positions, kind, name, ownModule = null) {
  const p = positions?.[`${kind}:${name}`];
  if (!p) return null;
  const { module, path } = splitFile(p.file);
  if (module !== null && module !== ownModule) return null;
  return { file: path, line: p.line ?? 1, column: p.column ?? 1 };
}

/**
 * Issues from typecat's error object on stderr. TypeScript errors arrive as
 * `"<file>:<line> <message>"` inside `params.errors` of `typecat.compile_error`; every other error is
 * one issue of source `typecat`. `moduleRoot` is the path of `i3cm/` to make file names relative.
 */
export function fromTypecat(error, moduleRoot = "") {
  if (!error) return [];
  if (error.error_id === "typecat.compile_error" && Array.isArray(error.params?.errors)) {
    return error.params.errors.map((text) => {
      const m = /^(.*?):(\d+) (.*)$/s.exec(text);
      return {
        severity: "error",
        source: "typescript",
        errorId: "typescript",
        description: m ? m[3] : text,
        params: {},
        file: m ? relativeFile(m[1], moduleRoot) : null,
        line: m ? Number(m[2]) : null,
        column: null,
      };
    });
  }
  return [{
    severity: "error",
    source: "typecat",
    errorId: error.error_id ?? "",
    description: error.description ?? "",
    params: error.params ?? {},
    file: relativeFile(error.params?.file ?? null, moduleRoot),
    line: error.params?.line ?? null,
    column: null,
  }];
}

/** Path relative to the module root (`i3cm/`), with forward slashes. */
export function relativeFile(file, moduleRoot) {
  if (!file) return null;
  const f = file.replaceAll("\\", "/");
  let root = moduleRoot.replaceAll("\\", "/");
  if (root && !root.endsWith("/")) root += "/";
  if (root && f.startsWith(root)) return f.slice(root.length);
  const i = f.lastIndexOf("/i3cm/");
  return i >= 0 ? f.slice(i + "/i3cm/".length) : f;
}

/** Sorted by severity, file, line; issues without a file last within their severity. */
export function sortIssues(list) {
  return list.slice().sort((a, b) =>
    (SEVERITY_RANK[a.severity] ?? 9) - (SEVERITY_RANK[b.severity] ?? 9)
    || (a.file === null) - (b.file === null)
    || (a.file ?? "").localeCompare(b.file ?? "")
    || (a.line ?? 0) - (b.line ?? 0));
}

/** Counts per severity for the header ("green" when there are no errors). */
export function summary(list) {
  const counts = { error: 0, warning: 0, info: 0 };
  for (const i of list) counts[i.severity] = (counts[i.severity] ?? 0) + 1;
  return { ...counts, green: counts.error === 0 };
}

/** Editor mode of a file: type, aspect or view (the three backgrounds of the editor). */
export function modeOf(path) {
  if (!path) return "type";
  if (path.startsWith("aspects/")) return "aspect";
  if (path.startsWith("ui/")) return "view";
  return "type";
}
