//! Installing module packages (operator, DEC-128/129): verify the signature and checksums, check
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

/// Verify a package against the trusted public keys (hex ed25519) and unpack it in memory.
pub fn verify(pkg: &[u8], sig_hex: &str, keys: &[String]) -> Result<BTreeMap<String, Vec<u8>>> {
    let digest = Sha256::digest(pkg);
    let sig = Signature::from_slice(&hex::decode(sig_hex.trim()).context("signature is not hex")?).context("bad signature")?;
    let ok = keys.iter().any(|k| {
        hex::decode(k.trim()).ok().and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()).and_then(|b| VerifyingKey::from_bytes(&b).ok()).is_some_and(|vk| vk.verify(&digest, &sig).is_ok())
    });
    if !ok {
        bail!("package signature does not match any trusted key");
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
    Ok(files)
}

/// Install a package: verify, check the merged catalog, create tables (one transaction), place files.
pub async fn install(db_url: &str, base: &Path, dir: &Path, pkg_path: &Path, keys: &[String]) -> Result<Installed> {
    let pkg = std::fs::read(pkg_path).with_context(|| format!("cannot read {}", pkg_path.display()))?;
    let sig_path = PathBuf::from(format!("{}.sig", pkg_path.display()));
    let sig = std::fs::read_to_string(&sig_path).with_context(|| format!("signature {} missing", sig_path.display()))?;
    let files = verify(&pkg, &sig, keys)?;
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
        Ok(())
    })();
    if let Err(e) = staged_ok {
        for (tmp, _, _) in &staged {
            let _ = std::fs::remove_file(tmp);
        }
        return Err(e);
    }
    tx.commit().await?;
    let mut placed = vec![];
    for (tmp, target, name) in staged {
        std::fs::rename(&tmp, &target).with_context(|| format!("placing {name} failed after the database commit; install again"))?;
        placed.push(name);
    }
    Ok(Installed { module, sha256: sha, tables: ddl.len(), files: placed })
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
    install(db_url, base, dir, &file, keys).await
}
