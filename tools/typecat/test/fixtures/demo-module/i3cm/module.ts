import type { ProfileDef } from "@i3cm/lib";

export const module = { name: "demo", requires: ["base"] };
export const rooms: string[] = ["demo"];
export const profiles: ProfileDef[] = [
  { name: "clerk", rights: [ { sign: "+", art: ["write", "create"], types: ["demo:Thing"], room: "demo" } ] },
];
