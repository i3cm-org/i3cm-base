// Finding declarations in module source files when the server gives no positions.

/**
 * Position of a catalog element in the files: `{file, line, column}` (1-based) or null.
 * kind: "type" (class), "aspect" (aspect("name", …)), "profile" (name: "…" in module.ts),
 * "room" (string in the rooms list of module.ts). `files` is a list of `{path, text}`.
 * The local part of a qualified name (`ns:Local`) is what the source spells.
 */
export function locate(files, kind, name) {
  const local = name.includes(":") ? name.slice(name.indexOf(":") + 1) : name;
  const esc = local.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const patterns = {
    type: [new RegExp(`\\bclass\\s+${esc}\\b`)],
    aspect: [new RegExp(`\\baspect\\(\\s*["'\`]${esc}["'\`]`)],
    profile: [new RegExp(`\\bname\\s*:\\s*["'\`]${esc}["'\`]`)],
    room: [new RegExp(`\\brooms\\b[^=]*=\\s*\\[[^\\]]*["'\`]${esc}["'\`]`, "s"), new RegExp(`["'\`]${esc}["'\`]`)],
  }[kind] ?? [];
  const order = (f) => (kind === "profile" || kind === "room" ? (f.path.endsWith("module.ts") ? 0 : 1) : 0);
  for (const re of patterns) {
    for (const f of [...files].sort((a, b) => order(a) - order(b))) {
      const m = re.exec(f.text);
      if (!m) continue;
      let index = m.index;
      if (kind === "room") index = f.text.indexOf(local, m.index);
      return { file: f.path, ...lineColumn(f.text, index) };
    }
  }
  return null;
}

/** 1-based line and column of a character offset. */
export function lineColumn(text, offset) {
  const before = text.slice(0, offset);
  const line = before.split("\n").length;
  const column = offset - before.lastIndexOf("\n");
  return { line, column };
}

/** Character offset of a 1-based line and column (clamped to the text). */
export function offsetOf(text, line, column = 1) {
  let offset = 0;
  for (let l = 1; l < line; l++) {
    const next = text.indexOf("\n", offset);
    if (next < 0) return text.length;
    offset = next + 1;
  }
  return Math.min(text.length, offset + Math.max(0, column - 1));
}

/** Last path segment, used as file name of the object's content. */
export function baseName(path) {
  return path.split("/").pop();
}

/** UTF-8 text as base64 (for the `content` operation of orders). */
export function base64Text(text) {
  const bytes = new TextEncoder().encode(text);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

/** Order operation that sets the content of an object (id or ref) to a text file. */
export function contentOp(id, path, text) {
  return { op: "content", id, mediaType: "text/plain", filename: baseName(path), base64: base64Text(text) };
}
