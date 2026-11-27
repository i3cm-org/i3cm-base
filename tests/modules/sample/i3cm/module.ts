import type { ProfileDef } from "@i3cm/lib";

/** Kernel test module (not shipped): mail, documents and tasks as the kernel's tests need them. */
export const module = { name: "sample", requires: ["base"], mandatory: true };
export const rooms: string[] = [];
export const profiles: ProfileDef[] = [
  { name: "mail-clerk", rights: [
    { sign: "+", art: ["write", "create", "delete"], types: ["sample:Mail", "sample:MailAttachment", "sample:Document"], room: "root" } ] },
];
