import { aspect } from "@i3cm/lib";

/** How a document is shown in every view of this module; `md` links to images as `images/<name>/<name>`. */
const documentNode = {
  attributes: { title: "title", date: "documentDate", sender: "sender", file: "_content.filename", concerns: "concerns.count()" },
  files: { "{_content.filename}": "_content", "text.md": "_texts.md", "text.plain.md": "_texts.mdplain" },
  subNodes: { images: { path: "images", folder: true } },
  tabs: { concerns: { path: "concerns" } },
};

/** An image of a document: a folder named like the image, holding the image file. */
const imageNode = {
  attributes: { name: "name", page: "page", caption: "caption" },
  files: { "{name}": "_content" },
};

/** Filing by document date: one folder per year, then per month; undated documents in the empty group. */
export const filing = aspect("filing", {
  entry: { type: "dms:Document", groupBy: ["documentDate.toString().substring(0, 4)", "documentDate.toString().substring(0, 7)"] },
  nodes: { "dms:Document": documentNode, "dms:Image": imageNode },
});

/** Documents by sender; documents without sender in the empty group. */
export const senders = aspect("senders", {
  entry: { type: "dms:Document", groupBy: ["sender"] },
  nodes: { "dms:Document": documentNode, "dms:Image": imageNode },
});

/** Inbox: documents without a document date, still to be filed. */
export const inbox = aspect("inbox", {
  entry: { type: "dms:Document", where: "documentDate.empty()" },
  nodes: { "dms:Document": documentNode, "dms:Image": imageNode },
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
    "dms:Image": imageNode,
  },
});

/** Scan sources by first letter, with their imported documents as sub-nodes and tab. */
export const sources = aspect("sources", {
  entry: { type: "dms:ScanSource", groupBy: ["name.substring(0, 1).upper()"] },
  nodes: {
    "dms:ScanSource": {
      attributes: { name: "name", location: "location", lastImport: "lastImportAt", documentCount: "imports.count()" },
      subNodes: { imports: { path: "imports" } },
      tabs: { imports: { path: "imports", columns: { title: "title", date: "documentDate", sender: "sender", file: "_content.filename" } } },
    },
    "dms:Document": documentNode,
    "dms:Image": imageNode,
  },
});
