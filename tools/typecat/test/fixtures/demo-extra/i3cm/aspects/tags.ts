import { aspect } from "@i3cm/lib";

const tagNode = { attributes: { label: "label" }, tabs: { thing: { path: "thing" } } };

export const byLabel = aspect("byLabel", { entry: { type: "extra:Tag", groupBy: ["label"] }, nodes: { "extra:Tag": tagNode } });
export const flat = aspect("flat", { entry: { type: "extra:Tag" }, nodes: { "extra:Tag": tagNode } });
