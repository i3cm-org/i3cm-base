import { ns, indexed, Ref, Text, DateTime, Enum, AnyObject } from "../lib";
import { Carrier, Person } from "./parties";

@ns("base")
export class Task {
  @indexed subject!: string;
  description?: Text<"MD" | "HTML">;
  @indexed due?: DateTime;
  @indexed status!: Enum<"open" | "in_progress" | "done">;
  origin?: string;
  assignees!: Ref<Carrier, "tasks">[];
  editor?: Ref<Person, "editedTasks">;
  concerns!: Ref<AnyObject, "concernedBy">[];
}
