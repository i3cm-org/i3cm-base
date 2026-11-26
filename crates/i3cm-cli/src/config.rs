//! Where the program finds server, tenant and token. Files in `$XDG_CONFIG_HOME/i3cm/`
//! (else `~/.config/i3cm/`, directory 0700): `config.toml` with server, tenant and
//! language, and `token` (0600) with the token alone. Sources have a fixed rank: the
//! environment (`I3CM_SERVER`, `I3CM_TENANT`, `I3CM_TOKEN_FILE`) first, then the files. The
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
    base.join("i3cm").join("config.toml")
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
    let covers = |what: &str, var: &str, lower: &str| eprintln!("i3cm: {what} from {var} covers the one in {lower}");
    if let Ok(v) = std::env::var("I3CM_SERVER") {
        if !c.server.is_empty() && c.server != v {
            covers("server", "I3CM_SERVER", &p.display().to_string());
        }
        c.server = v;
    }
    if let Ok(v) = std::env::var("I3CM_TENANT") {
        if !c.tenant.is_empty() && c.tenant != v {
            covers("tenant", "I3CM_TENANT", &p.display().to_string());
        }
        c.tenant = v;
    }
    match std::env::var("I3CM_TOKEN_FILE") {
        Ok(f) => {
            c.token = read_token(&f)?;
            if file_token.is_some() {
                covers("token", "I3CM_TOKEN_FILE", &tp.display().to_string());
            }
        }
        Err(_) => c.token = file_token.unwrap_or_default(),
    }
    if let Ok(v) = std::env::var("I3CM_LANG") {
        c.lang = v;
    }
    if c.lang.is_empty() {
        c.lang = lang_from_env();
    }
    if c.server.is_empty() || c.token.is_empty() {
        return Err(format!("not configured: run i3cm-init (or set I3CM_SERVER and I3CM_TOKEN_FILE); expected {} and {}", p.display(), tp.display()));
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

/// Every configuration value with its source, for `i3cm config` (the token itself is never shown).
pub fn describe() -> Vec<(String, String, String)> {
    let p = path();
    let file: Config = std::fs::read_to_string(&p).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
    let fp = p.display().to_string();
    let mut out = vec![];
    let mut val = |name: &str, var: &str, from_file: &str, default: Option<String>| {
        let (v, src) = match std::env::var(var) {
            Ok(v) => (v, format!("environment {var}")),
            Err(_) if !from_file.is_empty() => (from_file.to_string(), fp.clone()),
            Err(_) => match &default {
                Some(d) => (d.clone(), "default".into()),
                None => ("(not set)".into(), "-".into()),
            },
        };
        out.push((name.to_string(), v, src));
    };
    val("server", "I3CM_SERVER", &file.server, None);
    val("tenant", "I3CM_TENANT", &file.tenant, None);
    val("lang", "I3CM_LANG", &file.lang, Some(lang_from_env()));
    let tp = token_path();
    let token = match std::env::var("I3CM_TOKEN_FILE") {
        Ok(f) => ("(hidden)".to_string(), format!("environment I3CM_TOKEN_FILE ({f})")),
        Err(_) if tp.exists() => ("(hidden)".to_string(), tp.display().to_string()),
        Err(_) => ("(not set)".to_string(), "-".to_string()),
    };
    out.push(("token".into(), token.0, token.1));
    let pwd = match std::env::var("I3CM_PWD") {
        Ok(v) if !v.is_empty() => (v, "environment I3CM_PWD".to_string()),
        _ => match read_pwd() {
            Some(v) => (v, pwd_file().display().to_string()),
            None => ("/".into(), "default".into()),
        },
    };
    out.push(("pwd".into(), pwd.0, pwd.1));
    out
}

/// State file of `i3cm-cd` for this shell session (the session id of the calling shell).
pub fn pwd_file() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    let sid = unsafe { libc::getsid(0) };
    base.join("i3cm").join(format!("pwd-{sid}"))
}

pub fn read_pwd() -> Option<String> {
    std::fs::read_to_string(pwd_file()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn write_pwd(path: &str) -> Result<(), String> {
    let f = pwd_file();
    let dir = f.parent().unwrap();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&f, format!("{path}\n")).map_err(|e| format!("{}: {e}", f.display()))
}

/// Working path of the `i3cm-*` tools: `I3CM_PWD`, else the one set by `i3cm-cd`, else `/`.
pub fn pwd() -> String {
    std::env::var("I3CM_PWD").ok().filter(|s| !s.is_empty()).or_else(read_pwd).unwrap_or_else(|| "/".into())
}
