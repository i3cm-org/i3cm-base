# REST API (v1)

All requests send `Authorization: Bearer <token>` (401 with `auth.invalid_token`,
`auth.expired_token` or `auth.revoked_token` otherwise). Errors are
`{"error_id": "...", "description": "...", "params": {...}}` with status 400 (invalid),
401 (no or unknown token), 404 (not found or not allowed: both look the same), 409 (conflict,
integrity), 500.

## Orders

`POST /v1/orders` executes operations in one transaction; `"dry_run": true` checks everything
and rolls back. Object ids are UUIDs or the `ref` of an object created earlier in the order.

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
  {"op": "delete", "id": "<id>"}
]}
```

Response: `{"txn": 123, "dry_run": false, "created": {"e": "<id>", ...}, "links": [...]}`.
Integrity violations (preconditions, cardinalities, references needing a detached type) come
back as `integrity.violated` with a list of violations.

## Endpoints

| Method and path | Purpose |
|---|---|
| `POST /v1/objects`, `PATCH`/`DELETE /v1/objects/{id}` | single-operation shortcuts (`?dry_run=true`) |
| `POST`/`DELETE /v1/objects/{id}/types/{type}` | attach / detach a type |
| `POST /v1/objects/{id}/move` | move to a room |
| `GET /v1/objects/{id}` | current state, readable parts only; `?at=<version>` for history |
| `GET /v1/objects/{id}/versions` | versions with time and account |
| `GET /v1/objects/{id}/ends/{end}` | objects at a named reference end (`limit`, `offset`) |
| `PUT`/`GET /v1/objects/{id}/content` | binary content (`Content-Type`, `X-Filename`) |
| `PUT`/`GET /v1/objects/{id}/texts/{textType}` | texts, e.g. `base:mail.md` |
| `POST /v1/references`, `DELETE /v1/references/{reference}/{id}` | link / unlink |
| `GET /v1/me`, `GET /v1/me/groups` | me and my groups (resolved through subgroups) |
| `GET /v1/groups/{id}/members` | members, `?transitive=true` |
| `GET /v1/rooms` | module rooms by name and `root` |
| `GET /v1/rooms/{id}/children`, `/objects`, `/path` | room tree |
| `GET /v1/types/{type}/objects` | list by one attribute: `attr`, `eq` or `from`/`to`, `desc`, `limit`, `offset` |
| `GET /v1/types/{type}/groups` | counts per `unit` (year, month, day) of a date attribute `attr` |
| `GET /v1/catalog`, `GET /v1/roles` | catalog and roles |
| `POST /v1/assignments` | `{"role": "fibu:accountant", "carrier": "<group or account>"}` |
| `POST /v1/tokens` | `{"account": "<id>", "label": "...", "ttl": 90}` issues a token; see Tokens |
| `GET /v1/tokens`, `DELETE /v1/tokens/{id}` | list (without secrets), revoke |
| `GET /v1/metrics` | requests served by this server process |
| `GET /v1/objects/{id}/eval?expr=` | evaluate a path (FHIRPath, profile views) on an object |
| `GET /v1/aspects` | aspects |
| `GET /v1/aspects/{aspect}/children?node=&offset=&limit=` | children of a node; empty `node` is the entry; every listing is paged |
| `GET /v1/aspects/{aspect}/node?node=` | details: name, description, attributes, files, tabs, functions |
| `GET /v1/aspects/{aspect}/file?node=&name=` | a file of a node |
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


## Tokens

A token belongs to an account; the server stores only its hash. Without `account`, a caller
issues a token for itself; it needs no right and never expires later than the token of the
request. A token for another account needs the right to grant rights. `ttl` is in days
(default 90, at most 365). Tokens created on the command line expire as well:
`i3cm-server create-token <tenant> <login> [--ttl <days>]` (default 90, at most 365); the first
token printed by `init-tenant` is a start key valid for one day. `GET /v1/tokens` lists the caller's tokens, or all tokens of the
tenant with the right to grant rights (`?account=` filters); `DELETE /v1/tokens/{id}` revokes a
token at once.
