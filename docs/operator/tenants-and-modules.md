# Tenants and modules

## Tenants

Many tenants share one database and one main container; every record carries its tenant, and the
separation is done by the base alone (modules know nothing of tenants).

```sh
i3cm-server init-tenant <name> [--admin <login>]
```

creates the root room, the profiles of the base and the mandatory modules, a group of administrators,
the login (default `admin`) and prints a start token valid for one day. The mandatory modules `tasks`
and `dms` are active in every tenant from its creation.

### Development tenants

The studio and other tools for module authors run only in development tenants. The operator allows them
per instance:

```sh
I3CM_ALLOW_DEVELOPMENT=true i3cm-server init-tenant acme-dev --development
```

A module that declares `developmentOnly: true` can only be activated in such a tenant. The flag is a
convenience, not a security boundary: run development tenants on an instance of their own when the
business data matters.

## Installing modules

Installing is the operator's job and applies to the whole instance; activating is the tenant
administrator's job and applies to one tenant ([../tenant-admin/](../tenant-admin/README.md)).

* From a package file: `i3cm-server module install <name>-<version>.tar.gz --trusted <hex key>`; the
  signature is read from `<file>.sig`.
* From the registry: `i3cm-server module install <name>[@<version>] --registry <url or file>`; the
  checksum must match the registry entry and the signature one of the trusted keys
  (`I3CM_TRUSTED_KEYS`). Set `I3CM_REGISTRY_TOKEN` for private release assets.

The installer checks the package, its catalog part against the installed catalog, creates the tables of
the module (all or nothing) and puts `module.json`, `module.wasm`, texts, UI templates and sources into
the modules directory. The module is in force after the next start of the main containers; then tenants
can activate it. The package format and signatures are described in
[../developer/modules.md](../developer/modules.md).

The UI frame loads `modules/<ns>.html` for every active namespace; with the compose file for the UI, copy
the templates of installed modules into the frame's `ui/modules/` directory (`typecat … --ui-out`).

## The operator surface over HTTP

For operators without a shell, the server offers two calls when `I3CM_OPERATOR_TOKEN` (at least 32
characters) and a modules directory are set:

| Call | Purpose |
|---|---|
| `GET /v1/operator/registry` | entries of the configured registry |
| `POST /v1/operator/modules/install` | `{"name", "version"?}`: install from the registry |

Both are authenticated with `Authorization: Bearer <operator token>`, separate from tenant tokens. Without
the variable the surface does not exist.
