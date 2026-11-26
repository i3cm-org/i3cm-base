import { ns, indexed, Ref, LocalDate } from "@i3cm/lib";
import { Person } from "@i3cm/base";

/** A thing someone owns. */
@ns("demo")
export class Thing {
  @indexed label!: string;
  since?: LocalDate;
  owner?: Ref<Person, "demoThings">;
}
