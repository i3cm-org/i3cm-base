# Tools and file system

Agents work with the data as with files: one program, `base`, is the read-only FUSE driver
and the Unix tools at the same time. What it does depends on the name it is called by.

## Setup

```sh
base links ~/.local/base-bin                      # links: grep, ls, …, base-grep, …, base-init, base-mount
export PATH="$HOME/.local/base-bin:$PATH"         # first in PATH, in addition to the system tools
base-init --server https://erp.example.org --tenant acme < token.txt
base-mount ~/erp &                                # optional: needs /dev/fuse
```

`base-init` checks the token (`GET /v1/me`) and writes `$XDG_CONFIG_HOME/base-erp/config.toml`
(default `~/.config/base-erp/`, directory 0700, file 0600) with server, tenant, token and the
language of display names (from `LANG` or `--lang`). The token is read from stdin or
`--token-file`, never from the command line. `BASE_SERVER`, `BASE_TENANT` and
`BASE_TOKEN_FILE` override the file. A token for an agent comes from `POST /v1/tokens` (see
the API reference).

## Layout

The mount (and the tree the tools see) has one directory per aspect, with its index beside it:

```
/_id/<uuid>/…            every object by its id (the directory lists nothing)
/_type/base:Mail/…        objects by type
/base:mailbox/2026-11-24/anna@example.org - Angebot/
                          mail.eml  mail.md  Anhänge/
/base:mailbox/2026-11-24/anna@example.org - Angebot.md     the object file (index)
```

A node of an aspect is a directory `<name>/` with its index `<name>.md` beside it; a file is a
file. Names are display names with `/` replaced by `_`; equal names in one directory get
` ~2`, ` ~3`. Everything is read-only. A file of an object shows the same inode in every view
(content and texts) and `nlink` 2, so `find -samefile`, `du` and `tar` see one file.

The driver fetches a directory completely (paged internally) and reads the small files of a
directory in one request: a program reading recursively costs requests per directory, not per
file. Kernel caches hold entries for `--ttl` seconds (default 5).

## Tools

`ls`, `cat`, `head`, `tail`, `wc`, `stat`, `grep`, `find`. Called by their plain names (links
first in `PATH`), they check whether the paths lie in a base-erp mount (file system type
`fuse.base-erp`):

* yes: the tool calls the server directly, one request per call, and prints like GNU;
* no: it hands over to the next program of that name in `PATH`;
* both mixed: error.

As `base-<tool>` (or `base <tool>`) they work without a mount on tree paths such as
`/base:mailbox/2026-11-24`, relative to `BASE_PWD` (default `/`), for example in containers
without FUSE.

Options (anything else is an error that lists these, never a silent fallback):

| Tool | Options |
|---|---|
| `ls` | `-l -a -1 -R`, `--all`, `--recursive` |
| `cat`, `stat` | none |
| `head` | `-n N`, `-c N`, `-N`, `--lines=N`, `--bytes=N` |
| `tail` | as `head`, `+N` counts from the start |
| `wc` | `-l -w -c`, `--lines`, `--words`, `--bytes` |
| `grep` | `-r -R -i -n -l -c -v -w -F -E -e PATTERN` and the long forms |
| `find` | `-name -iname -path -type f\|d -maxdepth -mindepth -print`, one `-exec CMD {} ;` or `+` |

`grep -r` searches the texts of objects and the indexes, not binary content (a text type can be
excluded with `@texts("ocr.txt:text/plain:noGrep")`). A file named on the command line is always
searched. A trigram index (PostgreSQL extension `pg_trgm`) prefilters literal patterns; without
the extension the prefilter scans the texts. `find -exec` runs our tools on the server (`;`
per file, `+` once); another program runs locally per file through the mount. Standard input
is not supported; `grep` without files hands over to the system tool.

## API

The tools are reachable without the program:

```
POST /v1/tools/grep   {"cwd": "/base:mailbox", "paths": ["."], "options": {"recursive": true, "patterns": ["Rechnung"], "lineNumber": true}}
POST /v1/tools/exec   {"cwd": "/base:mailbox", "argv": ["grep", "-rn", "Rechnung", "."]}
```

Both answer `{"status": 0, "records": [...]}` with records such as
`{"kind": "match", "path": "./2026-11-24/…/mail.md", "line": 3, "text": "…"}` (`listing`,
`data` with base64, `counts`, `stat`, `binary`, `name`, `count`, `error`); `exec` adds `stdout`
and `stderr` as the tool prints them. Option names of the typed form: `long`, `all`,
`recursive`, `lines`, `bytes`, `fromStart`, `countLines`, `countWords`, `countBytes`,
`patterns`, `ignoreCase`, `lineNumber`, `filesWithMatches`, `count`, `invert`, `word`, `fixed`,
`extended`, `tests`, `maxdepth`, `mindepth`, `exec`. `GET /v1/metrics` counts requests.

## Limits

Links in an index use the plain names; when two entries of a directory share a name, the link
reaches the first. A directory cannot be a hard link in Linux: directories of the same object
in different places have different inodes; their files share them. Loops in a view (an object
below itself, possible in `_type`) are reported by `find` and skipped by `grep -r` and `ls -R`.
