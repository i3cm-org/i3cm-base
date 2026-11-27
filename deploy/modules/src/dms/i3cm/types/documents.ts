import { ns, hasContent, texts, noGrep, indexed, Ref, LocalDate, AnyObject } from "@i3cm/lib";

/**
 * A document: binary content with its media type, versions from the object history, and texts.
 * `plain` is the searchable full text; `ocr` is the text layer of a scan, kept for display and
 * copy but not searched by grep. Media type and file name belong to the content (`_content.mediaType`,
 * `_content.filename`). A document can be attached to any object through `concerns`;
 * every object then shows its documents at the opposite end `documents`.
 */
@ns("dms") @hasContent() @texts("plain:text/plain", "ocr:text/plain") @noGrep("ocr")
export class Document {
  @indexed title!: string;
  @indexed documentDate?: LocalDate;
  @indexed sender?: string;
  concerns!: Ref<AnyObject, "documents">[];
}

/** A binder (file folder) that collects documents; files copied into it become documents. */
@ns("dms")
export class Binder {
  @indexed name!: string;
  description?: string;
}
