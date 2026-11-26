import { ns, indexed } from "@i3cm/lib";

/** A short note, written by people or by the module itself. */
@ns("example")
export class Note {
  @indexed title!: string;
  author?: string;
}
