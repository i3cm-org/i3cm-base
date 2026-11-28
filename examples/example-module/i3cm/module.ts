import type { ProfileDef, OperationDef, HookDef, ScheduleDef, CarrierDef, FilterDef } from "@i3cm/lib";

export const module = { name: "example", requires: ["base"] };
export const rooms: string[] = ["example"];
export const profiles: ProfileDef[] = [
  { name: "writer", rights: [ { sign: "+", art: ["read", "write", "create", "delete"], types: ["example:Note", "example:Doc"], room: "root" } ] },
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
  { name: "onEnter" },
  { name: "onLeave" },
  { name: "onContent" },
  { name: "onSummary" },
];
/** State filter for hooks: notes that are done (no %me, no time, no paths). */
export const filters: Record<string, FilterDef> = {
  done: { type: "example:Note", where: "status = 'done'" },
};
export const hooks: HookDef[] = [
  { type: "example:Note", on: "create", phase: "before", op: "checkNote" },
  { type: "example:Note", on: "update", phase: "after", op: "onUpdate" },
  { filter: "@done", on: "enter", op: "onEnter" },
  { filter: "@done", on: "leave", op: "onLeave" },
  { type: "example:Doc", on: "content", op: "onContent" },
  { type: "example:Doc", on: "text", textType: "example:summary", op: "onSummary" },
];
export const schedules: ScheduleDef[] = [ { op: "tick", rrule: "FREQ=MINUTELY" } ];
