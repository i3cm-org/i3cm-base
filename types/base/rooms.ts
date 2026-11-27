import { ns, indexed, Ref } from "../lib";

/** Room: carrier of rights; every object lives in exactly one room. */
@ns("base")
export class Room {
  @indexed name!: string;
  parent?: Ref<Room, "children">;
}
