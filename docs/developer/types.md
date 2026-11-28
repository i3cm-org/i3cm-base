# Type notation

Types are TypeScript classes in `types/<module>/`. `typecat` reads them statically; nothing is
executed. A module directory contains `module.ts` (name, required modules, rooms, profiles) and
`i18n/<lang>.ftl` (Fluent display texts).

```ts
@ns("fibu")
export class JournalLine {
  @partOf entry!: Ref<JournalEntry, "lines", "1..n">;  // part of an entry; an entry has 1..n lines
  account!: Ref<LedgerAccount, "lines">;
  side!: Enum<"debit" | "credit">;
  amount!: Money;
}
```

* Optional attributes use `?`; all others are required.
* `extends` makes a subtype. It inherits attributes, references, `@hasContent` and `@texts` along
  the chain; in the catalog a subtype shows `content: null` and only its own text types, and the
  checker resolves content and texts through the ancestors.
* `Ref<T, "end", "card">` references an object carrying type `T`; the second argument names the
  opposite end, the optional third one its cardinality (`"0..n"` by default, also `"1..n"`,
  `"0..1"`, `"1"`). Cardinality at this end follows the form: `Ref` = 1, `Ref?` = 0..1,
  `Ref[]` = 0..n (`@min(n)` for more). `AnyObject` as target accepts every object.
* Names at a type are its attributes, its own reference ends and the opposite ends of references
  pointing to it, including those of its supertypes. A name may occur only once within a module
  and against the base; equal names from unrelated modules are allowed and told apart by
  qualification in paths (``person.`verein:memberships` ``). Names starting with `_` are reserved.
* Class decorators: `@ns`, `@abstractType`, `@requires(A, B)` (one of), `@requiresAll(A, B)`,
  `@brings(B)` (every object of this type is also a B), `@imprintsOn(A)` (every A is also of
  this type), `@hasContent(mediaType?)`, `@texts("name:mediaType", ...)`, `@noGrep("name", ...)` (text
  types not searched by `grep`, e.g. the OCR layer of a scan).
* Attribute decorators: `@indexed`, `@unique`; reference decorators: `@partOf`, `@ordered`, `@min`.
  `@rightsRelevant` is no longer needed and ignored (`typecat check` warns): a reference is
  rights-relevant when a rights path of some profile runs over it.
* References have no direction: both ends are named, and rights and deletion treat both ends alike
  (see the API reference, "References, rights and deletion"). `@partOf` makes the object a part of
  the referenced whole: parts move with the whole and are deleted with it.

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

## Profiles

```ts
export const profiles: ProfileDef[] = [
  { name: "accountant", rights: [
    { sign: "+", art: ["write", "create"], types: ["fibu:JournalEntry", "fibu:JournalLine"], room: "fibu" } ] },
];
```

`art`: read, write (includes read), create, place, remove, delete, grant. `types`: type names
or `*`. `room`: `root` or a room of the module. `includes` lists profiles that a profile contains.

## Aspects (views)

An aspect describes how objects are traversed and shown (a view: objects are stored once,
the aspect only arranges them). If a grouping expression yields several values, the object
appears under each of them, and group counts are occurrences. An aspect has an entry type with optional grouping,
and per type the attributes, sub nodes (direct or in folders), files, tabs and functions. Paths
are FHIRPath expressions (strict standard; profile "views": `where`, `ofType`, `first`, `last`,
`count`, `exists`, `empty`, `not`, `toString`, `substring`, `upper`, `lower`). Names starting with
`_` are reserved: `_content`, `_texts` (with one entry per text type), `_id`, `_room`,
`_createdBy`, `_createdAt`, `_changedBy`, `_changedAt` (last change of any readable part). The
order of keys is the display order, also between the `subNodes` and `files` blocks.

```ts
export const mailbox = aspect("mailbox", {
  entry: { type: "mail:Mail", groupBy: ["receivedAt.toString().substring(0, 10)"] },
  nodes: {
    "mail:Mail": {
      attributes: { sender: "sender", attachmentCount: "attachments.count()" },
      subNodes: { attachments: { path: "attachments", folder: true } },
      files: { "mail.eml": "_content", "mail.md": "_texts.`mail.md`" },
      tabs: { attachments: { path: "attachments", columns: { file: "filename" } } },
      functions: { reply: { url: "/modules/mail/reply" } },
    },
  },
});
```

A file may name a list of sources, `"mail.md": ["_texts.md", "_texts.txt"]`: the first one present
is read; writing the file always writes the first source, never a fallback (a missing first source is
created; content takes its media type from the file name).

Display texts within an aspect: `<ns>-<aspect>--<ns>-<Type>-name`/`-description` override the
global texts; `<ns>-<aspect>--label-<key>` names attributes, folders, tabs, columns and functions.
Folders keep their key as their name in paths (FUSE, tools, links in Markdown), whatever the
language; the translated label is only their display name.
A name rule may carry a fallback as the Fluent attribute `.fallback`; it applies when names in one
directory collide (otherwise ` ~2`, ` ~3` in id order):

```ftl
mail-mailbox--mail-Mail-name = { $sender } - { $subject }
    .fallback = { $sender } - { $subject } ({ $receivedAt })
```

At a type with `@requires(X)` (one type) or `@requiresAll(...)`, paths, filters and grouping
rules may also use the names of the required host types: the object is guaranteed to carry them
(e.g. at `tasks:Deadline` the attributes `due` and `status` of `tasks:Task`). Filters can shift
`now()`, `today()` and date attributes by a quantity: `due <= today() + 7 days` (minutes, hours,
days, weeks, months, years; SQL intervals). Enum values get display texts from Fluent messages
`<ns>-<Type>-<attr>-<value>`; node details return them as `display` next to `values`.
Objects without a value for the grouping rule (null, empty string, empty list) form one extra
group with the empty key, labelled `<ns>-<aspect>--label-_none` (default "—"). A node definition
may be held in a module-level `const` and used by several aspects of the same file (static values
only, no function calls). Paths and name patterns may use `_content.filename`, `_content.mediaType`
and `_content.size` (`"{_content.filename}"`). A grouping rule is an attribute of the entry type itself, optionally with `.toString()`,
`.substring(0, n)` and finally `.upper()` or `.lower()`; list attributes are allowed. Grouping
always runs in SQL; other expressions are rejected when the catalog is loaded. A tab lists the objects reached by its
`path`; with `columns` a client shows it as a grid, without as a list. Functions are
declared and delivered with the node; the base does not call them yet.
