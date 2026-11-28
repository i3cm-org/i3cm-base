# Operator guide

The operator runs an i3cm instance: one database, one or more main containers, the installed
modules. Tenants and their administrators are described in [../tenant-admin/](../tenant-admin/README.md).

| Page | Content |
|---|---|
| this page | installation, first tenant, upgrades |
| [configuration.md](configuration.md) | every setting, where it comes from, defaults |
| [tenants-and-modules.md](tenants-and-modules.md) | tenants, development tenants, installing modules, the operator surface |
| [tokens-backup-fuse.md](tokens-backup-fuse.md) | tokens from the command line, backups, FUSE and containers |

## Install

The image contains the server (`i3cm-server`), the agent program (`i3cm`), the catalog of the
kernel, Node and `typecat` (for source checks) and the mandatory modules `tasks` and `dms`.

```sh
docker compose -f deploy/compose.yaml up -d --build
docker compose -f deploy/compose.yaml exec server i3cm-server init-tenant acme
```

The server runs as user `i3cm` (uid 10001). Installed modules and contents are state: the compose files
keep the modules directory on the volume `modules` (`/var/lib/i3cm/modules`) and the file store on the
volume `data` (`/var/lib/i3cm/data`), so both survive a new container. When you run the image
yourself, mount volumes there that uid 10001 can write; the server refuses to start otherwise. A volume created earlier by root (or restored from a backup) gets the
right owner with `docker run --rm -v <volume>:/d debian chown -R 10001:10001 /d`. The image has a health check (`i3cm-server health`, which asks the
local `GET /health`); compose restarts the server unless it was stopped.

`init-tenant` prints a start token for the login `admin` that is valid for one day; use it to issue
lasting tokens (see [tokens-backup-fuse.md](tokens-backup-fuse.md)). The server listens on port 8480.
TLS belongs to a reverse proxy in front of it.

`deploy/compose.ui.yaml` adds the browser UI: a static file server (nginx) delivers `ui/` and passes
`/v1`, `/` (self-description) and the guide to the server under the same origin.

Without Docker, build with `cargo build --release -p i3cm-server -p i3cm-cli` and the catalog with
`make catalog`, then run `i3cm-server serve` with the settings from [configuration.md](configuration.md).

## Development instance

`cargo run -p i3cm-server -- dev` (or `make dev`) starts an embedded PostgreSQL in `.pgdata`, a tenant
`dev` and the API on `127.0.0.1:8480`, and prints an admin token. Not for production.

## Upgrades

A main container serves one version (several main containers of different versions can share a
database within one major version). The schema migrates itself on start; migrations run only when the
schema version changes. Installed modules are in force after the next start.

The mandatory modules come with the image. At start the server copies them into the modules directory
when they are missing there, or when the copy there came from an older image (all files of the module
are compared); a version the operator installed over them, or a copy that did not come from an image,
is kept (the log says so: `bundled modules: …`). A new image therefore updates the
mandatory modules without touching modules you installed.

Upgrading from a version that kept contents in the database (before the file store): add the `data`
volume, start the new version (it reads old contents from the database), then move them once with
`docker compose -f deploy/compose.yaml exec server i3cm-server migrate-blobs` (prints `{"moved": n}`;
safe to repeat and to run while the server serves).

After an upgrade, compare `GET /v1/version` with the version and revision you meant to deploy; the start
log ends with a line `ready` that names address, version and revision.

## Checking an instance

* `GET /health` — liveness (also `i3cm-server health`, exit code 0 when healthy); `GET /v1/version` — version and source revision (also `i3cm-server --version`).
* `GET /v1/metrics` — requests served by this process (useful to measure clients such as FUSE).
* `i3cm-server catalog check [catalog.json]` — checks a catalog with the server's checker, no database.
