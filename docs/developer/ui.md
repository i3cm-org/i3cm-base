# Browser UI (test app)

`ui/` is a static artifact: one HTML document with its own code, a public `config.json`, and
vendored libraries under `ui/vendor/` (versions and licences in `ui/vendor/VERSIONS.md`). There
is no build step and no server-side rendering. Deploying is copying the directory.

* **Data only through the public API.** The document holds form and hydration logic; every value
  is fetched at runtime with the caller's token (`/v1/me`, `/v1/aspects`, `children`, `node`,
  `index`, `file`). The client checks nothing; the server decides what a caller may see.
* **Token:** pasted on the sign-in screen and kept in `sessionStorage` of the browser tab.
* **Address:** the state is in the fragment, `#a=<aspect>&n=<node>[&f=<file>]`. The server knows
  one entry point and needs no rewrite rules.
* **Components:** Fluent 2 Web Components (`@fluentui/web-components`), themed with
  `@fluentui/tokens`. `config.json` sets `apiBase` (empty: same origin), `appName` and
  `brandColor`; the brand ramp of the theme is derived from that colour. Light and dark follow
  the system and can be switched.

## Frame and modules

`index.html` is the **frame** of the base: sign-in, theme, tree, detail pane and the web components
modules rely on. Each module delivers **one** file `modules/<namespace>.html` with `<template>`
elements only (no code, no data); the frame loads it for every namespace that has an aspect.
Frame and module files do not overlap.

A module template is chosen by `data-aspect` and `data-type` and shown above the tabs of a node.
The frame fills `data-attr="<key>"` with the values of the aspect attribute `<key>`,
`data-label="<key>"` with its display label and `data-field="<field>"` with a field of the node
(`displayName`, `type`, ...). `modules/base.html` holds the templates of the module `base`
(mailbox, recipients).

## Components of the frame

* `<base-list-view>`: items with icon, display name and a secondary line, paged with
  "Load more", selectable by mouse and keyboard (arrows, Home, End, Enter), fires `open`.
* `<base-grid-view>`: columns, server-side paging (25 rows) and sorting by clicking a column
  header (ascending, descending), fires `open` for a row. It **always** offers "Download as
  XLSX" and "Download as CSV"; the export contains **all** rows and is made by the server
  (`GET /v1/aspects/{aspect}/tab?…&format=xlsx|csv`), so agents get the same through the API.

## Layout

Left, a tree: the built-in views and every aspect at the top, children loaded when a node is
expanded, 50 at a time with "Load more". Right, the selected entry, the module template if there
is one, and tabs:

| Tab | Source |
|---|---|
| Details | attributes from `node` (objects only) |
| one per aspect tab | `tab`: with `columns` a grid view, without a list view; a row opens its object |
| Contents | the children of the node as a list view (also the type lists of `_type`) |
| Index | the Markdown index from `index`; `[[target\|label]]` links open their node (relative paths are walked with `children`, `/_id/<id>` opens the object in the `_id` view) |
| Files | the files of an object, with preview and download |

Files are previewed by type (text, Markdown, sandboxed HTML, images, PDF) or downloaded.

Known limits: the tree has no virtual scrolling (Fluent's tree renders every loaded item; paging
keeps it bounded); opening a node by link or address does not expand the tree to it; columns
cannot be resized; there is no data grid in Fluent Web Components, so `base-grid-view` renders a
plain table styled with the theme tokens.

## Running it

`deploy/compose.ui.yaml` starts database, server and an nginx that serves `ui/` and passes `/v1`
to the server, so UI and API share one origin:

```sh
docker compose -f deploy/compose.ui.yaml -p base-erp-ui up --build -d
docker compose -f deploy/compose.ui.yaml -p base-erp-ui exec server base-server init-tenant demo
python3 deploy/ui/seed-demo.py http://localhost:8090 <token>   # optional demo data
```

Open `http://localhost:8090/` and sign in with the token printed by `init-tenant`.
