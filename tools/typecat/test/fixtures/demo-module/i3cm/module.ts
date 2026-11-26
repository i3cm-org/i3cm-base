import type { RoleDef } from "@i3cm/lib";

export const module = { name: "demo", requires: ["base"] };
export const rooms: string[] = ["demo"];
export const roles: RoleDef[] = [
  { name: "clerk", rights: [ { sign: "+", art: ["write", "create"], types: ["demo:Thing"], room: "demo" } ] },
];
