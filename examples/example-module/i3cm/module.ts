import type { ProfileDef, OperationDef, HookDef, ScheduleDef, CarrierDef } from "@i3cm/lib";

export const module = { name: "example", requires: ["base"] };
export const rooms: string[] = ["example"];
export const profiles: ProfileDef[] = [
  { name: "writer", rights: [ { sign: "+", art: ["read", "write", "create", "delete"], types: ["example:Note"], room: "root" } ] },
  { name: "roomReader", rights: [ { sign: "+", art: ["read"], types: ["example:Note"], room: "example" } ] },
];
/** The module's own identity: an agent account created on activation. */
export const carriers: CarrierDef[] = [ { name: "system", profiles: ["writer"] } ];
export const operations: OperationDef[] = [
  { name: "greet" },
  { name: "addNote" },
  { name: "stamp", impersonates: ["system"] },
  { name: "stampDenied" },
  { name: "callGreet" },
  { name: "checkNote" },
  { name: "tick" },
  { name: "crash" },
  { name: "spin" },
  { name: "onUpdate" },
  { name: "findOpen" },
  { name: "refs" },
];
export const hooks: HookDef[] = [
  { type: "example:Note", on: "create", phase: "before", op: "checkNote" },
  { type: "example:Note", on: "update", phase: "after", op: "onUpdate" },
];
export const schedules: ScheduleDef[] = [ { op: "tick", rrule: "FREQ=MINUTELY" } ];
