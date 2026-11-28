import { ns, hasContent, texts } from "@i3cm/lib";

/** A document with content and a summary text; hooks on content and text write log notes. */
@ns("example") @hasContent() @texts("summary:text/plain", "remark:text/plain")
export class Doc {
  name!: string;
}
