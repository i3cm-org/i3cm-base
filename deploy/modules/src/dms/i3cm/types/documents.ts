import { ns, hasContent, texts, noGrep, indexed, partOf, Ref, LocalDate, DateTime, Int, AnyObject } from "@i3cm/lib";
import { Carrier, Room } from "@i3cm/base";

/**
 * A document: binary content with its media type and file name (`_content.mediaType`,
 * `_content.filename`), versions from the object history, and four texts:
 * - `txt`: the plain-text part of a mail, verbatim (empty for scans and files);
 * - `html`: the HTML part of a mail, verbatim (empty for scans and files);
 * - `md`: the best Markdown of the content; images only as links to the parts in `images`, never
 *   embedded;
 * - `mdplain`: `md` without images, cut from `md` in the same write. Only this text is searched.
 * A document can be attached to any object through `concerns`; every object then shows its
 * documents at the opposite end `documents`. `contentHash` is the duplicate key with its method as
 * prefix (`sha256:<hex>` for files); `duplicateOf` points to the original with the same content.
 */
@ns("dms") @hasContent()
@texts("txt:text/plain", "html:text/html", "md:text/markdown", "mdplain:text/markdown")
@noGrep("txt", "html", "md")
export class Document {
  @indexed title!: string;
  @indexed documentDate?: LocalDate;
  @indexed sender?: string;
  concerns!: Ref<AnyObject, "documents">[];
  @indexed contentHash?: string;
  source?: Ref<ScanSource, "imports", "0..n">;
  duplicateOf?: Ref<Document, "duplicates", "0..n">;
}

/** An image taken from a document; part of it and deleted with it. `md` links to it by `name`. */
@ns("dms") @hasContent()
export class Image {
  @partOf document!: Ref<Document, "images">;
  @indexed name!: string;
  page?: Int;
  caption?: string;
}

/**
 * A scan source (a scanner or a watched folder) with its own room; documents imported from it
 * live there. `readers` are the accounts and groups allowed to read its documents. `location`
 * is shown only; it never holds credentials.
 */
@ns("dms")
export class ScanSource {
  @indexed name!: string;
  location?: string;
  description?: string;
  lastImportAt?: DateTime;
  room!: Ref<Room, "scanSource", "0..1">;
  readers!: Ref<Carrier, "readableScanSources">[];
}

/** A binder (file folder) that collects documents; files copied into it become documents. */
@ns("dms")
export class Binder {
  @indexed name!: string;
  description?: string;
}
