//! Development support for the studio: checking module sources with typecat and the
//! catalog checker, and reading the sources of installed modules.
//!
//! Configuration (environment, read per request): `I3CM_NODE` (default `node`), `I3CM_TYPECAT`
//! (path of typecat.mjs), `I3CM_TYPES_DIR` (the kernel's type sources), `I3CM_MODULES_DIR`
//! (installed modules; their sources lie in `<dir>/src/<module>/i3cm/`).

use i3cm_core::{BaseError, Model};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// Limits of one check: modules, files and bytes of all texts.
pub const MAX_MODULES: usize = 16;
pub const MAX_FILES: usize = 400;
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

const REPO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var(var).map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(REPO).join(default))
}
fn typecat() -> PathBuf {
    env_path("I3CM_TYPECAT", "tools/typecat/typecat.mjs")
}
fn types_dir() -> PathBuf {
    env_path("I3CM_TYPES_DIR", "types")
}
pub fn modules_dir() -> PathBuf {
    env_path("I3CM_MODULES_DIR", "deploy/modules")
}

#[derive(Deserialize)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
}

#[derive(Deserialize)]
pub struct CheckModule {
    pub name: String,
    pub files: Vec<SourceFile>,
}

#[derive(Deserialize)]
pub struct CheckBody {
    pub modules: Vec<CheckModule>,
}

/// A path inside a module: relative, no `..`; a leading `i3cm/` is dropped.
fn safe_rel(p: &str) -> Result<PathBuf, BaseError> {
    let p = p.strip_prefix("i3cm/").unwrap_or(p);
    let rel = Path::new(p);
    if p.is_empty() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(BaseError::invalid("check.bad_path", "file paths must be relative and stay inside the module").with("path", p));
    }
    Ok(rel.to_path_buf())
}

fn name_ok(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `requires: ["a", "b"]` from a module.ts text.
fn requires_of(module_ts: &str) -> Vec<String> {
    let Some(i) = module_ts.find("requires") else { return vec![] };
    let rest = &module_ts[i..];
    let (Some(a), Some(b)) = (rest.find('['), rest.find(']')) else { return vec![] };
    if b < a {
        return vec![];
    }
    rest[a + 1..b].split(',').map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string()).filter(|s| name_ok(s)).collect()
}

/// Sources of an installed module, if its package carried them.
pub fn installed_sources(name: &str) -> Option<PathBuf> {
    let d = modules_dir().join("src").join(name).join("i3cm");
    (name_ok(name) && d.join("module.ts").is_file()).then_some(d)
}

/// All files of an installed module's sources as `[{path, text}]` (paths relative to `i3cm/`).
pub fn read_sources(name: &str) -> Option<Vec<Value>> {
    let root = installed_sources(name)?;
    let mut out = vec![];
    fn walk(root: &Path, d: &Path, out: &mut Vec<Value>) {
        let mut es: Vec<_> = std::fs::read_dir(d).into_iter().flatten().flatten().collect();
        es.sort_by_key(|e| e.file_name());
        for e in es {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else if let Ok(text) = std::fs::read_to_string(&p) {
                out.push(json!({ "path": format!("i3cm/{}", p.strip_prefix(root).unwrap_or(&p).to_string_lossy()), "text": text }));
            }
        }
    }
    walk(&root, &root, &mut out);
    Some(out)
}

/// Check uploaded module sources against the installed modules: typecat (all issues with
/// positions), then the catalog checker of the server. `installed` lists the requires of the
/// installed modules (catalog of the running server).
pub async fn check(body: CheckBody, installed: &[(String, Vec<String>)]) -> Result<Value, BaseError> {
    if body.modules.is_empty() {
        return Err(BaseError::invalid("check.no_modules", "nothing to check"));
    }
    let files: usize = body.modules.iter().map(|m| m.files.len()).sum();
    let bytes: usize = body.modules.iter().flat_map(|m| m.files.iter()).map(|f| f.text.len()).sum();
    if body.modules.len() > MAX_MODULES || files > MAX_FILES || bytes > MAX_BYTES {
        return Err(BaseError::invalid("check.too_large", "too many modules, files or bytes for one check")
            .with("maxModules", MAX_MODULES as i64).with("maxFiles", MAX_FILES as i64).with("maxBytes", MAX_BYTES as i64));
    }
    let work = std::env::temp_dir().join(format!("i3cm-check-{}", uuid::Uuid::new_v4()));
    let res = check_in(&work, body, installed).await;
    let _ = std::fs::remove_dir_all(&work);
    res
}

async fn check_in(work: &Path, body: CheckBody, installed: &[(String, Vec<String>)]) -> Result<Value, BaseError> {
    let io = |e: std::io::Error| BaseError::internal(format!("check: {e}"));
    let mut given: BTreeSet<String> = BTreeSet::new();
    let mut wanted: Vec<String> = vec![];
    let mut args: Vec<PathBuf> = vec![];
    for m in &body.modules {
        if !name_ok(&m.name) || !given.insert(m.name.clone()) {
            return Err(BaseError::invalid("check.bad_module", "module names must be unique words").with("module", m.name.clone()));
        }
        let root = work.join(&m.name).join("i3cm");
        for f in &m.files {
            let rel = safe_rel(&f.path)?;
            let p = root.join(&rel);
            std::fs::create_dir_all(p.parent().unwrap_or(&root)).map_err(io)?;
            std::fs::write(&p, &f.text).map_err(io)?;
            if rel == Path::new("module.ts") {
                wanted.extend(requires_of(&f.text));
            }
        }
        args.push(work.join(&m.name));
    }
    // sources of installed modules the given ones depend on, transitively
    let mut seen: BTreeSet<String> = given.clone();
    while let Some(r) = wanted.pop() {
        if r == "base" || !seen.insert(r.clone()) {
            continue;
        }
        if let Some(src) = installed_sources(&r) {
            args.push(src);
        }
        if let Some((_, req)) = installed.iter().find(|(n, _)| *n == r) {
            wanted.extend(req.iter().cloned());
        }
    }
    let mut cmd = tokio::process::Command::new(std::env::var("I3CM_NODE").unwrap_or_else(|_| "node".into()));
    cmd.arg(typecat()).arg("check").arg(types_dir());
    for a in &args {
        cmd.arg("--module").arg(a);
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), cmd.kill_on_drop(true).output())
        .await
        .map_err(|_| BaseError::internal("check: typecat timed out"))?
        .map_err(|e| BaseError::internal(format!("check: cannot run typecat: {e}")))?;
    let mut v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| BaseError::internal(format!("check: typecat failed: {}", String::from_utf8_lossy(&out.stderr).chars().take(500).collect::<String>())))?;
    // file paths relative to the module (i3cm/...), not to the temporary directory
    // file names without server paths: "<module>:i3cm/...", kernel types as "kernel:<path>"
    let canon = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    let (src_root, kernel) = (canon(modules_dir().join("src")), canon(types_dir()));
    let strip = |f: &str| -> String {
        let p = Path::new(f);
        for m in &given {
            if let Ok(r) = p.strip_prefix(work.join(m)) {
                return format!("{m}:{}", r.to_string_lossy());
            }
        }
        if let Ok(r) = p.strip_prefix(&src_root) {
            let mut c = r.components();
            if let Some(m) = c.next() {
                return format!("{}:{}", m.as_os_str().to_string_lossy(), c.as_path().to_string_lossy());
            }
        }
        if let Ok(r) = p.strip_prefix(&kernel) {
            return format!("kernel:{}", r.to_string_lossy());
        }
        "?".to_string()
    };
    if let Some(issues) = v["issues"].as_array_mut() {
        // self-service notes concern the modules being checked, not installed ones read along
        issues.retain(|i| {
            i["error_id"] != "rights.self_service"
                || i["params"]["profile"].as_str().and_then(|p| p.split(':').next()).is_some_and(|m| given.contains(m))
        });
        for i in issues.iter_mut() {
            if let Some(f) = i["file"].as_str().map(strip) {
                i["file"] = json!(f);
            }
        }
    }
    if let Some(pos) = v["positions"].as_object_mut() {
        for p in pos.values_mut() {
            if let Some(f) = p["file"].as_str().map(strip) {
                p["file"] = json!(f);
            }
        }
    }
    // the server's catalog checker on what typecat built
    if !v["catalog"].is_null() {
        if let Err(e) = Model::from_json(&v["catalog"].to_string()) {
            let mut issue = json!({ "severity": "error", "source": "checker", "error_id": e.error_id, "description": e.description, "params": e.params });
            // place it at the type or aspect it names, when known
            for key in ["type", "aspect"] {
                if let Some(n) = e.params.get(key).and_then(|x| x.as_str()) {
                    if let Some(p) = v["positions"].get(format!("{key}:{n}")) {
                        for f in ["file", "line", "column"] {
                            issue[f] = p[f].clone();
                        }
                        break;
                    }
                }
            }
            v["issues"].as_array_mut().map(|a| a.push(issue));
            v["catalog"] = Value::Null;
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_module() {
        assert_eq!(safe_rel("i3cm/types/a.ts").unwrap(), PathBuf::from("types/a.ts"));
        assert!(safe_rel("../x").is_err());
        assert!(safe_rel("/etc/passwd").is_err());
        assert!(safe_rel("types/../../x").is_err());
    }

    #[test]
    fn requires_are_read_from_module_ts() {
        assert_eq!(requires_of(r#"export const module = { name: "mail", requires: ["base", "dms"] };"#), vec!["base", "dms"]);
        assert!(requires_of("export const module = { name: \"x\" };").is_empty());
    }
}
