import { ns, requires, indexed, partOf, ordered, rightsRelevant, Ref, Text, DateTime, Enum, Recurrence, AnyObject } from "@i3cm/lib";
import { Carrier, Person } from "@i3cm/base";

/**
 * A task. To-dos and tasks are one type. Projects, checklists and series are structure, not
 * types of their own: a project is a task with subtasks, a checklist is an ordered list of
 * subtasks, a series is a task with a recurrence that later produces its occurrences.
 *
 * Dates: `due` is the due date of every task. `deferredTo` puts a task aside until that time
 * (resubmission), `followUpAt` is when to follow up on a task that waits for someone else,
 * `plannedAt` is an appointment for doing it. "Overdue" is not a status; it is derived from
 * `due` and the current time.
 */
@ns("tasks")
export class Task {
  @indexed subject!: string;
  description?: Text<"MD" | "HTML">;
  @indexed status!: Enum<"open" | "in_progress" | "waiting" | "done">;
  @indexed due?: DateTime;
  @indexed deferredTo?: DateTime;
  @indexed followUpAt?: DateTime;
  @indexed plannedAt?: DateTime;
  doneAt?: DateTime;
  /** Whom or what the task waits for while its status is `waiting`. */
  waitingFor?: string;
  /** Where the task came from, e.g. a mail, a meeting or a module. */
  origin?: string;
  /** Rule for a series (RFC 5545); the generator creates the occurrences. */
  recurrence?: Recurrence;
  @rightsRelevant assignees!: Ref<Carrier, "assignedTasks">[];
  editor?: Ref<Person, "taskEdits">;
  @partOf @ordered parent?: Ref<Task, "subtasks">;
  series?: Ref<Task, "occurrences">;
  concerns!: Ref<AnyObject, "relatedTasks">[];
}

/**
 * A weighted deadline: a facet that a task carries in addition to `tasks:Task`. The date is the
 * task's `due`; the deadline adds whether it runs against us or for us, a reminder date ahead
 * of it and what happens when it passes.
 */
@ns("tasks") @requires(Task)
export class Deadline {
  @indexed direction!: Enum<"against_us" | "for_us">;
  /** Advance reminder date (pre-deadline). */
  @indexed reminderAt?: DateTime;
  /** What happens when the deadline passes, e.g. loss of a right. */
  effect?: Text<"MD">;
  /** Legal or contractual basis of the deadline. */
  basis?: string;
}
