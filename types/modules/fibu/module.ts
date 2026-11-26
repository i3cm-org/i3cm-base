import type { ProfileDef } from "../../lib";

export const module = { name: "fibu", requires: ["base"] };
export const rooms: string[] = ["fibu"];
export const profiles: ProfileDef[] = [
  { name: "accountant", rights: [
    { sign: "+", art: ["read"], types: ["fibu:LedgerAccount"], room: "fibu" },
    { sign: "+", art: ["write", "create"], types: ["fibu:JournalEntry", "fibu:JournalLine"], room: "fibu" },
    { sign: "+", art: ["read"], types: ["base:Person"], room: "root" } ] },
  { name: "chart-admin", includes: ["fibu:accountant"], rights: [
    { sign: "+", art: ["write", "create", "delete"], types: ["fibu:LedgerAccount"], room: "fibu" },
    { sign: "+", art: ["place", "remove"], types: ["*"], room: "fibu" } ] },
];
