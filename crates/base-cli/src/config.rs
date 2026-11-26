//! Where the program finds server, tenant and token: `$XDG_CONFIG_HOME/base-erp/config.toml`
//! (else `~/.config/base-erp/config.toml`), mode 0600. `BASE_SERVER`, `BASE_TENANT` and
//! `BASE_TOKEN_FILE` take precedence over the file.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub server: String,
    #[serde(default)]
    pub tenant: String,
    pub token: String,
    /// Language of display names (decides names in paths); default from `LANG`.
    #[serde(default)]
    pub lang: String,
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("base-erp").join("config.toml")
}

/// `LANG=de_DE.UTF-8` -> `de-DE`.
pub fn lang_from_env() -> String {
    let l = std::env::var("LANG").unwrap_or_default();
    let l = l.split('.').next().unwrap_or("").replace('_', "-");
    if l.is_empty() || l == "C" || l == "POSIX" { "en".into() } else { l }
}

fn read_token(file: &str) -> Result<String, String> {
    Ok(std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?.trim().to_string())
}

pub fn load() -> Result<Config, String> {
    let p = path();
    let mut c: Config = match std::fs::read_to_string(&p) {
        Ok(s) => toml::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?,
        Err(_) => Config::default(),
    };
    if let Ok(v) = std::env::var("BASE_SERVER") {
        c.server = v;
    }
    if let Ok(v) = std::env::var("BASE_TENANT") {
        c.tenant = v;
    }
    if let Ok(f) = std::env::var("BASE_TOKEN_FILE") {
        c.token = read_token(&f)?;
    }
    if c.lang.is_empty() {
        c.lang = lang_from_env();
    }
    if c.server.is_empty() || c.token.is_empty() {
        return Err(format!("not configured: run base-init (or set BASE_SERVER and BASE_TOKEN_FILE); expected {}", p.display()));
    }
    Ok(c)
}

/// Write the configuration: directory 0700, file 0600 (the token never goes on a command line).
pub fn save(c: &Config) -> Result<PathBuf, String> {
    let p = path();
    let dir = p.parent().unwrap();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    let text = toml::to_string(c).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("toml.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(p)
}

pub fn token_from(file: Option<&str>) -> Result<String, String> {
    match file {
        Some(f) => read_token(f),
        None => {
            let mut s = String::new();
            std::io::stdin().read_line(&mut s).map_err(|e| e.to_string())?;
            Ok(s.trim().to_string())
        }
    }
}
