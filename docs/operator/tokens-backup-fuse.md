# Tokens, backups, FUSE

## Tokens from the command line

```sh
i3cm-server create-token <tenant> <login> [--ttl <days>]
```

prints a token and its expiry (default 90 days, at most 365). Tenant administrators issue and revoke
tokens over the API ([../tenant-admin/](../tenant-admin/README.md)). Only hashes are stored. A token unused
for 90 days lapses (`auth.unused_token`); expired and revoked tokens answer `auth.expired_token` and
`auth.revoked_token`.

## Backups

Objects, versions and texts live in the database; binary contents live in the file store of the data
directory (`I3CM_DATA_DIR`, files named by their SHA-256; older contents can still be in the database
table `blob` until `i3cm-server migrate-blobs` has moved them). A complete backup of the instance is a
consistent `pg_dump` of the database, a copy of the data directory's `blobs/` (taken after the dump:
files are only ever added, so a later copy contains every file the dump refers to) and a copy of the
modules directory (installed packages and their sources). The staging area (`staging/`, `tmp/`) holds
unassigned uploads and need not be saved. Restore the modules directory and `blobs/` first, then the
database, then start the server with the same catalog and modules.

## FUSE and containers

The program `i3cm` mounts the data as a file system (`i3cm-mount <dir>`). That needs `/dev/fuse`:

* on a normal host or VM: install `fuse3`;
* in Docker: `--device /dev/fuse --cap-add SYS_ADMIN`;
* in an unprivileged Proxmox LXC container: enable the feature `fuse=1` for the container, then restart it.

Without FUSE, the same tools work on tree paths with the `i3cm-` prefix (`i3cm-ls /dms:filing`,
`i3cm-grep -rn …`), with `i3cm-cd` or `I3CM_PWD` as the working path. Agents can also use the tools over
HTTP without installing anything (`POST /v1/tools/exec`, see [../developer/tools.md](../developer/tools.md)).
