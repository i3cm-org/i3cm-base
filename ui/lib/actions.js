// Parameters of <base-action>: the operation name and the filled parameter object.

/** `module/op` or `module:op` → [module, op]; null when it is not of that form. */
export function splitOp(op) {
  const m = /^([\w-]+)[/:]([\w-]+)$/.exec(op ?? "");
  return m ? [m[1], m[2]] : null;
}

/**
 * Fill `{id}` and `{<key>}` in a parameter template (parsed JSON) with values of a node:
 * `context = { id, attrs: {key: value} }`. A string that is exactly one placeholder takes the
 * value with its type (null when missing); placeholders inside longer strings become text.
 */
export function fillParams(template, context) {
  const val = (k) => (k === "id" ? context.id : context.attrs?.[k]);
  const fill = (v) => {
    if (typeof v === "string") {
      const whole = /^\{([\w.:-]+)\}$/.exec(v);
      if (whole) return val(whole[1]) ?? null;
      return v.replace(/\{([\w.:-]+)\}/g, (_, k) => String(val(k) ?? ""));
    }
    if (Array.isArray(v)) return v.map(fill);
    if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, fill(x)]));
    return v;
  };
  return fill(template);
}
