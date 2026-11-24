// Type definition library for base-erp.
// These declarations exist only for the TypeScript compiler; `typecat` reads them
// statically and produces catalog.json. Decorators are never executed.

/* ---------- value types (see docs/developer/types.md) ---------- */
export type Int = number & { readonly __vt?: "int" };
export type Decimal<P extends number, S extends number> = number & { readonly __vt?: ["decimal", P, S] };
export type Text<F extends "TXT" | "MD" | "HTML"> = string & { readonly __vt?: ["text", F] };
export type LocalDate = string & { readonly __vt?: "date" };
export type DateTime = string & { readonly __vt?: "datetime" };
export type Time = string & { readonly __vt?: "time" };
export type Duration = string & { readonly __vt?: "duration" };
export type Enum<V extends string> = V;
export type Money = { amount: string; currency: string };
export type Formatted<K extends "email" | "url" | "iban" | "phone"> = string & { readonly __vt?: ["formatted", K] };
/** Recurrence rule in iCalendar RRULE syntax (RFC 5545). */
export type Recurrence = string & { readonly __vt?: "recurrence" };
export type Struct<F> = F;

/* ---------- references ---------- */
/** Reference to an object carrying type T. `End` names the opposite end. */
export type Ref<T, End extends string> = { readonly __ref?: T; readonly __end?: End };
/** Any object, regardless of its types. */
export abstract class AnyObject {}

/* ---------- class decorators ---------- */
type Ctor = abstract new (...a: any[]) => any;
type ClassDeco = (target: any) => void;
type FieldDeco = (target: any, key: string) => void;
const noopC: ClassDeco = () => {};
const noopF: FieldDeco = () => {};

/** Namespace of the module that owns the type. */
export function ns(_name: string): ClassDeco { return noopC; }
/** The type cannot be attached on its own, only through subtypes. */
export function abstractType(_t: any) {}
/** Precondition: the type may only be attached to objects carrying one of the given types. */
export function requires(..._t: Ctor[]): ClassDeco { return noopC; }
/** Precondition: the type may only be attached to objects carrying all given types. */
export function requiresAll(..._t: Ctor[]): ClassDeco { return noopC; }
/** Implication declared by the new type: every object of the given types also gets this type. */
export function imprintsOn(..._t: Ctor[]): ClassDeco { return noopC; }
/** Implication declared by this type: every object of this type also gets the given types. */
export function brings(..._t: Ctor[]): ClassDeco { return noopC; }
/** Objects of this type may carry binary content. */
export function hasContent(_mediaType?: string): ClassDeco { return noopC; }
/** Text types attachable to objects of this type, as "name:mediaType". */
export function texts(..._defs: string[]): ClassDeco { return noopC; }

/* ---------- field decorators ---------- */
/** Cardinality at the opposite end of a reference: "1", "0..1", "1..n", "0..n". */
export function card(_c: "1" | "0..1" | "1..n" | "0..n"): FieldDeco { return noopF; }
/** Minimum number of entries for a list reference at this end. */
export function min(_n: number): FieldDeco { return noopF; }
/** "Part of": this object is a part of the referenced whole. */
export const partOf: FieldDeco = noopF;
/** References used in rights paths. */
export const rightsRelevant: FieldDeco = noopF;
/** Ordered reference (rank per target). */
export const ordered: FieldDeco = noopF;
/** Index the attribute for listing and range queries. */
export const indexed: FieldDeco = noopF;
/** Attribute value is unique per tenant among current objects. */
export const unique: FieldDeco = noopF;

/* ---------- rooms and roles ---------- */
export type Sign = "+" | "-" | "block";
export type Art = "read" | "write" | "create" | "place" | "remove" | "delete" | "grant";
export interface RightDef { sign: Sign; art: Art[]; types: string[]; room: string }
export interface RoleDef { name: string; guardian?: boolean; includes?: string[]; rights: RightDef[] }
