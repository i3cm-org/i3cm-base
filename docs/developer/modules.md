# Module repos

A module of i3cm can live in a repository of its own (for example `i3cm-org/i3cm-tasks`). The kernel's
type compiler `typecat` reads it next to the kernel's own types and produces one `catalog.json`.

## Limits

For module code: an order of module code (operation, hook, schedule) has 10 s (`ORDER_TIMEOUT`,
`module.timeout`), chains of hooks and calls end at depth 16 (`module.depth`), rights paths have at
most 3 steps (`MAX_RIGHTS_STEPS`). Contents written by module code as base64 are not limited yet; this
transition ends when module code takes upload handles. All limits of the API with value, reason and
error: [api.md](api.md#limits).

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
  ops/                 code for operations, hooks and schedules (WebAssembly component)
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

With a checkout of the kernel (`i3cm-org/i3cm-base`) at `$KERNEL`:

```sh
cd $KERNEL/tools/typecat && npm ci            # once
cd $KERNEL
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
`get`, `query`, `ends`, `order`, `call` and `impersonate`. One order is one database transaction;
writes go in immediately, any error or trap aborts the whole order; every order has a deadline of
10 s. Everything a handle reads or writes is checked against the rights of its identity.

| Function | What it does |
|---|---|
| `get(id)` | the object as JSON (readable parts only), with `references`: `{<end>: [id, …]}` for the filled reference ends, readable targets only, at most 100 per end; ends with more are listed in `referencesTruncated` |
| `query(type, filter)` | `filter` `{"where": "<expression>", "after": "<id>", "limit": 100}`: objects matching an expression in the language of an aspect's `where` (`now()`, `today()`, `%me`, date arithmetic, names of the host type), readable ones only, ordered by id, paged by key; answers `{"items": [...], "next": "<id>" \| null}`. Any other JSON object: own attributes equal to the given values, at most 200, as an array |
| `ends(id, end, offset, limit)` | ids of the readable objects at a reference end, in the end's order, at most 500 per call |
| `order(ops)` | operations as in `POST /v1/orders`, applied at once in the open order; returns `{local name: id}` |
| `call(op, params)` | an operation of a module (`ns:op`), same order, same identity |
| `impersonate(carrier)` | a handle acting as a carrier of this module, for operations marked for it |

The WIT package is `i3cm:module@0.6.1`, a compatible extension of `0.6.0`: components built against
`0.6.0` keep running; use `0.6.1` for `ends`.
`examples/example-module` is a complete module (types, profiles, carrier, operations, a hook, a
schedule) with its code in Rust (`wit-bindgen`).

`module.ts` declares the code:

```ts
export const module = { name: "example", requires: ["base"] };      // mandatory: true for kernel-delivered modules,
                                                                    // developmentOnly: true for tools such as the studio
export const carriers: CarrierDef[] = [ { name: "system", profiles: ["writer"] } ];
export const operations: OperationDef[] = [ { name: "greet" }, { name: "stamp", impersonates: ["system"] } ];
export const hooks: HookDef[] = [
  { type: "example:Note", on: "create", phase: "before", op: "checkNote" },
  { type: "example:Note", on: "update", phase: "after", op: "onUpdate" },
];
export const schedules: ScheduleDef[] = [ { op: "tick", rrule: "FREQ=MINUTELY" } ];
```

* A carrier is an agent account of the module, created on activation with the given profiles.
  `impersonate` is allowed only for operations listing the carrier.
* Hooks run in the identity of the order and get `{"phase", "on", "type", "id", "op"}` as
  parameters. A `before` hook gets a read-only handle and rejects the order by returning an error (a
  JSON error object `{"error_id", "description", "params"}` or text). An `after` hook may write
  with `order` in the same order, with the same rights; what it writes runs hooks again, so an
  `after` hook must stop writing once its work is done (check the state first). A chain of hooks
  and calls ends at depth 16 with `module.depth`; any failing hook aborts the whole order.
* Schedules run per tenant in the identity of the module's first carrier, exactly once per
  period even with several main containers (`FREQ=MINUTELY|HOURLY|DAILY|WEEKLY;INTERVAL=n`).

Build and run:

```sh
cd ops && cargo build --release --target wasm32-wasip2
mkdir -p ../modules && cp target/wasm32-wasip2/release/<crate>.wasm ../modules/<module>.wasm
i3cm-server serve --modules-dir ../modules
```

## Packaging and signing

A module is delivered as a signed package, built by `tools/typecat/pack.mjs` of the kernel:

```sh
node $KERNEL/tools/typecat/pack.mjs keygen keys/        # once: keys/signing.pem (secret), keys/signing.pub (hex)
node $KERNEL/tools/typecat/pack.mjs $KERNEL/types . \
     --wasm ops/target/wasm32-wasip2/release/<crate>.wasm --version 1.2.0 --out dist \
     [--module <repo of a required module>]... [--key keys/signing.pem | I3CM_SIGNING_KEY]
```

`dist/` then holds:

| File | Content |
|---|---|
| `<name>-<version>.tar.gz` | `module.json` (the module's part of the catalog), `module.wasm`, `i18n/*.ftl`, `ui/*.html`, `src/` (the sources under `i3cm/`), `SHA256SUMS`; reproducible (file times 0) |
| `<name>-<version>.tar.gz.sig` | ed25519 signature over the SHA-256 of the package, hex |
| `<name>-<version>.json` | the registry entry for this package |

A module without code packs without `--wasm`. The release workflow of
`examples/example-module/.github/workflows/release.yml` packs and attaches the files to a GitHub
release on every tag `v*`; it needs the secret `I3CM_SIGNING_KEY` (for modules of i3cm-org the
organisation's key).

## Registry

The registry (`i3cm-org/i3cm-registry`, file `registry.json`) only locates packages; trust comes
from the operator's keys, never from the registry:

```json
{ "modules": [
  { "name": "example", "version": "0.6.0", "requires": ["base"],
    "sha256": "<hex of the package>",
    "package": "https://github.com/i3cm-org/i3cm-example-module/releases/download/v0.6.0/example-0.6.0.tar.gz",
    "signature": "https://…/example-0.6.0.tar.gz.sig",
    "key": "<hex public key, informative>" } ] }
```

`package` and `signature` are URLs or file paths. The installer refuses a package whose checksum
differs from the entry or whose signature matches none of the trusted keys (see below).

## Activation

A tenant has the base and the mandatory modules from its creation. Other installed modules are
activated by the tenant administrator: `POST /v1/modules/{name}/activate` checks that required
modules are active, creates the module's rooms, profiles and carriers, applies imprinting rules to
existing objects, all or nothing, and reports the new rights (`red` where they reach the root room
or types of other modules). Types of modules not active in a tenant do not exist for it.

## Installing (operator)

* From a file: `i3cm-server module install <name>-<version>.tar.gz [--trusted <hex key>]`
  (signature in `<file>.sig`).
* From the registry: `i3cm-server module install <name>[@<version>] --registry <url or file of
  registry.json>`; the package checksum must match the registry entry, the signature one of the
  trusted keys. `I3CM_REGISTRY_TOKEN` is sent for private release assets.
* Trusted keys: the key of i3cm-org built into the server (`deploy/keys/i3cm-org.pub`), plus the
  operator's own keys from `--trusted`, `I3CM_TRUSTED_KEYS` (comma-separated) and
  `I3CM_TRUSTED_KEYS_FILE` (one hex key per line, `#` comments). Own keys let an instance run
  self-signed modules, e.g. its own customisations.
* An instance started with `I3CM_ALLOW_DEVELOPMENT` also installs packages without a trusted
  signature (no `.sig`, or signed by an unknown key), with a warning in the log and in the result;
  the checksums of the package are checked all the same. Other instances refuse them.
* Over HTTP: with `I3CM_OPERATOR_TOKEN` (at least 32 characters) and `--modules-dir`, the server
  offers `GET /v1/operator/registry` and `POST /v1/operator/modules/install` (`{"name", "version"?}`),
  authenticated with that secret, separate from tenant tokens. Off without the secret.

Packages carry the module's sources (`src/`, the files under `i3cm/`); the installer puts them in
`<modules-dir>/src/<module>/i3cm/`. They serve `GET /v1/modules/{name}/sources` and source checks
of modules that depend on it (`POST /v1/catalog/check`, see api.md).

Installed modules are in force after the next start of the main container. Mandatory modules of
the image are bundled with `tools/bundle-modules.sh` (sources and refs in `deploy/modules/SOURCES`):
catalog part, UI templates, sources and, for modules with code, `<module>.wasm`, taken from the
module's release package when a fourth column names it (checked against its `SHA256SUMS`), else
built from `ops/` at the ref.
The kernel itself carries no business types; its own tests use the test module
`tests/modules/sample`.

