//! Self-description: an agent needs only the address and a token. All of
//! this is readable without a token and carries no data of any tenant.
//!
//! The route list below is the source of `/openapi.json`; a test checks that it names every
//! route of the router, so the description cannot fall behind the code.

use serde_json::{json, Map, Value};

/// (method, path, summary) for every route of the API.
pub const ROUTES: &[(&str, &str, &str)] = &[
    ("get", "/health", "Liveness"),
    ("get", "/", "Entry page (Markdown)"),
    ("get", "/guide/agents", "Guide for agents (Markdown)"),
    ("get", "/openapi.json", "This description"),
    ("get", "/skill.zip", "The agent guide as a skill"),
    ("get", "/v1/version", "Version and source revision of the server"),
    ("get", "/v1/catalog", "Types, references, aspects, profiles"),
    ("post", "/v1/orders", "Run an order (all operations or none)"),
    ("post", "/v1/objects", "Create an object"),
    ("get", "/v1/objects/{id}", "Read an object (readable type parts)"),
    ("patch", "/v1/objects/{id}", "Change attributes"),
    ("delete", "/v1/objects/{id}", "Delete an object"),
    ("post", "/v1/objects/{id}/types/{t}", "Attach a type"),
    ("delete", "/v1/objects/{id}/types/{t}", "Detach a type"),
    ("post", "/v1/objects/{id}/move", "Move to another room"),
    ("get", "/v1/objects/{id}/versions", "History"),
    ("get", "/v1/objects/{id}/ends/{end}", "Objects at a reference end"),
    ("get", "/v1/objects/{id}/content", "Binary content"),
    ("put", "/v1/objects/{id}/content", "Store binary content"),
    ("get", "/v1/objects/{id}/texts/{tt}", "A text"),
    ("put", "/v1/objects/{id}/texts/{tt}", "Store a text"),
    ("get", "/v1/objects/{id}/eval", "Evaluate a path expression at an object"),
    ("post", "/v1/references", "Link two objects"),
    ("delete", "/v1/references/{reference}/{id}", "Unlink"),
    ("get", "/v1/me", "The caller"),
    ("get", "/v1/me/groups", "The caller's groups"),
    ("get", "/v1/groups/{id}/members", "Members of a group"),
    ("get", "/v1/rooms", "Rooms"),
    ("get", "/v1/rooms/{id}/children", "Sub rooms"),
    ("get", "/v1/rooms/{id}/objects", "Objects in a room"),
    ("get", "/v1/rooms/{id}/path", "Path of a room"),
    ("get", "/v1/types/{t}/objects", "List objects of a type (paging: after= or offset=)"),
    ("get", "/v1/types/{t}/groups", "Counts per year, month or day"),
    ("get", "/v1/profiles", "Profiles (collections of rights)"),
    ("post", "/v1/assignments", "Assign a profile to a group or account"),
    ("get", "/v1/assignments", "Current profile assignments (`carrier` filters)"),
    ("delete", "/v1/assignments/{id}", "End a profile assignment (refused if it removes the last administrator)"),
    ("post", "/v1/tokens", "Issue a token"),
    ("get", "/v1/tokens", "List tokens"),
    ("delete", "/v1/tokens/{id}", "Revoke a token"),
    ("get", "/v1/modules", "Installed modules, whether active in the tenant, their operations"),
    ("post", "/v1/modules/{m}/activate", "Activate an installed module in the tenant (tenant administrator)"),
    ("post", "/v1/modules/{m}/ops/{op}", "Run an operation of a module as one order"),
    ("get", "/v1/modules/{m}/sources", "Sources of an installed module (i3cm/ files), if its package carries them"),
    ("post", "/v1/catalog/check", "Development tenant: check module sources with typecat and the catalog checker; all issues with positions"),
    ("get", "/v1/metrics", "Request counter"),
    ("get", "/v1/aspects", "Aspects (views) and built-in views"),
    ("get", "/v1/aspects/{a}/children", "Children of a node (paging: after= or offset=)"),
    ("get", "/v1/aspects/{a}/node", "Details of a node"),
    ("get", "/v1/aspects/{a}/path", "Chain of nodes from the root to a node"),
    ("get", "/v1/aspects/{a}/entry", "One entry with size, time and inode"),
    ("get", "/v1/aspects/{a}/index", "Index of a node (Markdown)"),
    ("put", "/v1/aspects/{a}/index", "Write the front matter of an object index"),
    ("get", "/v1/aspects/{a}/file", "A file of a node"),
    ("put", "/v1/aspects/{a}/file", "Write a file of a node (content or text)"),
    ("post", "/v1/aspects/{a}/drop", "New file in a folder with dropType: creates an object"),
    ("post", "/v1/aspects/{a}/files", "Several files at once"),
    ("get", "/v1/aspects/{a}/tab", "A tab of a node (json, csv, xlsx)"),
    ("post", "/v1/tools/exec", "A tool in shell form (argv)"),
    ("post", "/v1/tools/{tool}", "A tool, typed form"),
];

pub fn openapi() -> Value {
    let mut paths: Map<String, Value> = Map::new();
    for (m, p, summary) in ROUTES {
        let params: Vec<Value> = p
            .split('/')
            .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
            .map(|n| json!({"name": n, "in": "path", "required": true, "schema": {"type": "string"}}))
            .collect();
        let entry = paths.entry(p.to_string()).or_insert_with(|| json!({}));
        let open = matches!(*p, "/" | "/health" | "/guide/agents" | "/openapi.json" | "/skill.zip" | "/v1/version");
        let mut op = json!({"summary": summary, "parameters": params, "responses": {"200": {"description": "OK"}, "default": {"description": "Error: {error_id, description, params}"}}});
        if open {
            op["security"] = json!([]);
        }
        entry[*m] = op;
    }
    json!({
        "openapi": "3.1.0",
        "info": {"title": "i3cm base", "version": env!("CARGO_PKG_VERSION"), "x-revision": env!("I3CM_REV")},
        "components": {"securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}}},
        "security": [{"bearer": []}],
        "paths": paths,
    })
}

pub const ENTRY: &str = r#"# i3cm base

This is the API of an i3cm base: business objects with types, rooms and rights, shown in
views (aspects) and as files.

* For agents: [/guide/agents](/guide/agents) explains everything you need with only this
  address and a token; [/skill.zip](/skill.zip) is the same as a skill.
* The API: [/openapi.json](/openapi.json). Errors come as `{error_id, description, params}`.
* Version: [/v1/version](/v1/version).

Every call except these pages needs `Authorization: Bearer <token>`.
"#;

pub const GUIDE: &str = r#"# Working with an i3cm base (for agents)

You need the address of the server and a token. Keep them together; the token goes nowhere
but to this address and never into a file in a repository.

## 1. Check the token

    curl -H "Authorization: Bearer $TOKEN" $SERVER/v1/me

## 2. Look around: views and files

The data is shown in views (aspects). `GET /v1/aspects` lists them; `_id` and `_type` are
built in. A view is a tree: `GET /v1/aspects/{aspect}/children?node=` (empty node = root)
lists nodes and files; long lists are paged with `after=<last node id>` (or `offset=`).
Every node has an index, `GET /v1/aspects/{aspect}/index?node=`: Markdown with a front matter
of attributes and references as `[[links]]`.

## 3. The Unix tools

The same tree is available to Unix tools. Without installing anything, send a shell command:

    curl -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
      -d '{"cwd": "/", "argv": ["grep", "-rn", "invoice", "/mail:mailbox"]}' $SERVER/v1/tools/exec

The answer carries `stdout`, `stderr` and `status` as the real tool would print them.
Tools: ls, cat, head, tail, wc, stat, grep (-r -i -n -l -c -v -w -F -E -e -A -B -C), find
(-name -iname -path -type -maxdepth -mindepth -exec), tree (-L -d -a), sed (-e -n -E -i).
An option a tool does not know is an error listing the supported ones.

With the program `i3cm` (one binary): `i3cm init --server URL --tenant NAME` (token on
stdin) stores the access; `i3cm links DIR` creates links; with DIR first in PATH, `grep`,
`ls`, … on paths in a mount (`i3cm mount DIR`) go straight to the server, and `i3cm-grep`
etc. work on tree paths without a mount (working path: `i3cm-cd PATH` or `I3CM_PWD`).

## 4. Writing

Every write is one order: rights, required attributes and checks apply, all or nothing.

* Orders: `POST /v1/orders` with operations create, update, attach, detach, delete, move,
  link, unlink, content (binary content, base64) and text; objects created in the same order
  are referenced by their `ref`.
* Through views and files (also in a mount):
  - a file of a node (`PUT /v1/aspects/{a}/file?node=&name=`) writes the content or text
    behind it;
  - the index of an object (`PUT /v1/aspects/{a}/index?node=`): change attribute values in
    the front matter (`key: <JSON value>`); the fields `_id`, `_type`, `_room` and the body
    with the references stay as they are (references are changed with orders);
  - a new file in a folder whose view declares `dropType` creates an object
    (`POST /v1/aspects/{a}/drop?node=&name=`);
  - `sed -i` on files of the tree runs on the server as one order over all files.
* In a mount, a rejected write fails with an I/O error; the server's message is in
  `/.i3cm-errors` at the root of the mount.

## 5. Rights

You see and change only what the profiles of your account (and its groups) allow. Objects
you may not read do not exist for you: the answer is `not found`, never `forbidden`.

## 6. Modules

Business types come from modules. `GET /v1/modules` lists the installed ones, whether they are
active in your tenant and their operations; `POST /v1/modules/{name}/ops/{op}` runs an
operation as one order. Types and views of modules not active in your tenant do not exist
for you. In a development tenant, `POST /v1/catalog/check` checks module sources and lists
every issue with file, line and column.
"#;

/// The guide as a skill: a zip with `i3cm/SKILL.md` (stored, no compression).
pub fn skill_zip() -> Vec<u8> {
    let body = format!("---\nname: i3cm\ndescription: Work with an i3cm base through its API, the Unix tools and a FUSE mount (reading and writing).\n---\n\n{GUIDE}");
    zip_one("i3cm/SKILL.md", body.as_bytes())
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c ^= *b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

fn zip_one(name: &str, data: &[u8]) -> Vec<u8> {
    let crc = crc32(data);
    let n = name.as_bytes();
    let size = data.len() as u32;
    let mut out = vec![];
    let le16 = |v: u16| v.to_le_bytes();
    let le32 = |v: u32| v.to_le_bytes();
    // local file header
    out.extend(le32(0x0403_4b50));
    out.extend(le16(20));
    out.extend(le16(0x0800)); // UTF-8 names
    out.extend(le16(0)); // stored
    out.extend(le16(0));
    out.extend(le16(0x21));
    out.extend(le32(crc));
    out.extend(le32(size));
    out.extend(le32(size));
    out.extend(le16(n.len() as u16));
    out.extend(le16(0));
    out.extend(n);
    out.extend(data);
    let cd_start = out.len() as u32;
    // central directory
    out.extend(le32(0x0201_4b50));
    out.extend(le16(20));
    out.extend(le16(20));
    out.extend(le16(0x0800));
    out.extend(le16(0));
    out.extend(le16(0));
    out.extend(le16(0x21));
    out.extend(le32(crc));
    out.extend(le32(size));
    out.extend(le32(size));
    out.extend(le16(n.len() as u16));
    out.extend(le16(0));
    out.extend(le16(0));
    out.extend(le16(0));
    out.extend(le16(0));
    out.extend(le32(0o100644 << 16));
    out.extend(le32(0));
    out.extend(n);
    let cd_size = out.len() as u32 - cd_start;
    out.extend(le32(0x0605_4b50));
    out.extend(le16(0));
    out.extend(le16(0));
    out.extend(le16(1));
    out.extend(le16(1));
    out.extend(le32(cd_size));
    out.extend(le32(cd_start));
    out.extend(le16(0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_route_is_described() {
        let src = include_str!("lib.rs");
        let mut missing = vec![];
        for line in src.lines().map(str::trim).filter(|l| l.starts_with(".route(\"")) {
            let path = line.trim_start_matches(".route(\"").split('"').next().unwrap();
            let methods: Vec<&str> = ["get", "post", "put", "patch", "delete"].into_iter().filter(|m| line.contains(&format!("{m}("))).collect();
            for m in methods {
                if !ROUTES.iter().any(|(rm, rp, _)| *rm == m && *rp == path) {
                    missing.push(format!("{m} {path}"));
                }
            }
        }
        assert!(missing.is_empty(), "routes without description: {missing:?}");
    }

    #[test]
    fn crc_and_zip() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let z = skill_zip();
        assert_eq!(&z[0..4], &[0x50, 0x4b, 0x03, 0x04]);
    }

    #[test]
    fn api_handlers_only_call_the_store() {
        // structural test (proposal one-operation-many-surfaces): no SQL and no database
        // access in the API surface; every action is an operation of the store
        let src = include_str!("lib.rs");
        for bad in ["tokio_postgres", "SELECT ", "INSERT ", "UPDATE ", ".query("] {
            assert!(!src.contains(bad), "API surface contains {bad:?}");
        }
    }
}
