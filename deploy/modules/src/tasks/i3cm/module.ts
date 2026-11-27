import type { ProfileDef } from "@i3cm/lib";

/** Mandatory module: delivered with the base and active in every tenant from its creation. */
export const module = { name: "tasks", requires: ["base"], mandatory: true };
export const rooms: string[] = ["tasks"];
export const profiles: ProfileDef[] = [
  { name: "reader", rights: [
    { sign: "+", art: ["read"], types: ["tasks:Task", "tasks:Deadline"], room: "root" } ] },
  { name: "worker", includes: ["tasks:reader"], rights: [
    { sign: "+", art: ["write", "create", "delete"], types: ["tasks:Task", "tasks:Deadline"], room: "root" },
    { sign: "+", art: ["place", "remove"], types: ["tasks:Task", "tasks:Deadline"], room: "root" } ] },
  { name: "assignee", rights: [
    { sign: "+", art: ["read", "write"], types: ["tasks:Task", "tasks:Deadline"], room: "root", when: "assignees = %me" } ] },
];
