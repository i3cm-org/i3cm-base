import { ns, card, indexed, Ref, Text, DateTime, Enum, AnyObject } from "../lib";
import { Carrier, Person } from "./parties";

@ns("base")
export class Task {
  @indexed subject!: string;
  description?: Text<"MD" | "HTML">;
  @indexed due?: DateTime;
  @indexed status!: Enum<"open" | "in_progress" | "done">;
  origin?: string;
  @card("0..n") assignees!: Ref<Carrier, "tasks">[];
  @card("0..n") editor?: Ref<Person, "editedTasks">;
  @card("0..n") concerns!: Ref<AnyObject, "concernedBy">[];
}
