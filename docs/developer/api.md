# REST API (v1)

All requests send `Authorization: Bearer <token>` (401 with `auth.invalid_token`,
`auth.expired_token`, `auth.revoked_token`, `auth.unused_token` or `auth.account_inactive`
otherwise), except the
self-description: `GET /` (entry page), `/guide/agents` (guide for agents), `/openapi.json`,
`/skill.zip` (the guide as a skill) and `/v1/version` (version and source revision). Errors are
`{"error_id": "...", "description": "...", "params": {...}, "hint": "..."}` with status 400 (invalid),
401 (no or unknown token), 404 (not found or not allowed: both look the same), 409 (conflict,
integrity), 500. `hint`, where present, says what to do next.

## Limits

Fixed values of the kernel, the reason for them and the error a caller gets. The operator can change
the ones marked "operator" ([configuration](../operator/configuration.md)).

| Limit | Value | Reason | Error |
|---|---|---|---|
| request body (JSON and direct content) | 64 MiB | memory of the server | 413 |
| part of an upload (`MAX_UPLOAD_PART`) | 64 MiB; any size below, recommended 8 MiB | fits under usual proxy limits | `upload.part_too_large` |
| file (`MAX_UPLOAD_SIZE`) | 16 GiB | protects the storage from accidents | `upload.too_large` |
| parts of one upload (`MAX_UPLOAD_PARTS`) | 100 000 | | `upload.bad_part` |
| unassigned upload (`UPLOAD_TTL`, operator: `I3CM_UPLOAD_TTL`) | expires after 24 hours | interrupted transfers can be resumed within a day | `upload.expired` |
| open uploads per account (`MAX_OPEN_UPLOADS`) | 100 | protects from forgotten uploads | `upload.too_many_open` |
| unassigned upload bytes per account (`MAX_STAGED_BYTES`) | 64 GiB | protects the staging area | `upload.staging_full` |
| content as base64 in an order of a client (`MAX_INLINE_CONTENT`) | 1 MiB (transition; module code is not limited yet) | files go through uploads | `content.inline_too_large` |
| stored answer of an idempotent request (`IDEMPOTENCY_TTL`) | 24 hours | covers retries after network errors | — (a later repeat runs again) |
| order of module code (operation, hook, schedule) (`ORDER_TIMEOUT`) | 10 s | one module must not block the instance | `module.timeout` |
| wait for the write turn of a tenant held in another server process (`WRITE_WAIT_SECS`) | 20 s | writing orders of a tenant run one after the other | 409 `tenant.busy` (retry) |
| wait for a database connection (`POOL_WAIT_SECS`) | 30 s | a request fails instead of waiting forever | 500 `pool` error |
| chain of hooks and calls | depth 16 | stops loops between hooks | `module.depth` |
| path in a filter or rights condition (`MAX_RIGHTS_STEPS`) | 3 steps | cost in SQL | `path.too_long` (catalog) |
| terms of a filter, named filters expanded (`MAX_FILTER_TERMS`) | 32 | SQL plans | `filter.too_complex` (catalog) |
| reference joins of a filter (`MAX_FILTER_JOINS`) | 6 | SQL plans | `filter.too_complex` (catalog) |
| named filters referring to named filters (`MAX_FILTER_DEPTH`) | 4 levels | stops cycles | `filter.too_deep` (catalog) |
| token lifetime (`TOKEN_TTL_DAYS`, `TOKEN_TTL_MAX_DAYS`) | default 90 days, at most 365 | | `token.bad_ttl` |
| start token of `init-tenant` (`INIT_TOKEN_TTL_DAYS`) | 1 day | only for issuing lasting tokens | `auth.expired_token` |
| unused token | lapses after 90 days | | `auth.unused_token` |
| source check of a development tenant (`MAX_MODULES`, `MAX_FILES`, `MAX_BYTES`) | 16 modules, 400 files, 4 MiB | | `check.too_large` |
| operator secret | at least 32 characters | | the server does not start |

## Orders

`POST /v1/orders` executes operations in one transaction; `"dry_run": true` checks everything
and rolls back. Object ids are UUIDs or the `ref` of an object created earlier in the order.

Writing transactions of one tenant run one after the other: an order waits until the previous
writing order of its tenant has committed or rolled back (reads never wait). Versions therefore
form a clean chain in commit order, and each committed writing transaction gets the next number of
its tenant's sequence (gap-free; refused orders and dry runs take none). Keep orders short, in
particular module operations, since they hold the tenant's write turn until they end.

```json
{"ops": [
  {"op": "create", "ref": "e", "room": "<room id>", "types": {"fibu:JournalEntry": {"documentDate": "2026-03-01", "postingDate": "2026-03-01", "documentNumber": "R-1", "fiscalYear": 2026}}},
  {"op": "create", "ref": "l1", "room": "<room id>", "types": {"fibu:JournalLine": {"side": "debit", "amount": {"amount": "10.00", "currency": "EUR"}}}},
  {"op": "link", "reference": "fibu:JournalLine.entry", "from": "l1", "to": "e"},
  {"op": "update", "id": "<id>", "types": {"base:Person": {"lastName": "New"}}},
  {"op": "attach", "id": "<id>", "type": "base:PersonAccount", "values": {"login": "x"}},
  {"op": "detach", "id": "<id>", "type": "base:PersonAccount"},
  {"op": "move", "id": "<id>", "room": "<room id>"},
  {"op": "unlink", "reference": "fibu:JournalLine.account", "id": "<link id>"},
  {"op": "content", "id": "e", "mediaType": "application/pdf", "filename": "r-1.pdf", "upload": "upl_…"},
  {"op": "text", "id": "e", "textType": "dms:mdplain", "content": "..."},
  {"op": "delete", "id": "<id>"}
]}
```

`content` and `text` write the content and a text of an object in the same order as the structure,
so that both are stored or neither. Hooks of modules do not run for them. The content comes from a
completed upload (`upload`, see [Uploads](#uploads)); `base64` instead of `upload` is still accepted
for small contents (see [Limits](#limits)) and answered with the warning `content.inline_deprecated`.

Response: `{"txn": 123, "dry_run": false, "created": {"e": "<id>", ...}, "links": [...], "warnings": [...]}`
(`warnings` only when there are any).
Integrity violations (preconditions, cardinalities, references needing a detached type) come
back as `integrity.violated` with a list of violations.

## Uploads

A file is uploaded first and assigned afterwards: the upload takes one file and returns one handle
`upl_…`, nothing else (no object, no assignment). An order then assigns it with the `content`
operation. If the order fails, nothing came into being and the handle can be used again.

```sh
# small files: one request
curl -X POST -H "Authorization: Bearer $TOKEN" --data-binary @r-1.pdf "$SERVER/v1/uploads?single=1"
# large files: open, send parts (any order, a repeat of the same part changes nothing), complete
curl -X POST -H "Authorization: Bearer $TOKEN" -d '{"sha256": "<optional>"}' $SERVER/v1/uploads
curl -X PUT  -H "Authorization: Bearer $TOKEN" --data-binary @part1 $SERVER/v1/uploads/upl_…/parts/1
curl -X POST -H "Authorization: Bearer $TOKEN" -d '{"parts": 2}' $SERVER/v1/uploads/upl_…/complete
```

* Completing assembles the parts (the upload shows `completing` meanwhile); a second `complete`
  while it runs is `upload.completing`, after it the same answer again.
* Parts are numbered from 1 without gaps and may have any size up to the part limit (there is no
  minimum; equal parts of e.g. 8 MiB with a shorter last one are usual). `complete` with `parts` names missing parts (`upload.incomplete`, `missing`); with a
  `sha256` (at open or complete) the assembled file is checked, and a mismatch keeps the upload open
  (`upload.checksum_mismatch`: send a missing last part and complete again). Other bytes under a part
  number already sent are `upload.part_conflict`.
* Downloads (`GET /v1/objects/{id}/content` and files of aspects backed by content) answer one range per request; a `Range` header that is not a single `bytes=` range is
  ignored and the whole content is sent (as HTTP allows), an unsatisfiable one gives 416.
* `GET /v1/uploads/{id}` shows state (`open`, `complete`, `consumed`, `expired`) and the parts
  received, so an interrupted transfer can resume with the missing parts.
* A handle belongs to the account that opened it; others get `upload.not_found`. It is used up by its
  first assignment (`upload.consumed`) and expires unassigned after the time in [Limits](#limits)
  (`upload.expired`).

## Idempotent requests

`POST /v1/orders`, `POST /v1/modules/{name}/ops/{op}`, `POST /v1/uploads` and
`POST /v1/uploads/{id}/complete` take a header `Idempotency-Key` (1 to 255 visible characters, e.g. a
UUID). Keys belong to the account. The first request with a key runs; a repeat with the same key on
the same endpoint and the same body returns the stored answer with `Idempotent-Replay: true` and
writes nothing. The same key with another endpoint or body is `409 idempotency.conflict`; a repeat
while the first request still runs is `409 idempotency.in_progress`. Only successful answers are
stored: after an error (4xx or 5xx) the key is free again. The client library and the tools send a
fresh key with every order.

## Endpoints

| Method and path | Purpose |
|---|---|
| `POST /v1/objects`, `PATCH`/`DELETE /v1/objects/{id}` | single-operation shortcuts (`?dry_run=true`) |
| `POST`/`DELETE /v1/objects/{id}/types/{type}` | attach / detach a type |
| `POST /v1/objects/{id}/move` | move to a room |
| `GET /v1/objects/{id}` | current state, readable parts only; `?at=<version>` for history |
| `GET /v1/objects/{id}/versions` | versions with time and account |
| `GET /v1/objects/{id}/ends/{end}` | objects at a named reference end (`limit`, `offset`) |
| `GET /v1/objects/{id}/content` | binary content, streamed; `Range: bytes=…` gives 206 with `Content-Range`; `ETag` is the SHA-256 |
| `PUT /v1/objects/{id}/content` | write content directly (`Content-Type`, `X-Filename`); deprecated, answered with `Deprecation: true`: use uploads |
| `POST /v1/uploads`, `PUT /v1/uploads/{id}/parts/{n}`, `POST /v1/uploads/{id}/complete`, `GET /v1/uploads/{id}` | uploads (see [Uploads](#uploads)) |
| `PUT`/`GET /v1/objects/{id}/texts/{textType}` | texts, e.g. `base:mail.md` |
| `POST /v1/references`, `DELETE /v1/references/{reference}/{id}` | link / unlink |
| `GET /v1/me`, `GET /v1/me/groups` | me and my groups (resolved through subgroups); `GET /v1/me` carries `_tenant: {id, name, development}` |
| `GET /v1/groups/{id}/members` | members, `?transitive=true` |
| `GET /v1/rooms` | module rooms by name and `root` |
| `GET /v1/rooms/{id}/children`, `/objects`, `/path` | room tree |
| `GET /v1/types/{type}/objects` | list by one attribute: `attr`, `eq` or `from`/`to`, `desc`, `limit` (without `attr` ordered by object id); paging by key with `after=<last id>` (or `offset` for short lists) |
| `GET /v1/types/{type}/groups` | counts per `unit` (year, month, day) of a date attribute `attr` |
| `GET /v1/catalog`, `GET /v1/profiles` | catalog and profiles |
| `POST /v1/assignments` | `{"profile": "fibu:accountant", "carrier": "<group or account>"}` |
| `GET /v1/assignments`, `DELETE /v1/assignments/{id}` | current assignments (`?carrier=` filters), end one |
| `POST /v1/tokens` | `{"account": "<id>", "label": "...", "ttl": 90}` issues a token; see Tokens |
| `GET /v1/tokens`, `DELETE /v1/tokens/{id}` | list (without secrets), revoke |
| `GET /v1/metrics` | requests served by this server process |
| `GET /health` | liveness, no token |
| `GET /v1/version` | version and source revision, no token |
| `GET /v1/objects/{id}/eval?expr=` | evaluate a path (FHIRPath, profile views) on an object |
| `GET /v1/aspects` | aspects |
| `GET /v1/aspects/{aspect}/children?node=&after=&limit=` | children of a node; empty `node` is the entry; every listing is paged by key (`after` = last node id of the previous page; `offset` still works for short lists) |
| `GET /v1/aspects/{aspect}/path?node=` | the chain of nodes from the top of the view down to `node` (to open a tree at a node) |
| `GET /v1/aspects/{aspect}/node?node=` | details: name, description, attributes, files, tabs, functions |
| `GET /v1/aspects/{aspect}/file?node=&name=` | a file of a node |
| `PUT /v1/aspects/{aspect}/file?node=&name=` | write a file of a node: the content or text behind it (one order) |
| `PUT /v1/aspects/{aspect}/index?node=` | write the index of an object: changed values in the front matter become an update (one order) |
| `POST /v1/aspects/{aspect}/drop?node=&name=` | a new file in a folder whose sub node declares `dropType`: creates an object with the bytes as content, linked over the folder's reference end |
| `GET /v1/aspects/{aspect}/tab?node=&tab=&offset=&limit=&sort=&desc=&format=` | one tab of an object node: `{key, label, columns, total, rows: [{node, displayName, type, cells}]}`, paged (default 100, at most 1000), sorted by the server by column index `sort` (as text, `desc=true` descending); `format=csv` or `format=xlsx` returns **all** rows as a download (CSV: UTF-8 with BOM, RFC 4180; a tab without columns exports the display names) |
| `GET /v1/aspects/{aspect}/index?node=&name=` | the index of a node (Markdown, see below) |
| `GET /v1/aspects/{aspect}/entry?node=&name=` | the node itself with the metadata of a listing |
| `POST /v1/aspects/{aspect}/files` | several files at once: `{"items": [{"node", "name"}, {"node", "index": true, "name"}]}`, answers `[{"data": "<base64>"}, …]` |
| `POST /v1/tools/{tool}`, `POST /v1/tools/exec` | the Unix tools on the data, see [tools.md](tools.md) |

Every entry is either a **node** (`"file": false`: it has details and children, possibly none)
or a **file** (`"file": true`: bytes, fetched with `file?node=&name=`). `node` is an opaque id:
clients pass it back unchanged and never build or parse it. `id` is set when an object stands
behind the entry, to switch to the generic object API. Entries of an object node appear in the
order of the aspect definition.

Every entry also carries `name` (the display name without `/`, used in paths), `size`,
`changedAt` and `ino`. For a file they describe its bytes; for a node they describe its
**index**. `ino` is derived from what the bytes are: the content and the texts of an object
have the same number in every view, the index of an object is the same within one aspect.
An object of an aspect whose name rule has a fallback (below) also carries `fallbackName`.

**Equal names.** `name` need not be unique within a listing; clients that need unique names
(FUSE, tools) resolve collisions over the complete listing: an entry whose `name` occurs more
than once takes its `fallbackName`, names that still collide stay plain for the smallest node id
and get ` ~2`, ` ~3`, … in node id order. The result does not depend on the order of the listing,
so a name always means the same object (`i3cm_tools::project`). An index shown under such a
name links relative to that name and has its own `ino`.

The **index** of an object node is a Markdown file: front matter with `_id`, `_type`, `_room`
and the readable attributes, then the display name, the description and the references as
`[[target|label]]` links. Targets below the node (its sub nodes and folders) are relative to the
directory name given as `name` (default: the node's `name`) and name exactly the entry a listing
shows (collisions resolved as above), everything else is `/_id/<id>`.
The index of a group or folder is its name as a heading. `GET /v1/aspects` reports `size` and
`ino` of the index of each aspect's entry.

Built-in views need no definition and are listed before the module aspects:

- `_id`: every object by its id; the entry lists nothing. Ids are addresses, view paths
  are not: store ids, never paths.
- `_type`: types with object counts, then their objects. Below an object: its content
  (named by its file name, else `content`), its texts, and one folder per named end that holds
  objects. Object details show `_id`, `_room`, `_type`, `_createdBy`, `_createdAt`, `_changedBy`,
  `_changedAt` and all attributes.


## Filters and rights conditions

One filter language serves the condition `when` of a right, the `where` of an aspect, named filters
and queries. It translates into SQL; what does not translate is refused when the catalog is loaded.

* comparisons of attributes with literals, `now()`, `today()` and date arithmetic (`today() + 7 days`,
  `due + 7 days < now()`, one quantity on a date, a date-time, `now()` or `today()`), `=`, `!=`, `<`,
  `<=`, `>`, `>=`; `x in ('a', 'b')` (one term); both sides must be of the same kind (numbers with
  numbers, dates and date-times with each other) and literals must be valid values of the attribute,
  otherwise the filter is refused (`path.bad_filter`);
* `exists()`, `empty()` on attributes and reference ends; `and`, `or`, `not()` (also `not(x)`); the
  logic is two-valued: a comparison with an absent value is false, so its `not()` is true;
* paths over reference ends with at most 3 steps, to `%me` (`applicant.team.leader = %me`) or to an
  attribute of the object reached (`applicant.team.name = 'a'`, true when some object reached matches);
* texts and content: `_texts.<text type>.exists()` / `.empty()`, `_content.exists()`,
  `_content.mediaType in (…)`, `_content.size > n`, `_content.sha256 = '…'`; media types are stored
  in lower case without parameters (`Text/Plain; charset=UTF-8` is stored as `text/plain`), so
  compare with that form;
* attributes of attachable types (`@requires` on the entry type) by their name; when the type is not
  attached the attribute is absent (`empty()` is true, comparisons are false);
* named filters: `@name` (a filter of the module that writes the condition, view or filter) or
  `@ns:name`, defined in the module's catalog (see [modules](modules.md)); a named filter on a type
  applies to its subtypes as well. Named filters and attachable types of modules not active in the
  tenant do not exist for its filters.

`%me` is the account or any of its groups. A **right with a condition** applies only to objects for
which it holds; conditions are plus rights on named types and grant read, write and delete on existing
objects (create, place and remove need unconditional rights). A right on a type applies to that
type's part of every object carrying it, also of objects of a subtype; the subtype's own part needs a
right on the subtype. A **query** over the API or from module code (`where` of `query`) may follow one
reference to `%me` but no longer paths (`query.path_not_allowed`), also not inside a named filter it
refers to: paths in queries could reveal objects the caller cannot read. For the same reason values of
other type parts of the object (supertype, host and attached types, their texts) count in a query, and
in the attribute filter of `find`, only where the caller may read that part; elsewhere they are
absent. Module code can ask `query` for the references of each result with `"references": true`.

Lists combine the rooms with unconditional rights and each condition as separate branches of one
`UNION` (no `OR` in a query) and page by key, so every page costs the same. Measured with 100,000
objects, pages of 50, a condition with `or`, `not()` and a path of three steps: p95 3.8 ms (2.9 ms
when nothing matches).

## References, rights and deletion

References have no direction; the end that declares a reference in TypeScript only names it.

* **Linking or unlinking** a reference needs write on one of the two objects (create for an object of
  the same order; rights paths count) and read on the other, from either side.
* A reference is **rights-relevant** when a condition of some profile runs over it. Linking or
  unlinking it needs nothing more than any other reference: writing data never grants rights. The
  profiles decide who may write which object (who may set `Team.leader` is whoever may write the
  team), and the tenant administrator decides who gets a profile. A module should therefore not give
  write on objects its own rights paths depend on to the people those paths favour.
* Group memberships need `grant` on the membership; the room tree (`base:Room.parent`) needs write on
  the child room.
* **Deleting** an object ends all its references at both ends. It is refused only if an object at the
  other end would fall below the minimum of its end (`integrity.violated`, `cardinality.violated`).
  Parts (`@partOf`) are deleted with their whole; the right to delete the whole suffices.

## Writing through views

Every write is one order: rights, required attributes and checks apply, all or nothing.
Writable are the content of an object, its texts and the attribute values in the front matter
of its index; the reserved fields and the body (references) are not writable through the index
(`index.reserved_field`, `index.body_readonly`; references are changed with orders). A file
copied into a folder with `dropType` creates an object; required text attributes named
`filename`, `name` or `title` take the file name. `sed -i` on files of the tree (tools) runs on
the server as one order over all files.

## Tokens

A token belongs to an account; the server stores only its hash. Without `account`, a caller
issues a token for itself; it needs no right and never expires later than the token of the
request. A token for another account needs the right to grant rights. `ttl` is in days
(default 90, at most 365). Tokens created on the command line expire as well:
`i3cm-server create-token <tenant> <login> [--ttl <days>]` (default 90, at most 365); the first
token printed by `init-tenant` is a start key valid for one day. `GET /v1/tokens` lists the caller's tokens, or all tokens of the
tenant with the right to grant rights (`?account=` filters); `DELETE /v1/tokens/{id}` revokes a
token at once. A token unused for 90 days lapses (`auth.unused_token`). The tokens of a deactivated
(`deactivated: true` on the account) or deleted account are refused (`auth.account_inactive`); after
reactivation they work again.

**The last administrator is protected.** An order, or the end of an assignment, that would leave the
tenant without an active account able to grant rights at the root room is refused with 409
`tenant.last_admin` (deleting, deactivating or detaching that account, deleting or unlinking its group
or membership, ending the profile assignment or adding one with `-` or `block` rights). A deactivated
account cannot act through module code either (impersonation, schedules). Add another administrator first.

## Modules

| Method and path | Purpose |
|---|---|
| `GET /v1/modules` | Installed modules: requires, mandatory, developmentOnly, active in the tenant, operations |
| `POST /v1/modules/{name}/activate` | Activate a module in the tenant (needs `grant` at the root) |
| `POST /v1/modules/{name}/ops/{op}` | Run an operation of the module as one order; body: JSON params |
| `GET /v1/modules/{name}/sources` | The module's sources (`{module, files: [{path, text}]}`, paths `i3cm/...`) if its package carries them; for modules active in the tenant, in a development tenant for every installed module |

With module code loaded (`--modules-dir`), every write runs the hooks of the modules around each
operation: `POST /v1/orders`, the single-operation shortcuts (`POST`/`PATCH`/`DELETE /v1/objects…`,
`/v1/references…`), writes to files of aspects (FUSE, `PUT /v1/aspects/{a}/file`, drops) and the tools.
Writes of content and texts (`content`, `text`, `PUT …/content`, `PUT …/texts/…`, files written
through views) run the hooks `on: "content"` and `on: "text"`; filter hooks run at the end of every
order (see [modules](modules.md)). Errors from module code come back with status 400 and the
module's error id.

## Development tenants and source checks (studio)

The operator allows development tenants per instance with `I3CM_ALLOW_DEVELOPMENT=true`; then
`i3cm-server init-tenant <name> --development` creates one. A module that declares
`developmentOnly: true` in `module.ts` can only be activated there (`module.development_only`).

`POST /v1/catalog/check` (development tenants only, for guardians or whoever may write
`studio:SourceFile`) checks module sources that are not installed yet:

```json
{ "modules": [ { "name": "probe", "files": [ { "path": "i3cm/module.ts", "text": "..." },
                                              { "path": "i3cm/types/a.ts", "text": "..." } ] } ] }
```

The sources are built with `typecat` against the kernel types and the sources of the installed
modules they require (packages carry their sources), then the catalog is loaded with the server's
checker. The answer lists every issue, not only the first:

```json
{ "catalog": { ... } | null,
  "issues": [ { "severity": "error", "source": "typescript|typecat|checker", "error_id": "...",
                "description": "...", "params": { ... }, "file": "probe:i3cm/types/a.ts", "line": 4, "column": 3 } ],
  "positions": { "type:probe:A": { "file": "probe:i3cm/types/a.ts", "line": 3, "column": 1 },
                 "aspect:probe:list": { ... } } }
```

`catalog` is null when there is an error. File names are `<module>:<path>`; kernel types appear as
`kernel:<path>`. The server needs `node` and typecat: `I3CM_NODE` (default `node`), `I3CM_TYPECAT`
(typecat.mjs), `I3CM_TYPES_DIR` (kernel type sources); the image sets them.

`i3cm-server catalog check [catalog.json]` runs the same checker on a catalog without a database
(exit 1 on an error).

## Operator surface

Separate from tenant tokens and off by default: with `I3CM_OPERATOR_TOKEN` (at least 32
characters) and a modules directory, the server also offers `GET /v1/operator/registry` and
`POST /v1/operator/modules/install` (`{"name", "version"?}`), authenticated with that secret.
See [modules.md](modules.md) and the operator documentation.
