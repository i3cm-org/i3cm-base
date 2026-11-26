# REST API (v1)

All requests send `Authorization: Bearer <token>`. Errors are
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
| `POST /v1/tokens` | `{"account": "<id>"}` issues a token |
| `GET /v1/objects/{id}/eval?expr=` | evaluate a path (FHIRPath, profile views) on an object |
| `GET /v1/aspects` | aspects |
| `GET /v1/aspects/{aspect}/children?node=&offset=&limit=` | children of a node; empty `node` is the entry; every listing is paged |
| `GET /v1/aspects/{aspect}/node?node=` | details: name, description, attributes, files, tabs, functions |
| `GET /v1/aspects/{aspect}/file?node=&name=` | a file of a node |

Every entry is either a **node** (`"file": false`: it has details and children, possibly none)
or a **file** (`"file": true`: bytes, fetched with `file?node=&name=`). `node` is an opaque id:
clients pass it back unchanged and never build or parse it. `id` is set when an object stands
behind the entry, to switch to the generic object API. Entries of an object node appear in the
order of the aspect definition.

Built-in views need no definition and are listed before the module aspects:

- `_id`: every object by its id; the entry lists nothing. Ids are addresses, view paths
  are not: store ids, never paths.
- `_type`: types with object counts, then their objects. Below an object: its content
  (named by its file name, else `content`), its texts, and one folder per named end that holds
  objects. Object details show `_id`, `_room`, `_type`, `_createdBy`, `_createdAt`, `_changedBy`,
  `_changedAt` and all attributes.

