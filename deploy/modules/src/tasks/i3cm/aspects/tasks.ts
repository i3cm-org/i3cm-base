import { aspect } from "@i3cm/lib";

const taskNode = {
  attributes: { subject: "subject", status: "status", due: "due", deferredTo: "deferredTo",
    followUpAt: "followUpAt", plannedAt: "plannedAt", waitingFor: "waitingFor", subtaskCount: "subtasks.count()" },
  subNodes: { subtasks: { path: "subtasks", folder: true, dropType: "tasks:Task" } },
  tabs: {
    subtasks: { path: "subtasks", columns: { subject: "subject", status: "status", due: "due" } },
    assignees: { path: "assignees" },
    concerns: { path: "concerns" },
  },
};

/** My tasks: open tasks assigned to me or one of my groups, by status. */
export const mine = aspect("mine", {
  entry: { type: "tasks:Task", groupBy: ["status"], where: "assignees = %me and status != 'done'" },
  nodes: { "tasks:Task": taskNode },
});

/** Due and overdue: open tasks with a due date, by due day. */
export const due = aspect("due", {
  entry: { type: "tasks:Task", groupBy: ["due.toString().substring(0, 10)"], where: "due.exists() and status != 'done'" },
  nodes: { "tasks:Task": taskNode },
});

/** Upcoming: open tasks due within the next seven days. */
export const upcoming = aspect("upcoming", {
  entry: { type: "tasks:Task", groupBy: ["due.toString().substring(0, 10)"], where: "due >= now() and due <= today() + 7 days and status != 'done'" },
  nodes: { "tasks:Task": taskNode },
});

/** Overdue: open tasks whose due date has passed. */
export const overdue = aspect("overdue", {
  entry: { type: "tasks:Task", groupBy: ["due.toString().substring(0, 10)"], where: "due < now() and status != 'done'" },
  nodes: { "tasks:Task": taskNode },
});

const deadlineNode = {
  attributes: { subject: "subject", due: "due", direction: "direction", reminderAt: "reminderAt",
    status: "status", basis: "basis" },
  tabs: { assignees: { path: "assignees" }, concerns: { path: "concerns" } },
};

/** Deadline book: open weighted deadlines, by month of the task's due date. */
export const deadlines = aspect("deadlines", {
  entry: { type: "tasks:Deadline", groupBy: ["due.toString().substring(0, 7)"], where: "status != 'done'" },
  nodes: { "tasks:Deadline": deadlineNode },
});

/** Deadline reminders: open weighted deadlines whose advance reminder date has come. */
export const deadlineReminders = aspect("deadlineReminders", {
  entry: { type: "tasks:Deadline", groupBy: ["reminderAt.toString().substring(0, 10)"], where: "reminderAt <= now() and status != 'done'" },
  nodes: { "tasks:Deadline": deadlineNode },
});

/** Resubmissions: open tasks put aside until a date, by that day. */
export const resubmissions = aspect("resubmissions", {
  entry: { type: "tasks:Task", groupBy: ["deferredTo.toString().substring(0, 10)"], where: "deferredTo.exists() and status != 'done'" },
  nodes: { "tasks:Task": taskNode },
});

/** Follow-ups: waiting tasks with a follow-up date, by that day. */
export const followUps = aspect("followUps", {
  entry: { type: "tasks:Task", groupBy: ["followUpAt.toString().substring(0, 10)"], where: "followUpAt.exists() and status = 'waiting'" },
  nodes: { "tasks:Task": taskNode },
});

/** Projects: top-level tasks with subtasks, by status. */
export const projects = aspect("projects", {
  entry: { type: "tasks:Task", groupBy: ["status"], where: "parent.empty() and subtasks.exists()" },
  nodes: { "tasks:Task": taskNode },
});
