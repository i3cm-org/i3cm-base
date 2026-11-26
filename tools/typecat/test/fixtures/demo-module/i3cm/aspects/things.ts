import { aspect } from "@i3cm/lib";

export const things = aspect("things", {
  entry: { type: "demo:Thing", groupBy: ["label.substring(0, 1).upper()"] },
  nodes: { "demo:Thing": { attributes: { label: "label", owner: "owner" } } },
});
