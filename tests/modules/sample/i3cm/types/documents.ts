// Kernel test module: the former mail and document types of the base, for the kernel's own tests.
import { ns, hasContent, texts, partOf, indexed, Ref, DateTime, Formatted } from "@i3cm/lib";

@ns("sample") @hasContent() @texts("plain:text/plain")
export class Document {
  @indexed title!: string;
}

@ns("sample") @hasContent("message/rfc822") @texts("mail.md:text/markdown", "body.html:text/html")
export class Mail {
  @indexed receivedAt!: DateTime;
  @indexed sender!: Formatted<"email">;
  @indexed subject!: string;
  recipients?: Formatted<"email">[];
  messageId?: string;
}

@ns("sample") @hasContent()
export class MailAttachment {
  filename!: string;
  @partOf mail!: Ref<Mail, "attachments">;
}
