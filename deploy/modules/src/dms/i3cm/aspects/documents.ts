import { aspect } from "@i3cm/lib";

/** How a document is shown in every view of this module. */
const documentNode = {
  attributes: { title: "title", date: "documentDate", sender: "sender", file: "_content.filename", concerns: "concerns.count()" },
  files: { "{_content.filename}": "_content", "text.txt": "_texts.plain", "ocr.txt": "_texts.ocr" },
  tabs: { concerns: { path: "concerns" } },
};

/** Filing by document date: one folder per year, then per month; undated documents in the empty group. */
export const filing = aspect("filing", {
  entry: { type: "dms:Document", groupBy: ["documentDate.toString().substring(0, 4)", "documentDate.toString().substring(0, 7)"] },
  nodes: { "dms:Document": documentNode },
});

/** Documents by sender; documents without sender in the empty group. */
export const senders = aspect("senders", {
  entry: { type: "dms:Document", groupBy: ["sender"] },
  nodes: { "dms:Document": documentNode },
});

/** Inbox: documents without a document date, still to be filed. */
export const inbox = aspect("inbox", {
  entry: { type: "dms:Document", where: "documentDate.empty()" },
  nodes: { "dms:Document": documentNode },
});

/** Binders by first letter; files copied into a binder's folder become documents concerning it. */
export const binders = aspect("binders", {
  entry: { type: "dms:Binder", groupBy: ["name.substring(0, 1).upper()"] },
  nodes: {
    "dms:Binder": {
      attributes: { name: "name", documentCount: "documents.count()" },
      subNodes: { documents: { path: "documents", folder: true, dropType: "dms:Document" } },
      tabs: { documents: { path: "documents", columns: { title: "title", date: "documentDate", sender: "sender", file: "_content.filename" } } },
    },
    "dms:Document": documentNode,
  },
});
