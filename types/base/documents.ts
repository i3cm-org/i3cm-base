import { ns, hasContent, texts, partOf, indexed, Ref, DateTime, Formatted } from "../lib";

@ns("base") @hasContent() @texts("plain:text/plain")
export class Document {
  @indexed title!: string;
}

@ns("base") @hasContent("message/rfc822") @texts("mail.md:text/markdown", "body.html:text/html")
export class Mail {
  @indexed receivedAt!: DateTime;
  @indexed sender!: Formatted<"email">;
  @indexed subject!: string;
  recipients?: Formatted<"email">[];
  messageId?: string;
}

@ns("base") @hasContent()
export class MailAttachment {
  filename!: string;
  @partOf mail!: Ref<Mail, "attachments">;
}
