# Tools and file system

Agents work with the data as with files: one program, `i3cm`, is the FUSE driver (reading
and writing) and the Unix tools at the same time. What it does depends on the name it is called by.

## Setup

```sh
i3cm links ~/.local/i3cm-bin                      # links: grep, ls, …, sort, uniq, cut, i3cm-grep, …, i3cm-init, i3cm-mount, i3cm-cd
export PATH="$HOME/.local/i3cm-bin:$PATH"         # first in PATH, in addition to the system tools
i3cm-init --server https://erp.example.org --tenant acme < token.txt
i3cm-mount ~/erp &                                # optional: needs /dev/fuse
```

`i3cm-init` checks the token (`GET /v1/me`) and writes two files to
`$XDG_CONFIG_HOME/i3cm/` (default `~/.config/i3cm/`, directory 0700): `config.toml`
with server, tenant and the language of display names (from `LANG` or `--lang`), and `token`
(0600) with the token alone. The token is read from stdin or `--token-file`, never from the
command line. A token for an agent comes from `POST /v1/tokens` (see the API reference).

Sources have a fixed rank: the environment (`I3CM_SERVER`, `I3CM_TENANT`, `I3CM_TOKEN_FILE`)
first, then the files. The higher source wins; when it covers a value of a lower one, the
program says so on stderr (`i3cm: token from I3CM_TOKEN_FILE covers the one in …/token`).
`i3cm config` prints every setting with its source (the token itself is never shown);
`i3cm --version` prints the version and the source revision (also `GET /v1/version`).

## Layout

The mount (and the tree the tools see) has one directory per aspect, with its index beside it:

```
/_id/<uuid>/…            every object by its id (the directory lists nothing)
/_type/mail:Mail/…        objects by type
/mail:mailbox/2026-11-24/anna@example.org - Angebot/
                          mail.eml  mail.md  Anhänge/
/mail:mailbox/2026-11-24/anna@example.org - Angebot.md     the object file (index)
```

A node of an aspect is a directory `<name>/` with its index `<name>.md` beside it; a file is a
file. Names are display names with `/` replaced by `_`; equal names in one directory take the
fallback name of their aspect's name rule if it has one (mailbox: with the time received), and
names that still collide get ` ~2`, ` ~3` in object id order, so a name always opens the same
object. A file of an object shows the same inode in every view
(content and texts) and `nlink` 2, so `find -samefile`, `du` and `tar` see one file.

The driver fetches a directory completely (paged internally) and reads the small files of a
directory in one request: a program reading recursively costs requests per directory, not per
file. Kernel caches hold entries for `--ttl` seconds (default 5).

## Writing

A file written in the mount is sent when it is closed (or flushed): the content or text behind
it, or, for an object's index `<name>.md`, the changed values in its front matter. Each send is
one order on the server, so rights and checks apply as always. A file copied into a folder
whose view declares `dropType` creates an object. A rejected write fails with an I/O error on
`fsync` or `close` (many programs ignore errors on close; then only the log shows it),
the server's message is appended to `/.i3cm-errors` at the root of the mount. Editors that
save by writing a new file and renaming it are not supported; write in place (`>`, `cp`,
`sed -i`, `tee`).

`sed -i` on files of the mount runs on the server (GNU sed in sandbox mode: no `e`, `r`, `w`)
as **one order over all files**: all or nothing.

## Tools

`ls`, `cat`, `head`, `tail`, `wc`, `stat`, `grep`, `find`, `tree`, `sed`. Called by their plain names (links
first in `PATH`), they check whether the paths lie in a i3cm mount (file system type
`fuse.i3cm`):

* yes: the tool calls the server directly, one request per call, and prints like GNU;
* no: it hands over to the next program of that name in `PATH`;
* both mixed: error.

As `i3cm-<tool>` (or `i3cm <tool>`) they work without a mount on tree paths such as
`/mail:mailbox/2026-11-24`, relative to the working path: `I3CM_PWD`, else the path set with
`i3cm-cd <path>` for this shell session (kept in `$XDG_STATE_HOME/i3cm/pwd-<session>`), else
`/`; for example in containers without FUSE.

`sort`, `uniq` and `cut` run locally as usual (the system programs, all their options): files in
a mount, or tree paths for `i3cm-sort` etc., are fetched in one request and given to the
program on standard input.

Options (anything else is an error that lists these, never a silent fallback):

| Tool | Options |
|---|---|
| `ls` | `-l -a -1 -R`, `--all`, `--recursive` |
| `cat`, `stat` | none |
| `head` | `-n N`, `-c N`, `-N`, `--lines=N`, `--bytes=N` |
| `tail` | as `head`, `+N` counts from the start |
| `wc` | `-l -w -c`, `--lines`, `--words`, `--bytes` |
| `grep` | `-r -R -i -n -l -c -v -w -F -E -e PATTERN -A N -B N -C N` and the long forms |
| `find` | `-name -iname -path -type f\|d -maxdepth -mindepth -print`, one `-exec CMD {} ;` or `+` |
| `tree` | `-L N -d -a` |
| `sed` | `-e SCRIPT -n -E -r -i` and the long forms |

`grep -r` searches the texts of objects and the indexes, not binary content (a text type is
excluded with the class decorator `@noGrep("ocr.txt")`). A file named on the command line is always
searched. A trigram index (PostgreSQL extension `pg_trgm`) prefilters literal patterns; without
the extension the prefilter scans the texts. `find -exec` runs our tools on the server (`;`
per file, `+` once); another program runs locally per file through the mount. Standard input
is not supported; `grep` without files hands over to the system tool.

## Studio: module repository and development tenant

In a module repository (recognised by `i3cm/module.ts`, searched from the working directory
upwards) the program copies the declarative part, the files below `i3cm/`, to and from a
development tenant. There it is a `studio:ModuleSource` with one `studio:SourceFile` per file
(path, content, and the SHA-256 of the disk state at the last load or save).

```sh
i3cm studio status        # per file: same, changed in tenant, changed on disk, changed on both sides,
                          # new in tenant, deleted on disk, only on disk
i3cm studio load          # make the tenant equal to the disk
i3cm studio save          # make the disk equal to the tenant (only below i3cm/, never ops/)
```

There is no merging; versions, branches and merges are git's. `load` stops (exit code 3) when it
would throw away changes made in the tenant since the last load or save, `save` when it would
throw away changes made on disk; `--force` goes on. Hidden files and `node_modules` are not
copied. `load` writes structure, contents and hashes in one order (all or nothing).

## Scaffolds

```sh
i3cm scaffold ops --lang rust    # ops/: WebAssembly component against the kernel's WIT world, with a test
i3cm scaffold ui                 # i3cm/ui/<module>.html: one template per aspect and node type
```

Existing files are not overwritten without `--force`. The WIT file is the one the program was
built with (`wit/module.wit` of the kernel).

## API

The tools are reachable without the program:

```
POST /v1/tools/grep   {"cwd": "/mail:mailbox", "paths": ["."], "options": {"recursive": true, "patterns": ["Rechnung"], "lineNumber": true}}
POST /v1/tools/exec   {"cwd": "/mail:mailbox", "argv": ["grep", "-rn", "Rechnung", "."]}
```

Both answer `{"status": 0, "records": [...]}` with records such as
`{"kind": "match", "path": "./2026-11-24/…/mail.md", "line": 3, "text": "…"}` (`listing`,
`data` with base64, `counts`, `stat`, `binary`, `name`, `count`, `context`, `separator`,
`treeLine`, `treeSummary`, `written`, `error`); `exec` adds `stdout`
and `stderr` as the tool prints them. Option names of the typed form: `long`, `all`,
`recursive`, `lines`, `bytes`, `fromStart`, `countLines`, `countWords`, `countBytes`,
`patterns`, `ignoreCase`, `lineNumber`, `filesWithMatches`, `count`, `invert`, `word`, `fixed`,
`extended`, `afterContext`, `beforeContext`, `tests`, `maxdepth`, `mindepth`, `exec`, `level`,
`dirsOnly`, `scripts`, `inPlace`, `quiet`. `GET /v1/metrics` counts requests.

## Limits

Two programs writing the same file at the same time share one buffer, as with a local file;
the content sent is what the file holds when one of them closes it.

A directory cannot be a hard link in Linux: directories of the same object
in different places have different inodes; their files share them. Loops in a view (an object
below itself, possible in `_type`) are reported by `find` and skipped by `grep -r` and `ls -R`.
