//! Where the program finds server, tenant and token. Files in `$XDG_CONFIG_HOME/base-erp/`
//! (else `~/.config/base-erp/`, directory 0700): `config.toml` with server, tenant and
//! language, and `token` (0600) with the token alone. Sources have a fixed rank: the
//! environment (`BASE_SERVER`, `BASE_TENANT`, `BASE_TOKEN_FILE`) first, then the files. The
//! higher source wins; when it covers a lower one the program says so on stderr.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub server: String,
    #[serde(default)]
    pub tenant: String,
    /// Kept in the file `token`, never in `config.toml`.
    #[serde(default, skip_serializing)]
    pub token: String,
    /// Language of display names (decides names in paths); default from `LANG`.
    #[serde(default)]
    pub lang: String,
}

pub fn dir() -> PathBuf {
    path().parent().unwrap().to_path_buf()
}

pub fn token_path() -> PathBuf {
    dir().join("token")
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
    let legacy = std::mem::take(&mut c.token);
    let tp = token_path();
    let file_token = match std::fs::read_to_string(&tp) {
        Ok(t) => Some(t.trim().to_string()),
        Err(_) if !legacy.is_empty() => Some(legacy),
        Err(_) => None,
    };
    let covers = |what: &str, var: &str, lower: &str| eprintln!("base: {what} from {var} covers the one in {lower}");
    if let Ok(v) = std::env::var("BASE_SERVER") {
        if !c.server.is_empty() && c.server != v {
            covers("server", "BASE_SERVER", &p.display().to_string());
        }
        c.server = v;
    }
    if let Ok(v) = std::env::var("BASE_TENANT") {
        if !c.tenant.is_empty() && c.tenant != v {
            covers("tenant", "BASE_TENANT", &p.display().to_string());
        }
        c.tenant = v;
    }
    match std::env::var("BASE_TOKEN_FILE") {
        Ok(f) => {
            c.token = read_token(&f)?;
            if file_token.is_some() {
                covers("token", "BASE_TOKEN_FILE", &tp.display().to_string());
            }
        }
        Err(_) => c.token = file_token.unwrap_or_default(),
    }
    if c.lang.is_empty() {
        c.lang = lang_from_env();
    }
    if c.server.is_empty() || c.token.is_empty() {
        return Err(format!("not configured: run base-init (or set BASE_SERVER and BASE_TOKEN_FILE); expected {} and {}", p.display(), tp.display()));
    }
    Ok(c)
}

fn write_private(p: &std::path::Path, text: &str) -> Result<(), String> {
    let tmp = p.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Write the configuration: directory 0700, `config.toml` without the token, `token` 0600 (the
/// token never goes on a command line).
pub fn save(c: &Config) -> Result<PathBuf, String> {
    let p = path();
    let dir = p.parent().unwrap();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    write_private(&token_path(), &format!("{}\n", c.token))?;
    write_private(&p, &toml::to_string(c).map_err(|e| e.to_string())?)?;
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
