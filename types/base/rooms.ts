import { ns, card, rightsRelevant, indexed, Ref } from "../lib";

/** Room: carrier of rights; every object lives in exactly one room. */
@ns("base")
export class Room {
  @indexed name!: string;
  @rightsRelevant @card("0..n") parent?: Ref<Room, "children">;
}
