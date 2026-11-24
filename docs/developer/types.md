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
