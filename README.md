# i3cm-base

The base of i3cm, a system for function portals (interoperable, integrated infrastructure and
content management): typed objects that carry several types at once, versioned
storage, rooms and roles for rights, and a REST API. Modules contribute types, rooms, roles
and display texts. This is milestone 0.4: the core without user interface, views (aspects), and the data as a
read-only file system with Unix tools for agents.

## Concepts in one paragraph

An **object** carries several **types** at the same time (for example `Person` and
`PersonAccount`); each type contributes its own attributes, stored in a table of its own.
Types are related by inheritance and by **rules** (a type *requires* another, *brings* another
along, or *imprints* itself on another). Objects are connected by **references** with two
named ends and cardinalities; "part of" references make parts follow their whole. Every change
creates a new version; only the changed type parts are rewritten. Every object lives in one
**room**; **roles** (collections of rights with sign `+`, `-` or `block`) are assigned to groups
or accounts, and rights are inherited down the room tree.

## Layout

| Path | Content |
|---|---|
| `types/` | Type definitions in TypeScript (`base` and the example module `fibu`), Fluent texts |
| `tools/typecat/` | Reads `types/` with the TypeScript compiler API and writes `catalog.json` |
| `crates/i3cm-core` | Catalog model, type checker, value types, rights evaluation, display names |
| `crates/i3cm-store` | PostgreSQL storage: schema from the catalog, orders, versions, reads |
| `crates/i3cm-api` | REST API (axum) |
| `crates/i3cm-server` | Server binary, embedded PostgreSQL for development and tests |
| `crates/i3cm-client` | Rust client |
| `crates/i3cm-tools` | The read-only Unix tools (`ls`, `cat`, `head`, `tail`, `wc`, `stat`, `grep`, `find`): parser, engine, output |
| `crates/i3cm-cli` | The program `i3cm`: FUSE driver, the tools by their Unix names, `i3cm-init` |
| `crates/i3cm-loadtest` | Load test with the accounting example |
| `ui/` | Browser test app: static HTML with Fluent 2 Web Components, data only through the API |
| `deploy/` | Dockerfile and compose files (server; server with UI) |
| `docs/developer/` | Type notation, API reference, tools and file system, browser UI, module repos ([modules.md](docs/developer/modules.md)) |

## Quick start

Requirements: Rust (stable), Node.js 22. PostgreSQL is downloaded and run embedded for
development and tests.

```sh
make catalog          # types/ -> catalog.json
make test             # typecat tests, unit tests, integration tests against PostgreSQL
cargo run -p i3cm-server -- dev     # embedded PostgreSQL, tenant "dev", prints an admin token
make loadtest         # 1000 accounts, 45 accountants, 100,000 journal entries, navigation
```

Production-like: `docker compose -f deploy/compose.yaml up --build`, then
`i3cm-server init-tenant <name>` inside the server container.

## License

Apache-2.0
