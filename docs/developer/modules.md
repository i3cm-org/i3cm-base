# Module repos

A module of i3cm can live in a repository of its own (for example `i3cm-org/i3cm-tasks`). The kernel's
type compiler `typecat` reads it next to the kernel's own types and produces one `catalog.json`.

## Layout

```
<repo>/
  i3cm/
    module.ts          name, required modules, rooms, roles
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
import type { RoleDef } from "@i3cm/lib";
export const module = { name: "demo", requires: ["base"] };
export const rooms: string[] = ["demo"];
export const roles: RoleDef[] = [];
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
