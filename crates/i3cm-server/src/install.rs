//! Installing module packages (operator): verify the signature and checksums, check
//! the merged catalog, create the tables in one transaction, then place the files in the modules
//! directory. The new catalog is in force after the next start of the main container.

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use i3cm_core::catalog::Catalog;
use i3cm_core::Model;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, serde::Serialize)]
pub struct Installed {
    pub module: String,
    pub sha256: String,
    pub tables: usize,
    pub files: Vec<String>,
    /// Set when an unsigned (or not trusted) package was accepted on a development instance.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// The built-in key file of i3cm-org (`deploy/keys/i3cm-org.pub`).
const BUILTIN_KEYS: &str = include_str!("../../../deploy/keys/i3cm-org.pub");

/// Keys of a key file: one hex key per line, `#` starts a comment.
pub fn parse_keys(text: &str) -> Vec<String> {
    text.lines().map(|l| l.split('#').next().unwrap_or("").trim().to_string()).filter(|l| !l.is_empty()).collect()
}

/// What the instance trusts: the built-in key of i3cm-org, the operator's keys, and whether
/// packages without a trusted signature are accepted (development instances only).
#[derive(Debug, Clone, Default)]
pub struct Trust {
    pub keys: Vec<String>,
    pub allow_unsigned: bool,
}

impl Trust {
    /// Exactly these keys, signatures required (tests, tools).
    pub fn keys(keys: &[String]) -> Trust {
        Trust { keys: keys.to_vec(), allow_unsigned: false }
    }

    /// The built-in keys of i3cm-org.
    pub fn builtin() -> Vec<String> {
        parse_keys(BUILTIN_KEYS)
    }

    /// The instance's trust: built-in keys, `extra` (command line), `I3CM_TRUSTED_KEYS`
    /// (comma-separated), the file `I3CM_TRUSTED_KEYS_FILE`; unsigned packages are accepted
    /// when the instance allows development (`I3CM_ALLOW_DEVELOPMENT`).
    pub fn from_env(extra: &[String]) -> Result<Trust> {
        let file = match std::env::var("I3CM_TRUSTED_KEYS_FILE") {
            Ok(f) => Some(std::fs::read_to_string(&f).with_context(|| format!("cannot read I3CM_TRUSTED_KEYS_FILE {f}"))?),
            Err(_) => None,
        };
        let env = std::env::var("I3CM_TRUSTED_KEYS").unwrap_or_default();
        let dev = flag("I3CM_ALLOW_DEVELOPMENT")?;
        Ok(Trust::build(BUILTIN_KEYS, extra, &env, file.as_deref(), dev))
    }

    /// The trust from its sources: built-in key file, extra keys, a comma-separated list, a key
    /// file of the operator, development instance or not.
    pub fn build(builtin: &str, extra: &[String], list: &str, file: Option<&str>, development: bool) -> Trust {
        let mut keys = parse_keys(builtin);
        keys.extend(extra.iter().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()));
        keys.extend(list.split(',').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()));
        keys.extend(file.map(parse_keys).unwrap_or_default());
        keys.sort();
        keys.dedup();
        Trust { keys, allow_unsigned: development }
    }
}

/// A yes/no setting: unset is no; `true`, `1`, `yes` and `false`, `0`, `no` in any case; anything
/// else is an error instead of a silent "no".
pub fn flag(name: &str) -> Result<bool> {
    match std::env::var(name) {
        Err(_) => Ok(false),
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" | "" => Ok(false),
            _ => bail!("{name}={v:?} is not a yes/no value; use true or false"),
        },
    }
}

fn hex_sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// Merge the base catalog with the module parts in `dir` (`<module>.json`).
pub fn merged_catalog(base: &Path, dir: Option<&Path>) -> Result<Catalog> {
    let mut cat: Catalog = serde_json::from_str(&std::fs::read_to_string(base).with_context(|| format!("cannot read {}", base.display()))?)?;
    if let Some(dir) = dir {
        let mut parts: Vec<PathBuf> = std::fs::read_dir(dir).map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "json")).collect()).unwrap_or_default();
        parts.sort();
        for p in parts {
            let part: Catalog = serde_json::from_str(&std::fs::read_to_string(&p)?).with_context(|| format!("module part {}", p.display()))?;
            add_part(&mut cat, part);
        }
    }
    Ok(cat)
}

fn add_part(cat: &mut Catalog, part: Catalog) {
    for m in &part.modules {
        let n = m.name.clone();
        let own = |x: &str| x.split(':').next() == Some(n.as_str());
        cat.modules.retain(|x| x.name != n);
        cat.types.retain(|t| !own(&t.name));
        cat.references.retain(|r| !own(&r.name));
        cat.text_types.retain(|t| !own(&t.name));
    }
    cat.modules.extend(part.modules);
    cat.types.extend(part.types);
    cat.references.extend(part.references);
    cat.text_types.extend(part.text_types);
}

/// Load the catalog for serving: base plus installed module parts.
pub fn load_model(base: &Path, dir: Option<&Path>) -> Result<Model> {
    Model::new(merged_catalog(base, dir)?).map_err(|e| anyhow::anyhow!("{}: {} {}", e.error_id, e.description, serde_json::Value::Object(e.params)))
}

/// Whether `sig_hex` is a signature of the package by one of the keys.
fn signed_by(pkg: &[u8], sig_hex: &str, keys: &[String]) -> bool {
    let digest = Sha256::digest(pkg);
    let Some(sig) = hex::decode(sig_hex.trim()).ok().and_then(|b| Signature::from_slice(&b).ok()) else { return false };
    keys.iter().any(|k| {
        hex::decode(k.trim()).ok().and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()).and_then(|b| VerifyingKey::from_bytes(&b).ok()).is_some_and(|vk| vk.verify(&digest, &sig).is_ok())
    })
}

/// Verify a package against the trusted public keys (hex ed25519) and unpack it in memory.
pub fn verify(pkg: &[u8], sig_hex: &str, keys: &[String]) -> Result<BTreeMap<String, Vec<u8>>> {
    Ok(verify_with(pkg, Some(sig_hex), &Trust::keys(keys))?.0)
}

/// Files of a package by name.
pub type Files = BTreeMap<String, Vec<u8>>;

/// Verify a package against the instance's trust and unpack it; the second value lists
/// warnings (a package without a trusted signature accepted on a development instance).
/// The checksums in SHA256SUMS are always checked.
pub fn verify_with(pkg: &[u8], sig_hex: Option<&str>, trust: &Trust) -> Result<(Files, Vec<String>)> {
    let mut warnings = vec![];
    let ok = sig_hex.is_some_and(|s| signed_by(pkg, s, &trust.keys));
    if !ok {
        let why = if sig_hex.is_none() { "package has no signature" } else { "package signature does not match any trusted key" };
        if !trust.allow_unsigned {
            bail!("{why}");
        }
        tracing::warn!("{why}: accepted because the instance allows development");
        warnings.push(format!("{why}: accepted because the instance allows development"));
    }
    let mut files = BTreeMap::new();
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(pkg));
    for e in ar.entries()? {
        let mut e = e?;
        let name = e.path()?.to_string_lossy().to_string();
        if name.contains("..") || name.starts_with('/') {
            bail!("unsafe path in package: {name}");
        }
        let mut b = vec![];
        e.read_to_end(&mut b)?;
        files.insert(name, b);
    }
    let sums = String::from_utf8(files.get("SHA256SUMS").context("package without SHA256SUMS")?.clone())?;
    let mut listed = 0;
    for line in sums.lines().filter(|l| !l.trim().is_empty()) {
        let (sum, name) = line.split_once("  ").context("bad SHA256SUMS line")?;
        let data = files.get(name).with_context(|| format!("{name} listed but missing"))?;
        if hex_sha(data) != sum {
            bail!("checksum mismatch for {name}");
        }
        listed += 1;
    }
    if listed + 1 != files.len() {
        bail!("package contains files not listed in SHA256SUMS");
    }
    Ok((files, warnings))
}

/// Install a package (signature in `<file>.sig`) with exactly these keys.
pub async fn install(db_url: &str, base: &Path, dir: &Path, pkg_path: &Path, keys: &[String]) -> Result<Installed> {
    install_with(db_url, base, dir, pkg_path, &Trust::keys(keys)).await
}

/// Install a package: verify, check the merged catalog, create tables (one transaction), place files.
pub async fn install_with(db_url: &str, base: &Path, dir: &Path, pkg_path: &Path, trust: &Trust) -> Result<Installed> {
    let pkg = std::fs::read(pkg_path).with_context(|| format!("cannot read {}", pkg_path.display()))?;
    let sig_path = PathBuf::from(format!("{}.sig", pkg_path.display()));
    let sig = match std::fs::read_to_string(&sig_path) {
        Ok(s) => Some(s),
        Err(_) if trust.allow_unsigned => None,
        Err(e) => return Err(anyhow::anyhow!("signature {} missing: {e}", sig_path.display())),
    };
    let (files, warnings) = verify_with(&pkg, sig.as_deref(), trust)?;
    let part: Catalog = serde_json::from_slice(files.get("module.json").context("package without module.json")?)?;
    let module = part.modules.first().context("module.json without module")?.name.clone();
    let mut cat = merged_catalog(base, Some(dir))?;
    add_part(&mut cat, part.clone());
    let model = Model::new(cat).map_err(|e| anyhow::anyhow!("catalog check failed: {}: {} {}", e.error_id, e.description, serde_json::Value::Object(e.params)))?;
    // tables of the new catalog in one transaction (PostgreSQL DDL is transactional)
    let (mut client, conn) = tokio_postgres::connect(db_url, tokio_postgres::NoTls).await?;
    tokio::spawn(conn);
    let tx = client.transaction().await?;
    tx.batch_execute(i3cm_store::schema::SYSTEM).await?;
    let ddl = i3cm_store::schema::catalog_ddl(&model);
    for stmt in &ddl {
        tx.batch_execute(stmt).await?;
    }
    let sha = hex_sha(&pkg);
    let version = pkg_path.file_name().and_then(|f| f.to_str()).and_then(|f| f.strip_prefix(&format!("{module}-"))).and_then(|f| f.strip_suffix(".tar.gz")).unwrap_or("0.0.0").to_string();
    tx.execute(
        "INSERT INTO module_install (name, version, sha256, catalog) VALUES ($1,$2,$3,$4) ON CONFLICT (name) DO UPDATE SET version=EXCLUDED.version, sha256=EXCLUDED.sha256, catalog=EXCLUDED.catalog, installed_at=now()",
        &[&module, &version, &sha, &serde_json::to_value(&part)?],
    )
    .await?;
    // files: all written to temporary names before the commit, renamed after it; a failure before
    // the commit leaves neither database nor files changed
    std::fs::create_dir_all(dir.join("ui"))?;
    let mut staged: Vec<(PathBuf, PathBuf, String)> = vec![];
    let mut stage = |name: String, data: &[u8]| -> Result<()> {
        let tmp = dir.join(format!(".{}.tmp", name.replace('/', "_")));
        if let Some(parent) = dir.join(&name).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&tmp, data)?;
        staged.push((tmp, dir.join(&name), name));
        Ok(())
    };
    let staged_ok = (|| -> Result<()> {
        stage(format!("{module}.json"), &serde_json::to_vec_pretty(&part)?)?;
        if let Some(w) = files.get("module.wasm") {
            stage(format!("{module}.wasm"), w)?;
        }
        for (name, data) in files.iter().filter(|(n, _)| n.starts_with("ui/")) {
            stage(name.clone(), data)?;
        }
        // sources (for checks of dependent modules): <dir>/src/<module>/i3cm/...
        for (name, data) in files.iter().filter(|(n, _)| n.starts_with("src/")) {
            stage(format!("src/{module}/i3cm/{}", &name[4..]), data)?;
        }
        Ok(())
    })();
    if let Err(e) = staged_ok {
        for (tmp, _, _) in &staged {
            let _ = std::fs::remove_file(tmp);
        }
        return Err(e);
    }
    // publish the files first (keeping the previous versions), then commit; if the commit fails,
    // the previous files come back, so database and files never disagree
    let mut placed: Vec<(PathBuf, Option<PathBuf>, String)> = vec![];
    let restore = |placed: &[(PathBuf, Option<PathBuf>, String)]| {
        for (target, bak, _) in placed.iter().rev() {
            match bak {
                Some(b) => {
                    let _ = std::fs::rename(b, target);
                }
                None => {
                    let _ = std::fs::remove_file(target);
                }
            }
        }
    };
    for (tmp, target, name) in &staged {
        let bak = if target.exists() {
            let b = dir.join(format!(".{}.bak", name.replace('/', "_")));
            std::fs::rename(target, &b)?;
            Some(b)
        } else {
            None
        };
        if let Err(e) = std::fs::rename(tmp, target) {
            if let Some(b) = &bak {
                let _ = std::fs::rename(b, target);
            }
            restore(&placed);
            for (t, _, _) in &staged {
                let _ = std::fs::remove_file(t);
            }
            return Err(anyhow::anyhow!("placing {name} failed: {e}; nothing installed"));
        }
        placed.push((target.clone(), bak, name.clone()));
    }
    if let Err(e) = tx.commit().await {
        restore(&placed);
        return Err(anyhow::anyhow!("database commit failed: {e}; files restored, nothing installed"));
    }
    for (_, bak, _) in &placed {
        if let Some(b) = bak {
            let _ = std::fs::remove_file(b);
        }
    }
    let placed: Vec<String> = placed.into_iter().map(|(_, _, n)| n).collect();
    Ok(Installed { module, sha256: sha, tables: ddl.len(), files: placed, warnings })
}

/// Registry entry (`registry.json` of `i3cm-org/i3cm-registry`).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RegistryEntry {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub requires: Vec<String>,
    pub sha256: String,
    pub package: String,
    pub signature: String,
    #[serde(default)]
    pub key: Option<String>,
}

async fn fetch(src: &str) -> Result<Vec<u8>> {
    if src.starts_with("http://") || src.starts_with("https://") {
        let mut req = reqwest::Client::new().get(src);
        // private registries: a token for GitHub release assets, given by the operator
        if let Ok(t) = std::env::var("I3CM_REGISTRY_TOKEN") {
            req = req.bearer_auth(t).header("Accept", "application/octet-stream");
        }
        let r = req.send().await?;
        if !r.status().is_success() {
            bail!("fetching {src}: HTTP {}", r.status());
        }
        Ok(r.bytes().await?.to_vec())
    } else {
        Ok(std::fs::read(src.strip_prefix("file://").unwrap_or(src)).with_context(|| format!("cannot read {src}"))?)
    }
}

/// Read the registry (URL or file) and return its module entries.
pub async fn registry(src: &str) -> Result<Vec<RegistryEntry>> {
    let v: serde_json::Value = serde_json::from_slice(&fetch(src).await?).context("registry is not JSON")?;
    Ok(serde_json::from_value(v.get("modules").cloned().unwrap_or_default()).context("registry modules")?)
}

/// Install `name@version` from the registry: download package and signature, check the checksum
/// listed in the registry, then verify the signature against the operator's trusted keys (the
/// registry only locates packages; trust comes from the operator's configuration).
pub async fn install_from_registry(db_url: &str, base: &Path, dir: &Path, registry_src: &str, name: &str, version: Option<&str>, keys: &[String]) -> Result<Installed> {
    install_from_registry_with(db_url, base, dir, registry_src, name, version, &Trust::keys(keys)).await
}

/// As [`install_from_registry`], with the instance's trust.
pub async fn install_from_registry_with(db_url: &str, base: &Path, dir: &Path, registry_src: &str, name: &str, version: Option<&str>, trust: &Trust) -> Result<Installed> {
    let entries = registry(registry_src).await?;
    let e = entries
        .iter()
        .filter(|e| e.name == name && version.is_none_or(|v| e.version == v))
        .max_by(|a, b| a.version.cmp(&b.version))
        .with_context(|| format!("{name}{} not in the registry", version.map(|v| format!("@{v}")).unwrap_or_default()))?
        .clone();
    let pkg = fetch(&e.package).await?;
    if hex_sha(&pkg) != e.sha256 {
        bail!("package checksum differs from the registry entry");
    }
    let sig = fetch(&e.signature).await?;
    let dl = dir.join(".downloads");
    std::fs::create_dir_all(&dl)?;
    let file = dl.join(format!("{}-{}.tar.gz", e.name, e.version));
    std::fs::write(&file, &pkg)?;
    std::fs::write(PathBuf::from(format!("{}.sig", file.display())), &sig)?;
    install_with(db_url, base, dir, &file, trust).await
}

/// Fail unless `dir` exists (or can be created) and is writable; the message says what to do.
pub fn ensure_writable(dir: &Path) -> Result<()> {
    let hint = || {
        format!(
            "the modules directory {} is not writable by this process; mount a volume there that this user can write \
             (the image runs as uid {}; for an existing volume created by root: \
             `docker run --rm -v <volume>:/d debian chown -R 10001:10001 /d`), or point I3CM_MODULES_DIR to a writable directory",
            dir.display(),
            current_uid()
        )
    };
    std::fs::create_dir_all(dir).with_context(hint)?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let probe = dir.join(format!(".write-probe-{}-{nanos}", std::process::id()));
    std::fs::write(&probe, b"").with_context(hint)?;
    std::fs::remove_file(&probe).with_context(hint)?;
    Ok(())
}

fn current_uid() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Uid:")).and_then(|l| l.split_whitespace().nth(1)).map(str::to_string))
        .unwrap_or_else(|| "?".into())
}

/// Copy the modules bundled with the image (`bundled`) into the modules directory `dir`.
///
/// A bundled module is copied when `dir` lacks it, or when the copy in `dir` is still the one
/// copied from an earlier bundle (marker `.bundled/<name>.sha256`) and the bundle changed. A module
/// the operator installed over it (its catalog part differs from the marker) is kept. Returns one
/// line per change.
pub fn sync_bundled(bundled: &Path, dir: &Path) -> Result<Vec<String>> {
    let mut out = vec![];
    let mut names: Vec<String> = vec![];
    for e in std::fs::read_dir(bundled).with_context(|| format!("cannot read the bundled modules in {}", bundled.display()))? {
        let p = e.with_context(|| format!("cannot read the bundled modules in {}", bundled.display()))?.path();
        if p.extension().is_some_and(|x| x == "json") {
            names.push(p.file_stem().and_then(|s| s.to_str()).map(str::to_string).with_context(|| format!("bad module file name {}", p.display()))?);
        }
    }
    names.sort();
    let markers = dir.join(".bundled");
    std::fs::create_dir_all(&markers)?;
    for name in names {
        let bsha = module_hash(bundled, &name)?.context("bundled module without catalog part")?;
        let marker = markers.join(format!("{name}.sha256"));
        let current = module_hash(dir, &name)?;
        let last = std::fs::read_to_string(&marker).ok().map(|s| s.trim().to_string());
        let copy = match (&current, &last) {
            (None, _) => true,
            (Some(cur), Some(last)) => cur == last && *cur != bsha,
            (Some(_), None) => false,
        };
        if copy {
            copy_module(bundled, dir, &name)?;
            out.push(format!("{name}: copied from the image"));
        } else if current.as_deref() != Some(bsha.as_str()) {
            if current.is_some() && last.is_none() {
                out.push(format!("{name}: kept the copy in the modules directory (not from this image)"));
            } else if current != last {
                out.push(format!("{name}: kept the version installed by the operator"));
            }
            continue;
        }
        std::fs::write(&marker, &bsha)?;
    }
    Ok(out)
}

/// Hash over all files of a module in `dir` (`<name>.json`, `<name>.wasm`, `ui/<name>.html`,
/// `src/<name>/…` with their paths), or `None` without its catalog part.
fn module_hash(dir: &Path, name: &str) -> Result<Option<String>> {
    let json = dir.join(format!("{name}.json"));
    if !json.exists() {
        return Ok(None);
    }
    let mut files: Vec<(String, PathBuf)> = vec![(format!("{name}.json"), json)];
    for f in [format!("{name}.wasm"), format!("ui/{name}.html")] {
        if dir.join(&f).exists() {
            files.push((f.clone(), dir.join(&f)));
        }
    }
    fn walk(base: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        let p = base.join(rel);
        if !p.exists() {
            return Ok(());
        }
        for e in std::fs::read_dir(&p)? {
            let e = e?;
            let r = format!("{rel}/{}", e.file_name().to_string_lossy());
            if e.file_type()?.is_dir() {
                walk(base, &r, out)?;
            } else {
                out.push((r, e.path()));
            }
        }
        Ok(())
    }
    walk(dir, &format!("src/{name}"), &mut files)?;
    files.sort();
    let mut h = Sha256::new();
    for (rel, p) in files {
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(Sha256::digest(std::fs::read(&p)?));
    }
    Ok(Some(hex::encode(h.finalize())))
}

/// Copy one module's files (`<name>.json`, `<name>.wasm`, `ui/<name>.html`, `src/<name>/`): all
/// are staged under temporary names first and renamed into place only when every copy succeeded,
/// the catalog part last, so that a failure leaves the previous version intact.
fn copy_module(from: &Path, to: &Path, name: &str) -> Result<()> {
    let tag = format!(".sync-{}", std::process::id());
    let stage = || -> Result<(Vec<(PathBuf, PathBuf)>, Vec<PathBuf>)> {
        let mut staged = vec![];
        let mut removed = vec![];
        for f in [format!("{name}.wasm"), format!("ui/{name}.html")] {
            let src = from.join(&f);
            let dst = to.join(&f);
            if src.exists() {
                if let Some(p) = dst.parent() {
                    std::fs::create_dir_all(p)?;
                }
                let tmp = PathBuf::from(format!("{}{tag}", dst.display()));
                std::fs::copy(&src, &tmp).with_context(|| format!("copying {}", src.display()))?;
                staged.push((tmp, dst));
            } else if dst.exists() {
                removed.push(dst);
            }
        }
        let src = from.join("src").join(name);
        let dst = to.join("src").join(name);
        if src.exists() {
            let tmp = to.join("src").join(format!("{name}{tag}"));
            if tmp.exists() {
                std::fs::remove_dir_all(&tmp)?;
            }
            copy_dir(&src, &tmp)?;
            staged.push((tmp, dst));
        } else if dst.exists() {
            removed.push(dst);
        }
        let tmp = to.join(format!("{name}.json{tag}"));
        std::fs::copy(from.join(format!("{name}.json")), &tmp)?;
        staged.push((tmp, to.join(format!("{name}.json"))));
        Ok((staged, removed))
    };
    let (staged, removed) = match stage() {
        Ok(x) => x,
        Err(e) => {
            // leave no temporary copies behind; the old version stays as it was
            for d in [to.to_path_buf(), to.join("ui"), to.join("src")] {
                for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                    if e.file_name().to_string_lossy().ends_with(&tag) {
                        let p = e.path();
                        let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                    }
                }
            }
            return Err(e);
        }
    };
    for (tmp, dst) in staged {
        if dst.is_dir() {
            std::fs::remove_dir_all(&dst)?;
        }
        std::fs::rename(&tmp, &dst).with_context(|| format!("placing {}", dst.display()))?;
    }
    for p in removed {
        if p.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let p = e.path();
        if e.file_type()?.is_dir() {
            copy_dir(&p, &to.join(e.file_name()))?;
        } else {
            std::fs::copy(&p, to.join(e.file_name()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod bundle_tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("i3cm-bundle-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn bundled_modules_are_copied_updated_and_never_override_an_operator_install() {
        let b = tmp("b");
        let d = tmp("d");
        std::fs::write(b.join("tasks.json"), "v1").unwrap();
        std::fs::write(b.join("tasks.wasm"), "w1").unwrap();
        std::fs::create_dir_all(b.join("src/tasks/i3cm")).unwrap();
        std::fs::write(b.join("src/tasks/i3cm/module.ts"), "s1").unwrap();
        // first start: copied
        assert_eq!(sync_bundled(&b, &d).unwrap(), vec!["tasks: copied from the image"]);
        assert_eq!(std::fs::read_to_string(d.join("tasks.wasm")).unwrap(), "w1");
        assert_eq!(std::fs::read_to_string(d.join("src/tasks/i3cm/module.ts")).unwrap(), "s1");
        // unchanged start: nothing to do
        assert!(sync_bundled(&b, &d).unwrap().is_empty());
        // newer image: updated
        std::fs::write(b.join("tasks.json"), "v2").unwrap();
        assert_eq!(sync_bundled(&b, &d).unwrap(), vec!["tasks: copied from the image"]);
        assert_eq!(std::fs::read_to_string(d.join("tasks.json")).unwrap(), "v2");
        // the operator installs another version: kept, also when the image changes again
        std::fs::write(d.join("tasks.json"), "operator").unwrap();
        std::fs::write(b.join("tasks.json"), "v3").unwrap();
        assert_eq!(sync_bundled(&b, &d).unwrap(), vec!["tasks: kept the version installed by the operator"]);
        assert_eq!(std::fs::read_to_string(d.join("tasks.json")).unwrap(), "operator");
        let _ = std::fs::remove_dir_all(&b);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_directory_that_cannot_be_written_is_reported_with_advice() {
        let e = ensure_writable(Path::new("/proc/i3cm-not-writable")).unwrap_err();
        assert!(format!("{e:#}").contains("mount a volume"), "{e:#}");
    }
}
