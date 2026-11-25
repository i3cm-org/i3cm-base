import { aspect } from "../lib";

/** Mail filing: one folder per day, then "sender - subject", then the files of the mail. */
export const mailbox = aspect("mailbox", {
  entry: { type: "base:Mail", groupBy: ["receivedAt.toString().substring(0, 10)"] },
  nodes: {
    "base:Mail": {
      attributes: { sender: "sender", subject: "subject", received: "receivedAt", attachmentCount: "attachments.count()" },
      subNodes: { attachments: { path: "attachments", folder: true } },
      files: { "mail.eml": "_content", "mail.md": "_texts.`mail.md`", "body.html": "_texts.`body.html`" },
      tabs: { attachments: { path: "attachments", columns: { file: "filename" } } },
      functions: { reply: { url: "/modules/mail/reply" } },
    },
    "base:MailAttachment": {
      attributes: { file: "filename" },
      files: { "{filename}": "_content" },
    },
  },
});

/** Mails by recipient: a mail with several recipients appears under each of them (the mail exists once). */
export const recipients = aspect("recipients", {
  entry: { type: "base:Mail", groupBy: ["recipients"] },
  nodes: {
    "base:Mail": {
      attributes: { sender: "sender", subject: "subject", received: "receivedAt" },
      files: { "mail.eml": "_content", "mail.md": "_texts.`mail.md`" },
    },
  },
});
