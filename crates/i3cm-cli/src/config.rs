//! Where the program finds server, tenant and token. Files in `$XDG_CONFIG_HOME/i3cm/`
//! (else `~/.config/i3cm/`, directory 0700): `config.toml` with server, tenant and
//! language, and `token` (0600) with the token alone. Sources have a fixed rank: the
//! environment (`I3CM_SERVER`, `I3CM_TENANT`, `I3CM_TOKEN_FILE`) first, then the files. The
//! higher source wins; when it covers a lower one the program says so on stderr.
//!
//! `i3cm studio` adds a source between the two: `.i3cm/studio.toml` in the module repository
//! (server and tenant, never a token), and looks for the token per server and tenant in
//! `tokens/<host>[:<port>]/<tenant>`, because a token belongs to exactly one tenant.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

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
/// In a module repository with `.i3cm/studio.toml` (`repo`), server, tenant and token are the
/// ones `i3cm studio` uses there.
pub fn describe(repo: Option<&Path>) -> Vec<(String, String, String)> {
    let p = path();
    let file: Config = std::fs::read_to_string(&p).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
    let fp = p.display().to_string();
    let mut out = vec![];
    if let Some((r, rc)) = repo.and_then(|r| read_repo(r).ok().flatten().map(|c| (r, c))) {
        let rp = repo_file(r).display().to_string();
        let mut eff = vec![];
        for (name, var, rv, uv) in [("server", "I3CM_SERVER", &rc.server, &file.server), ("tenant", "I3CM_TENANT", &rc.tenant, &file.tenant)] {
            let ev = std::env::var(var).ok();
            let label = format!("environment {var}");
            let (v, from, _) = rank(name, &[(&label, ev.as_deref()), (&rp, rv.as_deref()), (&fp, Some(uv.as_str()))]);
            eff.push(v.clone().unwrap_or_default());
            out.push((name.to_string(), v.unwrap_or_else(|| "(not set)".into()), from));
        }
        out.push(("lang".into(), std::env::var("I3CM_LANG").ok().or(Some(file.lang.clone())).filter(|l| !l.is_empty()).unwrap_or_else(lang_from_env), "-".into()));
        let token = match std::env::var("I3CM_TOKEN_FILE") {
            Ok(f) => ("(hidden)".to_string(), format!("environment I3CM_TOKEN_FILE ({f})")),
            Err(_) => match token_for(&eff[0], &eff[1], &file) {
                Some((_, from)) => ("(hidden)".to_string(), from),
                None => ("(not set)".to_string(), "-".to_string()),
            },
        };
        out.push(("token".into(), token.0, token.1));
        return out;
    }
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

/// Where `i3cm studio` found server, tenant and token (for `status` and error messages).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Origin {
    pub server: String,
    pub tenant: String,
    pub token: String,
}

/// `.i3cm/studio.toml` of a module repository: server and tenant, nothing else.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

pub const REPO_DIR: &str = ".i3cm";

pub fn repo_file(repo: &Path) -> PathBuf {
    repo.join(REPO_DIR).join("studio.toml")
}

/// Parse `.i3cm/studio.toml`; a `token` key gets its own message.
pub fn parse_repo(text: &str, file: &Path) -> Result<RepoConfig, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| format!("{}: {e}", file.display()))?;
    if table.contains_key("token") {
        return Err(format!("{}: tokens never go into the repository; remove the key `token` and store the token with i3cm studio register --token-file FILE", file.display()));
    }
    toml::from_str(text).map_err(|e| format!("{}: {e}", file.display()))
}

pub fn read_repo(repo: &Path) -> Result<Option<RepoConfig>, String> {
    let f = repo_file(repo);
    match std::fs::read_to_string(&f) {
        Ok(s) => parse_repo(&s, &f).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", f.display())),
    }
}

pub fn repo_text(c: &RepoConfig) -> String {
    format!("# Written by `i3cm studio register`. Not committed. The token is never stored here.\n{}", toml::to_string(c).unwrap_or_default())
}

/// The value of the highest source that has one (sources from high to low, `(label, value)`;
/// empty counts as missing), its label, and a notice for every lower source it covers with a
/// different value.
pub fn rank(what: &str, sources: &[(&str, Option<&str>)]) -> (Option<String>, String, Vec<String>) {
    let present: Vec<(&str, &str)> = sources.iter().filter_map(|(l, v)| v.filter(|v| !v.is_empty()).map(|v| (*l, v))).collect();
    let Some(&(label, value)) = present.first() else {
        return (None, "-".into(), vec![]);
    };
    let notices = present[1..].iter().filter(|(_, v)| *v != value).map(|(l, _)| format!("i3cm: {what} from {label} covers the one in {l}")).collect();
    (Some(value.to_string()), label.to_string(), notices)
}

fn same_server(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// `https://Dev.Example.org:8443/x` -> `dev.example.org:8443`; the port only if it is not the
/// scheme's default.
pub fn host_of(server: &str) -> Result<String, String> {
    let (scheme, rest) = server.split_once("://").ok_or_else(|| format!("server {server}: expected http://… or https://…"))?;
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" => ":80",
        "https" => ":443",
        _ => return Err(format!("server {server}: expected http://… or https://…")),
    };
    let host = hostport.strip_suffix(default).unwrap_or(&hostport).to_string();
    if host.is_empty() || host.contains(['@', '\\']) || host.starts_with('.') {
        return Err(format!("server {server}: no usable host"));
    }
    Ok(host)
}

/// A tenant name usable as a file name.
pub fn check_tenant(name: &str) -> Result<(), String> {
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\', '\0']) {
        return Err(format!("tenant name {name:?} is not usable (empty, leading dot, or a slash)"));
    }
    Ok(())
}

/// `tokens/<host>[:<port>]/<tenant>` in the configuration directory.
pub fn tenant_token_path(server: &str, tenant: &str) -> Result<PathBuf, String> {
    check_tenant(tenant)?;
    Ok(dir().join("tokens").join(host_of(server)?).join(tenant))
}

/// The token for `server` and `tenant` (not `I3CM_TOKEN_FILE`): `tokens/<host>/<tenant>`, else
/// `token` only if the pair is the one in `config.toml` (the one `i3cm init` stored it for).
pub fn token_for(server: &str, tenant: &str, user: &Config) -> Option<(String, String)> {
    let per_tenant = tenant_token_path(server, tenant).ok().and_then(|p| std::fs::read_to_string(&p).ok().map(|t| (t.trim().to_string(), p.display().to_string())));
    if let Some(found) = per_tenant.filter(|(t, _)| !t.is_empty()) {
        return Some(found);
    }
    if same_server(server, &user.server) && tenant == user.tenant {
        let tp = token_path();
        let t = std::fs::read_to_string(&tp).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).or_else(|| Some(user.token.clone()).filter(|t| !t.is_empty()));
        return t.map(|t| (t, tp.display().to_string()));
    }
    None
}

fn read_user() -> Result<Config, String> {
    let p = path();
    match std::fs::read_to_string(&p) {
        Ok(s) => toml::from_str(&s).map_err(|e| format!("{}: {e}", p.display())),
        Err(_) => Ok(Config::default()),
    }
}

/// Configuration for `i3cm studio` in `repo`: server and tenant each from the environment, then
/// `.i3cm/studio.toml`, then `config.toml`; the token from `I3CM_TOKEN_FILE`, else per server and
/// tenant (`token_for`).
pub fn load_for(repo: Option<&Path>) -> Result<(Config, Origin), String> {
    let user = read_user()?;
    let up = path().display().to_string();
    let rc = match repo {
        Some(r) => read_repo(r)?,
        None => None,
    };
    let rp = repo.map(|r| repo_file(r).display().to_string()).unwrap_or_default();
    let (rs, rt) = rc.as_ref().map(|c| (c.server.as_deref(), c.tenant.as_deref())).unwrap_or((None, None));
    let env_server = std::env::var("I3CM_SERVER").ok();
    let env_tenant = std::env::var("I3CM_TENANT").ok();
    let (server, server_from, n1) = rank("server", &[("I3CM_SERVER", env_server.as_deref()), (&rp, rs), (&up, Some(user.server.as_str()))]);
    let (tenant, tenant_from, n2) = rank("tenant", &[("I3CM_TENANT", env_tenant.as_deref()), (&rp, rt), (&up, Some(user.tenant.as_str()))]);
    for n in n1.iter().chain(&n2) {
        eprintln!("{n}");
    }
    let server = server.ok_or_else(|| format!("no server: run i3cm studio register --server URL --tenant NAME (or i3cm-init); expected {up}"))?;
    let tenant = tenant.ok_or_else(|| "no tenant: run i3cm studio register --tenant NAME".to_string())?;
    check_tenant(&tenant)?;
    let (token, token_from) = match std::env::var("I3CM_TOKEN_FILE") {
        Ok(f) => (read_token(&f)?, format!("I3CM_TOKEN_FILE ({f})")),
        Err(_) => token_for(&server, &tenant, &user).ok_or_else(|| {
            format!("no token for tenant {tenant} on {}; store it with i3cm studio register --tenant {tenant} --token-file FILE", host_of(&server).unwrap_or_else(|_| server.clone()))
        })?,
    };
    let mut lang = std::env::var("I3CM_LANG").ok().filter(|l| !l.is_empty()).unwrap_or(user.lang);
    if lang.is_empty() {
        lang = lang_from_env();
    }
    Ok((Config { server, tenant, token, lang }, Origin { server: server_from, tenant: tenant_from, token: token_from }))
}

/// Write a file atomically (hidden temporary file beside it, then rename); `mode` for new files.
pub fn write_atomic(p: &Path, text: &str, mode: u32) -> Result<(), String> {
    let name = p.file_name().and_then(|n| n.to_str()).ok_or_else(|| format!("{}: no file name", p.display()))?;
    let tmp = p.with_file_name(format!(".{name}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Store a token for `server` and `tenant`: directories 0700, file 0600.
pub fn save_tenant_token(server: &str, tenant: &str, token: &str) -> Result<PathBuf, String> {
    let p = tenant_token_path(server, tenant)?;
    let d = p.parent().unwrap();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d).map_err(|e| format!("{}: {e}", d.display()))?;
    for x in [d, d.parent().unwrap(), &dir()] {
        let _ = std::fs::set_permissions(x, std::fs::Permissions::from_mode(0o700));
    }
    write_atomic(&p, &format!("{token}\n"), 0o600)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_higher_source_wins_and_says_what_it_covers() {
        let (v, from, n) = rank("tenant", &[("I3CM_TENANT", None), ("repo", Some("dev")), ("user", Some("acme"))]);
        assert_eq!((v.as_deref(), from.as_str()), (Some("dev"), "repo"));
        assert_eq!(n, vec!["i3cm: tenant from repo covers the one in user"]);
        let (v, from, n) = rank("tenant", &[("I3CM_TENANT", Some("x")), ("repo", Some("dev")), ("user", Some("acme"))]);
        assert_eq!((v.as_deref(), from.as_str(), n.len()), (Some("x"), "I3CM_TENANT", 2));
        // equal values cover nothing, empty ones count as missing
        let (v, from, n) = rank("server", &[("I3CM_SERVER", Some("")), ("repo", Some("a")), ("user", Some("a"))]);
        assert_eq!((v.as_deref(), from.as_str(), n.len()), (Some("a"), "repo", 0));
        let (v, from, _) = rank("server", &[("I3CM_SERVER", None), ("repo", None), ("user", Some("u"))]);
        assert_eq!((v.as_deref(), from.as_str()), (Some("u"), "user"));
        let (v, from, n) = rank("server", &[("I3CM_SERVER", None), ("repo", None), ("user", Some(""))]);
        assert_eq!((v, from.as_str(), n.len()), (None, "-", 0));
    }

    #[test]
    fn hosts_for_token_paths() {
        assert_eq!(host_of("https://Dev.Example.org/").unwrap(), "dev.example.org");
        assert_eq!(host_of("https://dev.example.org:443").unwrap(), "dev.example.org");
        assert_eq!(host_of("https://dev.example.org:8443/x").unwrap(), "dev.example.org:8443");
        assert_eq!(host_of("http://127.0.0.1:80").unwrap(), "127.0.0.1");
        assert_eq!(host_of("http://127.0.0.1:8090").unwrap(), "127.0.0.1:8090");
        for bad in ["dev.example.org", "ftp://x", "https://", "https:///x", "https://u@x"] {
            assert!(host_of(bad).is_err(), "{bad}");
        }
        for bad in ["", ".hidden", "..", "a/b", "a\\b"] {
            assert!(check_tenant(bad).is_err(), "{bad:?}");
        }
        assert!(check_tenant("acme-dev").is_ok());
    }

    #[test]
    fn the_repository_file_holds_server_and_tenant_only() {
        let f = Path::new("/r/.i3cm/studio.toml");
        let c = parse_repo("server = \"https://x\"\ntenant = \"dev\"\n", f).unwrap();
        assert_eq!(c, RepoConfig { server: Some("https://x".into()), tenant: Some("dev".into()) });
        assert_eq!(parse_repo(&repo_text(&c), f).unwrap(), c, "written text reads back");
        assert!(parse_repo("tenant = \"dev\"\ntoken = \"secret\"\n", f).unwrap_err().contains("tokens never go into the repository"));
        assert!(parse_repo("tenant = \"dev\"\nlang = \"de\"\n", f).is_err(), "unknown keys are refused");
    }
}
