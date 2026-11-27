import { ns } from "@i3cm/lib";

/** A draft that only exists in development tenants. */
@ns("devonly")
export class Draft {
  title!: string;
}
