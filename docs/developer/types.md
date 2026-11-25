# Type notation

Types are TypeScript classes in `types/<module>/`. `typecat` reads them statically; nothing is
executed. A module directory contains `module.ts` (name, required modules, rooms, roles) and
`i18n/<lang>.ftl` (Fluent display texts).

```ts
@ns("fibu")
export class JournalLine {
  @partOf @card("1..n") entry!: Ref<JournalEntry, "lines">;  // part of an entry; an entry has 1..n lines
  @card("0..n") account!: Ref<LedgerAccount, "lines">;
  side!: Enum<"debit" | "credit">;
  amount!: Money;
}
```

* Optional attributes use `?`; all others are required.
* `Ref<T, "end">` references an object carrying type `T`; the second argument names the
  opposite end. Cardinality at this end follows the form: `Ref` = 1, `Ref?` = 0..1,
  `Ref[]` = 0..n (`@min(n)` for more). `@card(...)` sets the cardinality at the opposite end.
  `AnyObject` as target accepts every object.
* Class decorators: `@ns`, `@abstractType`, `@requires(A, B)` (one of), `@requiresAll(A, B)`,
  `@brings(B)` (every object of this type is also a B), `@imprintsOn(A)` (every A is also of
  this type), `@hasContent(mediaType?)`, `@texts("name:mediaType", ...)`.
* Attribute decorators: `@indexed`, `@unique`; reference decorators: `@partOf`, `@ordered`,
  `@rightsRelevant`, `@card`, `@min`.

## Value types

| Notation | Stored as | JSON |
|---|---|---|
| `string`, `boolean`, `Int`, `Decimal<P,S>` | text, boolean, bigint, numeric | string, bool, number, string |
| `Text<"TXT"\|"MD"\|"HTML">` | jsonb | `"..."` or `{"format": "MD", "content": "..."}` |
| `LocalDate`, `DateTime`, `Time`, `Duration` | date, timestamptz (UTC), time, text | ISO 8601 strings |
| `Enum<"a"\|"b">` | text | string |
| `Money` | jsonb | `{"amount": "12.50", "currency": "EUR"}` |
| `Formatted<"email"\|"url"\|"iban"\|"phone">` | text | string, checked on write |
| `Recurrence` | text | RRULE (RFC 5545), e.g. `FREQ=MONTHLY;BYDAY=1MO` |
| `Struct<{...}>` | jsonb | object with primitive fields |
| `T[]` | array (jsonb for structured values) | array, ordered, duplicates allowed |

## Display texts

`i18n/<lang>.ftl` holds `<ns>-<Type>-name` and `<ns>-<Type>-description`; variables are the
attributes of the type and its supertypes. The API picks the language from `Accept-Language`.

## Roles

```ts
export const roles: RoleDef[] = [
  { name: "accountant", rights: [
    { sign: "+", art: ["write", "create"], types: ["fibu:JournalEntry", "fibu:JournalLine"], room: "fibu" } ] },
];
```

`art`: read, write (includes read), create, place, remove, delete, grant. `types`: type names
or `*`. `room`: `root` or a room of the module. `includes` lists roles that a role contains.

## Aspects (views)

An aspect describes how objects are traversed and shown (a view: objects are stored once,
the aspect only arranges them). If a grouping expression yields several values, the object
appears under each of them, and group counts are occurrences. An aspect has an entry type with optional grouping,
and per type the attributes, sub nodes (direct or in folders), files, tabs and functions. Paths
are FHIRPath expressions (strict standard; profile "views": `where`, `ofType`, `first`, `last`,
`count`, `exists`, `empty`, `not`, `toString`, `substring`, `upper`, `lower`). Names starting with
`_` are reserved: `_content`, `_texts` (with one entry per text type), `_id`, `_room`,
`_createdBy`, `_createdAt`. The order of keys is the display order.

```ts
export const mailbox = aspect("mailbox", {
  entry: { type: "base:Mail", groupBy: ["receivedAt.toString().substring(0, 10)"] },
  nodes: {
    "base:Mail": {
      attributes: { sender: "sender", attachmentCount: "attachments.count()" },
      subNodes: { attachments: { path: "attachments", folder: true } },
      files: { "mail.eml": "_content", "mail.md": "_texts.`mail.md`" },
      tabs: { attachments: { path: "attachments", columns: { file: "filename" } } },
      functions: { reply: { url: "/modules/mail/reply" } },
    },
  },
});
```

Display texts within an aspect: `<ns>-<aspect>--<ns>-<Type>-name`/`-description` override the
global texts; `<ns>-<aspect>--label-<key>` names attributes, folders, tabs, columns and functions.
Group expressions of the form `attr`, `attr.toString()` or `attr.toString().substring(0, n)` on
the entry type's own attributes are evaluated in SQL, other expressions in memory. Functions are
declared and delivered with the node; the base does not call them yet.
