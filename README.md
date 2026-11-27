# i3cm-base

The kernel of i3cm, a base for function portals (interoperable, integrated infrastructure and content
management): business data that stays reliable, opened to agents that stay flexible. Typed objects carry
several types at once, every change is versioned, rooms and profiles decide who may do what, and one API
serves the browser UI, the file system view, the Unix tools and agents alike. Business functions come
from modules; the mandatory modules `tasks` and `dms` are delivered with the kernel.

This is milestone 0.7.1: kernel with views, rights paths, the data as a file system with Unix tools,
modules as signed WebAssembly packages with installer and activation per tenant, development tenants
and the studio support, and a self-description for agents at `/guide/agents`.

## Documentation

| For | Start here |
|---|---|
| Everyone | [docs/architecture.md](docs/architecture.md): how the parts fit together |
| Operators | [docs/operator/](docs/operator/README.md): installation, configuration, tenants, installing modules, tokens, backups, FUSE |
| Tenant administrators | [docs/tenant-admin/](docs/tenant-admin/README.md): activating modules, groups, profiles, rights paths, tokens for agents |
| Module developers | [docs/developer/types.md](docs/developer/types.md) (type notation and views), [modules.md](docs/developer/modules.md) (module repos, WebAssembly code, packaging, registry), [ui.md](docs/developer/ui.md) (browser UI) |
| Integrators and agents | [docs/developer/api.md](docs/developer/api.md) (REST API), [tools.md](docs/developer/tools.md) (tools and file system); a running server describes itself at `/`, `/guide/agents` and `/openapi.json` |

## Concepts in one paragraph

An **object** carries several **types** at the same time (for example `Person` and `PersonAccount`); each
type contributes its own attributes, stored in a table of its own. Types are related by inheritance and by
**rules** (a type *requires* another, *brings* another along, or *imprints* itself on another). Objects are
connected by **references** with two named ends and cardinalities; "part of" references make parts follow
their whole. Every change creates a new version; only the changed type parts are rewritten. Every object
lives in one **room**; **profiles** (collections of rights with sign `+`, `-` or `block`, optionally tied
to the data by a rights path) are assigned to groups or accounts and inherited down the room tree.
**Views** (aspects) arrange objects as trees for the UI, the file system and the tools.

## Layout

| Path | Content |
|---|---|
| `types/` | Kernel types in TypeScript (`base`, the notation library `lib`, the example module `fibu`), Fluent texts |
| `tools/typecat/` | Type compiler (TypeScript compiler API → `catalog.json`), source checks, `pack.mjs` (signed packages) |
| `tools/bundle-modules.sh` | Bundles the mandatory modules into `deploy/modules` |
| `wit/` | WIT world `i3cm:module` for module code |
| `crates/i3cm-core` | Catalog model, checker, value types, rights, path language, display texts |
| `crates/i3cm-store` | PostgreSQL storage: schema, orders, versions, reads, views, rights paths |
| `crates/i3cm-wasm` | Module runtime (wasmtime): operations, hooks, schedules |
| `crates/i3cm-api` | REST API (axum) and self-description |
| `crates/i3cm-server` | Server binary: serve, tenants, tokens, catalog check, module installer, operator surface |
| `crates/i3cm-client` | Rust client |
| `crates/i3cm-tools` | The Unix tools on the data: parser, engine, output |
| `crates/i3cm-cli` | The program `i3cm`: FUSE driver, tools by their Unix names, `i3cm-init`, `i3cm studio`, `i3cm scaffold` |
| `crates/i3cm-loadtest` | Load test with the accounting example |
| `examples/example-module` | A complete module with Rust code, hook, schedule and release workflow |
| `ui/` | Browser UI: static frame with Fluent 2 Web Components, data only through the API |
| `deploy/` | Dockerfile, compose files, bundled mandatory modules |
| `docs/` | Documentation (table above) |

## Quick start

Requirements: Rust (stable), Node.js 22. PostgreSQL is downloaded and run embedded for development and
tests.

```sh
make catalog          # types/ -> catalog.json
make test             # typecat tests, unit tests, integration tests against PostgreSQL
make doccheck         # documented endpoints, options and variables exist in the code
cargo run -p i3cm-server -- dev     # embedded PostgreSQL, tenant "dev", prints an admin token
make loadtest         # 1000 accounts, 45 accountants, 100,000 journal entries, navigation
```

Production-like: `docker compose -f deploy/compose.yaml up --build`, then
`i3cm-server init-tenant <name>` inside the server container (see the operator guide).

## License

Apache-2.0
