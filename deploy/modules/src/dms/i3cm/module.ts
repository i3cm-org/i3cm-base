import type { ProfileDef } from "@i3cm/lib";

export const module = { name: "dms", requires: ["base"], mandatory: true };
export const rooms: string[] = ["dms"];
export const profiles: ProfileDef[] = [
  { name: "reader", rights: [
    { sign: "+", art: ["read"], types: ["dms:Document", "dms:Image", "dms:ScanSource", "dms:Binder"], room: "root" } ] },
  { name: "clerk", includes: ["dms:reader"], rights: [
    { sign: "+", art: ["write", "create", "delete"], types: ["dms:Document", "dms:Image", "dms:Binder"], room: "root" },
    { sign: "+", art: ["place", "remove"], types: ["dms:Document", "dms:Image", "dms:Binder"], room: "root" } ] },
  /** Reads the documents of the scan sources that name the account or one of its groups in `readers`. */
  { name: "sourceReader", rights: [
    { sign: "+", art: ["read"], types: ["dms:Document"], room: "dms", when: "source.readers = %me" },
    { sign: "+", art: ["read"], types: ["dms:Image"], room: "dms", when: "document.source.readers = %me" },
    { sign: "+", art: ["read"], types: ["dms:ScanSource"], room: "dms", when: "readers = %me" } ] },
  /** For scan importers: may create documents in the scan sources, reads nothing. */
  { name: "importer", rights: [
    { sign: "+", art: ["create"], types: ["dms:Document"], room: "dms" } ] },
  /** Manages scan sources and names their readers. */
  { name: "sourceAdmin", rights: [
    { sign: "+", art: ["read", "write", "create", "delete"], types: ["dms:ScanSource"], room: "dms" },
    { sign: "+", art: ["read"], types: ["base:Carrier"], room: "root" } ] },
];
