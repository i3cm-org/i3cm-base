import { ns, abstractType, requires, indexed, unique, Ref, Formatted, LocalDate } from "../lib";

@ns("base")
export class Person {
  @indexed lastName!: string;
  firstName?: string;
  birthDate?: LocalDate;
}

@ns("base")
export class Company {
  @indexed name!: string;
}

/** Rights carrier: something that can hold profiles. */
@ns("base") @abstractType
export abstract class Carrier {}

@ns("base")
export class Group extends Carrier {
  @unique @indexed name!: string;
}

@ns("base") @abstractType
export abstract class Account extends Carrier {
  @unique @indexed login!: string;
  email?: Formatted<"email">;
  /** A deactivated account keeps its data, but its tokens no longer work. */
  deactivated?: boolean;
}

/** Login of a person; attached to the person object itself. */
@ns("base") @requires(Person)
export class PersonAccount extends Account {}

/** Login of a machine or agent (module worker, drone). */
@ns("base")
export class AgentAccount extends Account {
  purpose?: string;
}

/** Membership of a carrier in a group (technical, distinct from business memberships). */
@ns("base")
export class GroupMembership {
  group!: Ref<Group, "memberships">;
  member!: Ref<Carrier, "groupMemberships">;
}
