# Browser UI (test app)

`ui/` is a static artifact: one HTML document with its own code, the frame components in
`ui/components/` and their logic in `ui/lib/` (plain ES modules), a public `config.json`, and
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

## Tool pages

Besides templates for nodes, a module file may hold **one page template**
`<template data-page="<ns>" data-title="…">`. The frame shows a button with the title in the
header and, when it is pressed (address `#p=<ns>`), the page instead of tree and detail pane.
The page consists of frame components only (the frame drops `<script>`, `<iframe>`, `<object>`,
`<embed>`, event attributes and `javascript:` links from module templates); the frame hands every element that has an `api`
property its authenticated `fetch`, so all data still comes from the public API. A page is
offered when its module has at least one aspect in the tenant (inactive modules have none).

```html
<template data-page="studio" data-title="Studio">
  <base-source-workbench container-type="studio:ModuleSource" name-attr="name"
    file-type="studio:SourceFile" path-attr="path" file-end="files"
    file-reference="studio:SourceFile.module" room="studio" check="/v1/catalog/check">
  </base-source-workbench>
</template>
```

### Components for tool pages

* `<base-code-editor>`: plain text editor with highlighting (`language`: `typescript`, `html`,
  `plain`), line numbers, markers per line (`markers = [{line, severity, message}]`), `readonly`,
  and a `mode` word that the stylesheet maps to a background (`type`, `aspect`, `view`). Methods
  `reveal(line, column)` and `insert(text)`; events `change` (debounced) and `save` (Ctrl+S). It
  is a textarea under a highlighted copy, so editing, undo and input methods are the browser's.
* `<base-diagram>`: simplified UML class diagram of catalog types (`catalog = …`): boxes with
  attributes, references with both end names and cardinalities, generalisation, part-of, and
  dashed prerequisites. The context menu shows referenced or referencing types, hides a type or
  opens it (`open-type` event); `pick()` waits for the user to click a type. Boxes can be moved;
  with `storage-key`, shown types and positions are kept in `localStorage`.
* `<base-issue-list>`: problems of a check sorted by severity, file and line, with a summary;
  fires `reveal` with the chosen issue.
* `<base-source-workbench>`: the three components put together for source files stored as
  objects with content: a container type per module, a file type with a path attribute. Left the
  packages of the catalog (types, aspects, profiles, rooms; the edited module writable, the
  others read only), in the middle the editor with the modes type (`types/`, `module.ts`),
  aspect (`aspects/`) and view (`ui/`), right the issues, above the diagram. Saving writes the
  content of each changed file (`PUT /v1/objects/{id}/content`); new and deleted files are
  orders. The unsaved state of all files is sent to `check` (`POST /v1/catalog/check`, see
  api.md) after each change; its answer `{catalog, issues, positions}` feeds packages, diagram,
  issues and jumps to declarations (while there are errors the last good catalog stays).
  Without `check` (or if the server lacks it) the installed catalog (`GET /v1/catalog`) is shown,
  and declarations are found in the sources by name. Elements of other modules open read only
  from their sources (`GET /v1/modules/{name}/sources`).

The logic without DOM (catalog index, diagram model, layout, highlighting, issue conversion,
finding declarations) lives in `ui/lib/` and has unit tests: `node --test ui/test/*.test.mjs` (Node's test runner,
no dependencies).

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
docker compose -f deploy/compose.ui.yaml -p i3cm-ui up --build -d
docker compose -f deploy/compose.ui.yaml -p i3cm-ui exec server i3cm-server init-tenant demo
python3 deploy/ui/seed-demo.py http://localhost:8090 <token>   # optional demo data
```

Open `http://localhost:8090/` and sign in with the token printed by `init-tenant` (valid one day;
`i3cm-server create-token demo admin` issues a longer one).
