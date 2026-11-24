import type { RoleDef } from "../lib";

export const module = { name: "base", requires: [] as string[] };
export const rooms: string[] = [];
export const roles: RoleDef[] = [
  { name: "admin", guardian: true, rights: [
    { sign: "+", art: ["read", "write", "create", "place", "remove", "delete", "grant"], types: ["*"], room: "root" } ] },
  { name: "reader", rights: [ { sign: "+", art: ["read"], types: ["*"], room: "root" } ] },
  { name: "person-clerk", rights: [
    { sign: "+", art: ["write", "create", "delete"], types: ["base:Person", "base:Company"], room: "root" },
    { sign: "+", art: ["place", "remove"], types: ["*"], room: "root" } ] },
  { name: "mail-clerk", rights: [
    { sign: "+", art: ["write", "create", "delete"], types: ["base:Mail", "base:MailAttachment", "base:Document"], room: "root" } ] },
];
