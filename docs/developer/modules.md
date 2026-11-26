# Module repos

A module of i3cm can live in a repository of its own (for example `i3cm-org/i3cm-tasks`). The kernel's
type compiler `typecat` reads it next to the kernel's own types and produces one `catalog.json`.

## Layout

```
<repo>/
  i3cm/
    module.ts          name, required modules, rooms, profiles
    types/*.ts         type classes (see types.md)
    aspects/*.ts       aspects (views)
    i18n/de.ftl        display texts, one file per language
    i18n/en.ftl
    ui/<ns>.html       <template> elements for the UI frame (see ui.md)
  ops/                 code for operations and hooks (later, WebAssembly)
```

Files may sit in any subdirectory below `i3cm/`; a file belongs to the nearest directory above it that
holds a `module.ts`. The repository is recognised by `i3cm/module.ts`.

## Imports

Module sources import the kernel through two fixed names; no copy of the kernel is needed in the module
repo:

```ts
import { ns, indexed, Ref, LocalDate, aspect } from "@i3cm/lib";   // notation: value types, decorators, aspect()
import { Person } from "@i3cm/base";                                 // types of the base module
```

```ts
// i3cm/module.ts
import type { ProfileDef } from "@i3cm/lib";
export const module = { name: "demo", requires: ["base"] };
export const rooms: string[] = ["demo"];
export const profiles: ProfileDef[] = [];
```

Every name in `requires` must be a module that is part of the same build (the kernel's `base` or
another module repo given to `typecat`); otherwise `typecat` stops with `typecat.unknown_module`. A
module name given twice stops with `typecat.duplicate_module`.

## Building against the kernel

With a checkout of the kernel (`i3cm-org/i3cm-base`) at `$I3CM_BASE`:

```sh
cd $I3CM_BASE/tools/typecat && npm ci            # once
cd $I3CM_BASE
node tools/typecat/typecat.mjs types --module /path/to/i3cm-tasks --out catalog.json
# several modules: repeat --module, or
I3CM_MODULES=/path/to/i3cm-dms:/path/to/i3cm-mail make catalog
```

Without `--out` the catalog goes to stdout, which is handy for a quick check. Errors come as JSON on
stderr (`error_id`, `description`, `params`) with exit code 2; TypeScript errors name the file and line.

The UI templates of all modules are copied into the frame's module directory with `--ui-out`:

```sh
node tools/typecat/typecat.mjs types --module /path/to/i3cm-mail --out catalog.json --ui-out ui/modules
```

The server reads `catalog.json` as before (`I3CM_CATALOG`); the UI frame loads `modules/<ns>.html` for
every namespace that has an aspect.

## Editor support

For type checking in an editor, a module repo may carry a `tsconfig.json` that points the two names at a
kernel checkout:

```json
{
  "compilerOptions": {
    "target": "ES2022", "module": "ES2022", "moduleResolution": "Bundler",
    "experimentalDecorators": true, "strict": true, "noEmit": true, "skipLibCheck": true,
    "paths": {
      "@i3cm/lib": ["../i3cm-base/types/lib/index.ts"],
      "@i3cm/base": ["../i3cm-base/types/base/index.ts"]
    }
  },
  "include": ["i3cm/**/*.ts"]
}
```

`typecat` does not read this file; it sets the two names itself.

An example module repo is `tools/typecat/test/fixtures/demo-module`.

## Importing other modules

Besides `@i3cm/lib` and `@i3cm/base`, the types of every module given in the same run are
importable as `@i3cm/<module>` if the module has an `i3cm/index.ts` (or `i3cm/types/index.ts`)
that re-exports them, e.g. `import { Document } from "@i3cm/dms"`.

## Module code (WebAssembly)

Operations and hooks are a WebAssembly component (`wasm32-wasip2`) built from the module's `ops/`
directory against the kernel's WIT world `i3cm:module` (`wit/module.wit`): the module exports
`run(conn, op, params) -> result<string, string>`; the kernel hands it a `connection` with `me`,
`get`, `query`, `order`, `call` and `impersonate`. One order is one database transaction; writes go
in immediately, any error or trap aborts the whole order; every order has a deadline of 10 s.
`examples/example-module` is a complete module (types, profiles, carrier, operations, a hook, a
schedule) with its code in Rust (`wit-bindgen`).

`module.ts` declares the code:

```ts
export const module = { name: "example", requires: ["base"] };      // mandatory: true for kernel-delivered modules
export const carriers: CarrierDef[] = [ { name: "system", profiles: ["writer"] } ];
export const operations: OperationDef[] = [ { name: "greet" }, { name: "stamp", impersonates: ["system"] } ];
export const hooks: HookDef[] = [ { type: "example:Note", on: "create", phase: "before", op: "checkNote" } ];
export const schedules: ScheduleDef[] = [ { op: "tick", rrule: "FREQ=MINUTELY" } ];
```

* A carrier is an agent account of the module, created on activation with the given profiles.
  `impersonate` is allowed only for operations listing the carrier.
* Hooks get read-only handles; a `before` hook rejects the order by returning an error (a JSON
  error object `{"error_id", "description", "params"}` or text).
* Schedules run per tenant in the identity of the module's first carrier, exactly once per
  period even with several main containers (`FREQ=MINUTELY|HOURLY|DAILY|WEEKLY;INTERVAL=n`).

Build and run:

```sh
cd ops && cargo build --release --target wasm32-wasip2
mkdir -p ../modules && cp target/wasm32-wasip2/release/<crate>.wasm ../modules/<module>.wasm
i3cm-server serve --modules-dir ../modules
```

## Activation

A tenant has the base and the mandatory modules from its creation. Other installed modules are
activated by the tenant administrator: `POST /v1/modules/{name}/activate` checks that required
modules are active, creates the module's rooms, profiles and carriers, applies imprinting rules to
existing objects, all or nothing, and reports the new rights (`red` where they reach the root room
or types of other modules). Types of modules not active in a tenant do not exist for it.

