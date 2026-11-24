import { ns, hasContent, texts, card, partOf, indexed, Ref, DateTime, Formatted } from "../lib";

@ns("base") @hasContent() @texts("plain:text/plain")
export class Document {
  @indexed title!: string;
}

@ns("base") @hasContent("message/rfc822") @texts("mail.md:text/markdown", "body.html:text/html")
export class Mail {
  @indexed receivedAt!: DateTime;
  @indexed sender!: Formatted<"email">;
  @indexed subject!: string;
  messageId?: string;
}

@ns("base") @hasContent()
export class MailAttachment {
  filename!: string;
  @partOf @card("0..n") mail!: Ref<Mail, "attachments">;
}
