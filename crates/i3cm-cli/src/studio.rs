//! `i3cm studio load|save|status`: the declarative part of a module repository (the files below
//! `i3cm/`) and its copy in a development tenant, kept as `studio:ModuleSource` with parts
//! `studio:SourceFile` (path, content, hash of the disk state at the last load or save).
//!
//! There is no merging: `load` makes the tenant equal to the disk (one order), `save` makes the disk equal to
//! the tenant. Each refuses (unless `--force`) when it would throw away changes made on the other
//! side since the last load or save; git does versions, branches and merges.

use crate::api::Api;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const CONTAINER_TYPE: &str = "studio:ModuleSource";
pub const FILE_TYPE: &str = "studio:SourceFile";
pub const FILE_END: &str = "files";
pub const FILE_REFERENCE: &str = "studio:SourceFile.module";
pub const ROOM: &str = "studio";
/// Directory of the declarative part inside a module repository.
pub const SOURCE_DIR: &str = "i3cm";

/// Hex SHA-256 of some bytes.
pub fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The repository root: the first directory from `start` upwards that holds `i3cm/module.ts`.
pub fn find_repo(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join(SOURCE_DIR).join("module.ts").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// The module name from `export const module = { name: "…", … }` in `module.ts`, read without
/// running the file.
pub fn module_name(module_ts: &str) -> Option<String> {
    let start = module_ts.find("export const module")?;
    let rest = &module_ts[start..];
    let open = rest.find('{')?;
    let body = &rest[open..rest[open..].find('}').map(|i| open + i)?];
    let at = body.find("name")?;
    let after = body[at + 4..].trim_start().strip_prefix(':')?.trim_start();
    let quote = after.chars().next().filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let value = &after[1..];
    let end = value.find(quote)?;
    Some(value[..end].to_string()).filter(|s| !s.is_empty())
}

/// All files below `<repo>/i3cm/` as relative paths with `/` separators, without hidden files
/// and directories and without `node_modules`; sorted.
pub fn disk_files(repo: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let root = repo.join(SOURCE_DIR);
    let mut out = BTreeMap::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for e in entries {
            let e = e.map_err(|e| e.to_string())?;
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            let p = e.path();
            let t = e.file_type().map_err(|e| e.to_string())?;
            if t.is_dir() {
                stack.push(p);
            } else if t.is_file() {
                let rel = p.strip_prefix(&root).map_err(|e| e.to_string())?.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                out.insert(rel, std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?);
            }
        }
    }
    Ok(out)
}

/// A file as the tenant holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct TenantFile {
    pub id: String,
    pub path: String,
    pub content: Vec<u8>,
    /// Hash of the disk state at the last load or save (None: never synchronised).
    pub disk_hash: Option<String>,
}

/// State of one path, comparing tenant and disk with the hash of the last load or save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Same,
    ChangedInTenant,
    ChangedOnDisk,
    ChangedBoth,
    /// Only in the tenant: new there (`known` false) or deleted on disk since the last sync.
    OnlyTenant { known: bool },
    /// Only on disk: new there (or deleted in the tenant; the tenant keeps no trace of that).
    OnlyDisk,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Same => "same",
            State::ChangedInTenant => "changed in tenant",
            State::ChangedOnDisk => "changed on disk",
            State::ChangedBoth => "changed on both sides",
            State::OnlyTenant { known: false } => "new in tenant",
            State::OnlyTenant { known: true } => "deleted on disk",
            State::OnlyDisk => "only on disk",
        }
    }
    /// `load` would throw away a change made in the tenant.
    pub fn blocks_load(self) -> bool {
        matches!(self, State::ChangedInTenant | State::ChangedBoth | State::OnlyTenant { known: false })
    }
    /// `save` would throw away a change made on disk.
    pub fn blocks_save(self) -> bool {
        matches!(self, State::ChangedOnDisk | State::ChangedBoth | State::OnlyDisk | State::OnlyTenant { known: true })
    }
}

/// State of every path that occurs on either side, sorted by path.
pub fn compare(tenant: &[TenantFile], disk: &BTreeMap<String, Vec<u8>>) -> Vec<(String, State)> {
    let by_path: BTreeMap<&str, &TenantFile> = tenant.iter().map(|f| (f.path.as_str(), f)).collect();
    let paths: BTreeSet<&str> = by_path.keys().copied().chain(disk.keys().map(|s| s.as_str())).collect();
    paths
        .into_iter()
        .map(|p| {
            let state = match (by_path.get(p), disk.get(p)) {
                (Some(t), Some(d)) => {
                    let (ht, hd) = (hash(&t.content), hash(d));
                    if ht == hd {
                        State::Same
                    } else {
                        match &t.disk_hash {
                            None => State::ChangedBoth,
                            Some(h0) => match (&ht != h0, &hd != h0) {
                                (true, false) => State::ChangedInTenant,
                                (false, true) => State::ChangedOnDisk,
                                _ => State::ChangedBoth,
                            },
                        }
                    }
                }
                (Some(t), None) => State::OnlyTenant { known: t.disk_hash.is_some() },
                (None, Some(_)) => State::OnlyDisk,
                (None, None) => unreachable!(),
            };
            (p.to_string(), state)
        })
        .collect()
}

/// Paths that stop `load` or `save` (without `--force`).
pub fn conflicts(states: &[(String, State)], for_load: bool) -> Vec<(String, State)> {
    states.iter().filter(|(_, s)| if for_load { s.blocks_load() } else { s.blocks_save() }).cloned().collect()
}

/// What `load` changes in the tenant.
#[derive(Debug, Default, PartialEq)]
pub struct LoadPlan {
    pub create: Vec<String>,
    pub update: Vec<String>,
    pub delete: Vec<String>,
}

pub fn plan_load(states: &[(String, State)]) -> LoadPlan {
    let mut p = LoadPlan::default();
    for (path, s) in states {
        match s {
            State::Same => {}
            State::OnlyDisk => p.create.push(path.clone()),
            State::OnlyTenant { .. } => p.delete.push(path.clone()),
            _ => p.update.push(path.clone()),
        }
    }
    p
}

/// What `save` changes on disk.
#[derive(Debug, Default, PartialEq)]
pub struct SavePlan {
    pub write: Vec<String>,
    pub delete: Vec<String>,
}

pub fn plan_save(states: &[(String, State)]) -> SavePlan {
    let mut p = SavePlan::default();
    for (path, s) in states {
        match s {
            State::Same => {}
            State::OnlyDisk => p.delete.push(path.clone()),
            _ => p.write.push(path.clone()),
        }
    }
    p
}

/// A path that is safe to write below `i3cm/`: relative, no `..`, no empty or hidden parts.
pub fn safe_path(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.contains('\\') && path.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && !c.starts_with('.'))
}

/// UTC time as RFC 3339 (seconds), from seconds since the Unix epoch.
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil from days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn now() -> String {
    rfc3339(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

/// `git describe --always --dirty` of the repository, if git is there.
fn revision(repo: &Path) -> Option<String> {
    let out = std::process::Command::new("git").arg("-C").arg(repo).args(["describe", "--always", "--dirty"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn attr<'a>(obj: &'a Value, ty: &str, name: &str) -> Option<&'a Value> {
    obj["parts"].as_array()?.iter().find(|p| p["type"] == ty).map(|p| &p["attributes"][name])
}

/// The module under development and its files, as the tenant holds them.
pub struct Remote {
    pub container: Option<String>,
    pub files: Vec<TenantFile>,
}

fn fetch(api: &Api, name: &str) -> Result<Remote, String> {
    let list = api.get_json(&format!("/types/{CONTAINER_TYPE}/objects"), &[("attr", "name"), ("eq", name), ("limit", "2")])?;
    let Some(id) = list.as_array().and_then(|a| a.first()).and_then(|o| o["id"].as_str()).map(String::from) else {
        return Ok(Remote { container: None, files: vec![] });
    };
    let ends = api.get_json(&format!("/objects/{id}/ends/{FILE_END}"), &[("limit", "1000")])?;
    let mut files = vec![];
    for e in ends.as_array().cloned().unwrap_or_default() {
        let fid = e["id"].as_str().unwrap_or_default().to_string();
        let obj = api.get_json(&format!("/objects/{fid}"), &[])?;
        let path = attr(&obj, FILE_TYPE, "path").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let disk_hash = attr(&obj, FILE_TYPE, "diskHash").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);
        let content = if obj["content"].is_null() { vec![] } else { api.get_bytes(&format!("/objects/{fid}/content"), &[])? };
        files.push(TenantFile { id: fid, path, content, disk_hash });
    }
    let mut seen = BTreeSet::new();
    for f in &files {
        if !seen.insert(f.path.as_str()) {
            return Err(format!("the tenant holds {} twice in module {name}; delete one in the studio", f.path));
        }
    }
    Ok(Remote { container: Some(id), files })
}

/// Whether a `.gitignore` already ignores `.i3cm/` (one of the four usual spellings on a line).
pub fn ignores_repo_dir(gitignore: &str) -> bool {
    gitignore.lines().any(|l| matches!(l.trim(), ".i3cm" | ".i3cm/" | "/.i3cm" | "/.i3cm/"))
}

/// The `.gitignore` with `.i3cm/` added, or `None` if it already ignores it.
pub fn with_repo_dir_ignored(gitignore: &str) -> Option<String> {
    if ignores_repo_dir(gitignore) {
        return None;
    }
    let mut t = gitignore.to_string();
    if !t.is_empty() && !t.ends_with('\n') {
        t.push('\n');
    }
    t.push_str(".i3cm/\n");
    Some(t)
}

/// Check the tenant behind the token (`GET /v1/me`): a development tenant, and the configured
/// one when the server reports its name.
pub fn check_tenant(me: &Value, tenant: &str) -> Result<(), String> {
    let t = &me["_tenant"];
    if let Some(name) = t["name"].as_str().filter(|n| *n != tenant) {
        return Err(format!("the token belongs to tenant {name}, not to {tenant}"));
    }
    if t["development"].as_bool() != Some(true) {
        return Err(format!("tenant {tenant} is not a development tenant"));
    }
    Ok(())
}

/// Repository, module name and the files on disk.
/// Repository root, module name, files below `i3cm/`.
type Local = (PathBuf, String, BTreeMap<String, Vec<u8>>);

fn local(start: &Path) -> Result<Local, String> {
    let repo = find_repo(start).ok_or_else(|| format!("no {SOURCE_DIR}/module.ts in {} or above", start.display()))?;
    let module_ts = std::fs::read_to_string(repo.join(SOURCE_DIR).join("module.ts")).map_err(|e| e.to_string())?;
    let name = module_name(&module_ts).ok_or("module.ts names no module (export const module = { name: \"…\" })")?;
    let files = disk_files(&repo)?;
    Ok((repo, name, files))
}

fn report(states: &[(String, State)]) {
    for (p, s) in states {
        println!("{:<22} {p}", s.label());
    }
}

fn refuse(what: &str, list: &[(String, State)]) -> Result<i32, String> {
    eprintln!("i3cm studio {what}: stopped, it would throw away these changes (use --force to go on):");
    for (p, s) in list {
        eprintln!("  {:<22} {p}", s.label());
    }
    Ok(3)
}

pub fn status(api: &Api, start: &Path) -> Result<i32, String> {
    let (_, name, disk) = local(start)?;
    let remote = fetch(api, &name)?;
    if remote.container.is_none() {
        println!("module {name} is not in the tenant yet (i3cm studio load)");
    }
    report(&compare(&remote.files, &disk));
    Ok(0)
}

/// The operations of `load`: the container if missing, new files with their link, contents of new
/// and changed files, deleted files, the hashes of the disk state, time and revision. One order,
/// all or nothing.
pub fn load_ops(name: &str, room: Option<&str>, remote: &Remote, disk: &BTreeMap<String, Vec<u8>>, plan: &LoadPlan, now: &str, revision: Option<&str>) -> Vec<Value> {
    let mut ops = vec![];
    let container = match &remote.container {
        Some(id) => id.clone(),
        None => {
            let mut op = json!({"op": "create", "ref": "m", "types": {CONTAINER_TYPE: {"name": name}}});
            if let Some(room) = room {
                op["room"] = json!(room);
            }
            ops.push(op);
            "m".to_string()
        }
    };
    let mut ids: BTreeMap<&str, String> = remote.files.iter().map(|f| (f.path.as_str(), f.id.clone())).collect();
    for (i, p) in plan.create.iter().enumerate() {
        let r = format!("f{i}");
        ops.push(json!({"op": "create", "ref": r, "types": {FILE_TYPE: {"path": p}}}));
        ops.push(json!({"op": "link", "reference": FILE_REFERENCE, "from": r, "to": container}));
        ids.insert(p.as_str(), r);
    }
    for p in plan.create.iter().chain(plan.update.iter()) {
        let file_name = p.rsplit('/').next().unwrap_or(p);
        ops.push(json!({"op": "content", "id": ids[p.as_str()], "mediaType": "text/plain", "filename": file_name,
            "base64": base64::engine::general_purpose::STANDARD.encode(&disk[p])}));
    }
    for p in &plan.delete {
        ops.push(json!({"op": "delete", "id": ids[p.as_str()]}));
    }
    for (p, b) in disk {
        ops.push(json!({"op": "update", "id": ids[p.as_str()], "types": {FILE_TYPE: {"diskHash": hash(b)}}}));
    }
    let mut values = json!({"loadedAt": now});
    if let Some(r) = revision {
        values["revision"] = json!(r);
    }
    ops.push(json!({"op": "update", "id": container, "types": {CONTAINER_TYPE: values}}));
    ops
}

/// Make the tenant equal to the disk, in one order.
pub fn load(api: &Api, start: &Path, force: bool) -> Result<i32, String> {
    let (repo, name, disk) = local(start)?;
    for p in disk.keys() {
        if !safe_path(p) {
            return Err(format!("unsafe file name below {SOURCE_DIR}/: {p}"));
        }
    }
    let remote = fetch(api, &name)?;
    let states = compare(&remote.files, &disk);
    let blocking = conflicts(&states, true);
    if !blocking.is_empty() && !force {
        return refuse("load", &blocking);
    }
    let plan = plan_load(&states);
    let room = if remote.container.is_none() { api.get_json("/rooms", &[])?[ROOM].as_str().map(String::from) } else { None };
    let ops = load_ops(&name, room.as_deref(), &remote, &disk, &plan, &now(), revision(&repo).as_deref());
    api.post_json("/orders", &json!({"ops": ops}))?;
    println!("i3cm studio load: module {name}: {} new, {} changed, {} deleted", plan.create.len(), plan.update.len(), plan.delete.len());
    Ok(0)
}

/// Make the disk equal to the tenant; never touches anything outside `i3cm/`.
pub fn save(api: &Api, start: &Path, force: bool) -> Result<i32, String> {
    let (repo, name, disk) = local(start)?;
    let remote = fetch(api, &name)?;
    let Some(container) = remote.container.clone() else {
        return Err(format!("module {name} is not in the tenant (i3cm studio load first)"));
    };
    for f in &remote.files {
        if !safe_path(&f.path) {
            return Err(format!("unsafe file name in the tenant: {}", f.path));
        }
    }
    let states = compare(&remote.files, &disk);
    let blocking = conflicts(&states, false);
    if !blocking.is_empty() && !force {
        return refuse("save", &blocking);
    }
    let plan = plan_save(&states);
    let root = repo.join(SOURCE_DIR);
    let by_path: BTreeMap<&str, &TenantFile> = remote.files.iter().map(|f| (f.path.as_str(), f)).collect();
    // stage first: every new content goes to a hidden sibling; only when all are written are they
    // renamed into place and the deleted files removed, so a failing write changes nothing
    let staged: Vec<(PathBuf, PathBuf)> = plan.write.iter().map(|p| {
        let target = root.join(p);
        let tmp = target.with_file_name(format!(".{}.i3cm-save", target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
        (tmp, target)
    }).collect();
    let written = (|| -> Result<(), String> {
        for ((tmp, target), p) in staged.iter().zip(&plan.write) {
            if let Some(dir) = target.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::write(tmp, &by_path[p.as_str()].content).map_err(|e| format!("{}: {e}", target.display()))?;
        }
        Ok(())
    })();
    if let Err(e) = written {
        for (tmp, _) in &staged {
            let _ = std::fs::remove_file(tmp);
        }
        return Err(e);
    }
    for (tmp, target) in &staged {
        std::fs::rename(tmp, target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    for p in &plan.delete {
        let target = root.join(p);
        std::fs::remove_file(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    let mut ops: Vec<Value> = remote.files.iter().map(|f| json!({"op": "update", "id": f.id, "types": {FILE_TYPE: {"diskHash": hash(&f.content)}}})).collect();
    ops.push(json!({"op": "update", "id": container, "types": {CONTAINER_TYPE: {"savedAt": now()}}}));
    api.post_json("/orders", &json!({"ops": ops}))?;
    println!("i3cm studio save: module {name}: {} written, {} deleted", plan.write.len(), plan.delete.len());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tf(path: &str, content: &str, disk_hash: Option<&str>) -> TenantFile {
        TenantFile { id: format!("id-{path}"), path: path.into(), content: content.as_bytes().to_vec(), disk_hash: disk_hash.map(|s| hash(s.as_bytes())) }
    }
    fn disk(files: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
        files.iter().map(|(p, c)| (p.to_string(), c.as_bytes().to_vec())).collect()
    }

    #[test]
    fn module_name_is_read_without_running_the_file() {
        assert_eq!(module_name("import x;\nexport const module = { name: \"studio\", requires: [\"base\"] };").as_deref(), Some("studio"));
        assert_eq!(module_name("export const module = {\n  requires: ['base'],\n  name: 'dms', mandatory: true,\n};").as_deref(), Some("dms"));
        assert_eq!(module_name("export const rooms = [];"), None);
        assert_eq!(module_name("export const module = { name: \"\" };"), None);
    }

    #[test]
    fn the_six_states() {
        let t = vec![
            tf("same.ts", "a", Some("a")),
            tf("tenant.ts", "b2", Some("b")),
            tf("disk.ts", "c", Some("c")),
            tf("both.ts", "d2", Some("d")),
            tf("newtenant.ts", "e", None),
            tf("gone.ts", "f", Some("f")),
            tf("unsynced.ts", "g", None),
        ];
        let d = disk(&[("same.ts", "a"), ("tenant.ts", "b"), ("disk.ts", "c2"), ("both.ts", "d3"), ("newdisk.ts", "h"), ("unsynced.ts", "g2")]);
        let s: BTreeMap<String, State> = compare(&t, &d).into_iter().collect();
        assert_eq!(s["same.ts"], State::Same);
        assert_eq!(s["tenant.ts"], State::ChangedInTenant);
        assert_eq!(s["disk.ts"], State::ChangedOnDisk);
        assert_eq!(s["both.ts"], State::ChangedBoth);
        assert_eq!(s["newtenant.ts"], State::OnlyTenant { known: false });
        assert_eq!(s["gone.ts"], State::OnlyTenant { known: true });
        assert_eq!(s["newdisk.ts"], State::OnlyDisk);
        assert_eq!(s["unsynced.ts"], State::ChangedBoth, "different without a known common state counts as both");
        let names: Vec<String> = compare(&t, &d).into_iter().map(|(p, _)| p).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn load_and_save_refuse_what_they_would_throw_away() {
        let t = vec![tf("tenant.ts", "b2", Some("b")), tf("disk.ts", "c", Some("c")), tf("gone.ts", "f", Some("f")), tf("new.ts", "n", None)];
        let d = disk(&[("tenant.ts", "b"), ("disk.ts", "c2"), ("local.ts", "l")]);
        let states = compare(&t, &d);
        let load: Vec<String> = conflicts(&states, true).into_iter().map(|(p, _)| p).collect();
        let save: Vec<String> = conflicts(&states, false).into_iter().map(|(p, _)| p).collect();
        assert_eq!(load, ["new.ts", "tenant.ts"]);
        assert_eq!(save, ["disk.ts", "gone.ts", "local.ts"]);
    }

    #[test]
    fn plans() {
        let t = vec![tf("same.ts", "a", Some("a")), tf("changed.ts", "x", Some("y")), tf("onlytenant.ts", "o", Some("o"))];
        let d = disk(&[("same.ts", "a"), ("changed.ts", "y2"), ("onlydisk.ts", "n")]);
        let states = compare(&t, &d);
        assert_eq!(plan_load(&states), LoadPlan { create: vec!["onlydisk.ts".into()], update: vec!["changed.ts".into()], delete: vec!["onlytenant.ts".into()] });
        assert_eq!(plan_save(&states), SavePlan { write: vec!["changed.ts".into(), "onlytenant.ts".into()], delete: vec!["onlydisk.ts".into()] });
    }

    #[test]
    fn load_is_one_order_with_contents() {
        let remote = Remote { container: None, files: vec![tf("old.ts", "o", Some("o")), tf("changed.ts", "x", Some("x"))] };
        let d = disk(&[("changed.ts", "y"), ("module.ts", "export const module = { name: \"demo\" };")]);
        let plan = plan_load(&compare(&remote.files, &d));
        let ops = load_ops("demo", Some("room-1"), &remote, &d, &plan, "2026-11-27T00:00:00Z", Some("v1-2-gabc"));
        let kinds: Vec<String> = ops.iter().map(|o| format!("{}:{}", o["op"].as_str().unwrap(), o["id"].as_str().or(o["ref"].as_str()).unwrap_or(""))).collect();
        assert_eq!(kinds, [
            "create:m", "create:f0", "link:", "content:f0", "content:id-changed.ts", "delete:id-old.ts",
            "update:id-changed.ts", "update:f0", "update:m",
        ]);
        assert_eq!(ops[0]["room"], "room-1");
        assert_eq!(ops[1]["types"][FILE_TYPE]["path"], "module.ts");
        assert_eq!(ops[2]["to"], "m");
        assert_eq!(ops[3]["filename"], "module.ts");
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(ops[4]["base64"].as_str().unwrap()).unwrap(), b"y");
        assert_eq!(ops[6]["types"][FILE_TYPE]["diskHash"], hash(b"y"));
        assert_eq!(ops[8]["types"][CONTAINER_TYPE]["revision"], "v1-2-gabc");
    }

    #[test]
    fn safe_paths_only() {
        assert!(safe_path("types/x.ts"));
        assert!(safe_path("module.ts"));
        for bad in ["", "/etc/passwd", "../x", "a/../b", "a//b", ".hidden", "a/.git/config", "a\\b", "./x"] {
            assert!(!safe_path(bad), "{bad}");
        }
    }

    #[test]
    fn time_format() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_795_740_000), "2026-11-27T00:40:00Z");
    }

    #[test]
    fn repo_and_files_on_disk() {
        let base = std::env::temp_dir().join(format!("i3cm-studio-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("i3cm/types")).unwrap();
        std::fs::create_dir_all(base.join("i3cm/.cache")).unwrap();
        std::fs::create_dir_all(base.join("i3cm/node_modules/x")).unwrap();
        std::fs::create_dir_all(base.join("ops/src")).unwrap();
        std::fs::write(base.join("i3cm/module.ts"), "export const module = { name: \"demo\" };").unwrap();
        std::fs::write(base.join("i3cm/types/a.ts"), "class A {}").unwrap();
        std::fs::write(base.join("i3cm/.cache/x"), "no").unwrap();
        std::fs::write(base.join("i3cm/node_modules/x/y.js"), "no").unwrap();
        std::fs::write(base.join("ops/src/lib.rs"), "no").unwrap();
        assert_eq!(find_repo(&base.join("ops/src")).as_deref(), Some(base.as_path()));
        let files = disk_files(&base).unwrap();
        assert_eq!(files.keys().cloned().collect::<Vec<_>>(), ["module.ts", "types/a.ts"]);
        assert_eq!(find_repo(Path::new("/")), None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn gitignore_gets_the_repository_directory_once() {
        assert_eq!(with_repo_dir_ignored("").as_deref(), Some(".i3cm/\n"));
        assert_eq!(with_repo_dir_ignored("target/").as_deref(), Some("target/\n.i3cm/\n"));
        assert_eq!(with_repo_dir_ignored("target/\n").as_deref(), Some("target/\n.i3cm/\n"));
        for has in [".i3cm", ".i3cm/", "/.i3cm", "/.i3cm/", "x\n  .i3cm/  \ny"] {
            assert_eq!(with_repo_dir_ignored(has), None, "{has:?}");
        }
        assert!(with_repo_dir_ignored("# .i3cm/\n").is_some(), "a comment does not count");
    }

    #[test]
    fn the_tenant_behind_the_token() {
        let me = |name: Option<&str>, dev: bool| json!({"_tenant": {"id": "x", "name": name, "development": dev}});
        assert!(check_tenant(&me(Some("dev"), true), "dev").is_ok());
        assert!(check_tenant(&me(Some("prod"), true), "dev").unwrap_err().contains("belongs to tenant prod"));
        assert!(check_tenant(&me(Some("dev"), false), "dev").unwrap_err().contains("not a development tenant"));
        assert!(check_tenant(&json!({"_tenant": {"development": true}}), "dev").is_ok(), "older servers report no name");
    }

    #[test]
    fn hash_is_sha256_hex() {
        assert_eq!(hash(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
}
