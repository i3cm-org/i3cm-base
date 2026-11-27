import type { ProfileDef } from "@i3cm/lib";

export const module = { name: "dms", requires: ["base"], mandatory: true };
export const rooms: string[] = ["dms"];
export const profiles: ProfileDef[] = [
  { name: "reader", rights: [
    { sign: "+", art: ["read"], types: ["dms:Document", "dms:Binder"], room: "root" } ] },
  { name: "clerk", includes: ["dms:reader"], rights: [
    { sign: "+", art: ["write", "create", "delete"], types: ["dms:Document", "dms:Binder"], room: "root" },
    { sign: "+", art: ["place", "remove"], types: ["dms:Document", "dms:Binder"], room: "root" } ] },
];
