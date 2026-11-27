# Configuration

## Server (`i3cm-server`)

The server has no configuration file. Every value comes from a command line option or, where named,
from an environment variable; the option wins over the variable, and a value missing from both takes
the default. The image sets the variables marked "image".

| Option | Variable | Default | Meaning |
|---|---|---|---|
| `--db` | `I3CM_DATABASE_URL` | — (required) | PostgreSQL URL |
| `--catalog` | `I3CM_CATALOG` | `catalog.json` at the root of the source tree the server was built from; image: `/etc/i3cm/catalog.json` | catalog of the kernel, built by `typecat` |
| `--modules-dir` | `I3CM_MODULES_DIR` | none (module code, hooks and schedules off); image: `/opt/i3cm/modules` | installed modules: `<module>.json`, `<module>.wasm`, `src/` |
| `serve --listen` | `I3CM_LISTEN` | `0.0.0.0:8480` | address of the API |
| `serve --pool` | — | `32` | database connections |
| `module install --registry` | `I3CM_REGISTRY` | — | registry (URL or file of `registry.json`) |
| `module install --trusted` | `I3CM_TRUSTED_KEYS` | — | further trusted ed25519 public keys (hex), comma-separated; added to the built-in key of i3cm-org |
| — | `I3CM_TRUSTED_KEYS_FILE` | — | file with further trusted keys, one hex key per line, `#` comments |
| — | `I3CM_REGISTRY_TOKEN` | — | bearer token for private release assets when fetching packages |
| — | `I3CM_OPERATOR_TOKEN` | — (surface off) | secret of the operator surface over HTTP, at least 32 characters |
| — | `I3CM_ALLOW_DEVELOPMENT` | off | `true`, `1` or `yes`: development tenants may be created, and module packages without a trusted signature are installed (with a warning) |
| — | `I3CM_NODE` | `node` | Node for source checks |
| — | `I3CM_TYPECAT` | image: `/opt/i3cm/typecat/typecat.mjs` | `typecat` for source checks |
| — | `I3CM_TYPES_DIR` | image: `/opt/i3cm/types` | type sources of the kernel for source checks |
| — | `I3CM_TIMEZONE` | UTC | fixed offset for `TIME()` in display texts, e.g. `+01:00` |
| — | `RUST_LOG` | `info` | log filter; logs go to stderr |

Build only: `I3CM_REV` (`docker build --build-arg I3CM_REV=$(git rev-parse --short=12 HEAD)`) stamps the
source revision into the binaries.

## Agent program (`i3cm`)

The program has a fixed rank of sources: the environment first, then the files written by `i3cm-init`
in `$XDG_CONFIG_HOME/i3cm/` (default `~/.config/i3cm/`). When a higher source covers a lower one, the
program says so on stderr. `i3cm config` prints every value with its source; the token itself is never
shown.

| Value | Variable | File |
|---|---|---|
| server | `I3CM_SERVER` | `config.toml` |
| tenant | `I3CM_TENANT` | `config.toml` |
| token | `I3CM_TOKEN_FILE` (path of a file holding the token) | `token` (0600) |
| language of display names | `I3CM_LANG` | `config.toml` (from `LANG` at `i3cm-init`) |
| working path without a mount | `I3CM_PWD` | state file of `i3cm-cd` |

## Packaging (module authors)

`I3CM_SIGNING_KEY` holds the ed25519 private key (PEM) for `tools/typecat/pack.mjs`, see
[../developer/modules.md](../developer/modules.md). `I3CM_MODULES` (colon-separated module repos) is read by
`make catalog`.

## Tests

`I3CM_TEST_DATABASE_URL` runs the integration tests against an existing database instead of the embedded
one; `I3CM_TEST_CATALOG` selects the catalog for them.
