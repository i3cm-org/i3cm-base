import { ns, indexed, Ref, Enum, DateTime } from "@i3cm/lib";

/** A short note, written by people or by the module itself. */
@ns("example")
export class Note {
  @indexed title!: string;
  author?: string;
  @indexed status?: Enum<"open" | "done">;
  /** Set by the after-update hook when the status becomes `done`. */
  doneAt?: DateTime;
  @indexed due?: DateTime;
  /** Notes can hang below another note. */
  parent?: Ref<Note, "children">;
}
