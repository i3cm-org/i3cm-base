import { ns, Ref } from "@i3cm/lib";
import { Thing } from "@i3cm/demo";

/** A tag on a thing of the demo module. */
@ns("extra")
export class Tag {
  label!: string;
  thing!: Ref<Thing, "tags">;
}
