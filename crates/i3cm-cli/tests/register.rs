//! `i3cm studio register`, `.i3cm/studio.toml`, tokens per server and tenant, `-C`.

mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn repo(env: &Env, name: &str) -> PathBuf {
    let r = env.home.join(name);
    std::fs::create_dir_all(r.join("i3cm")).unwrap();
    std::fs::write(r.join("i3cm/module.ts"), "export const module = { name: \"probe\", requires: [\"base\"] };\n").unwrap();
    r
}

/// A tenant on the test server: (name, file with its token).
fn tenant(env: &Env, prefix: &str, development: bool) -> (String, PathBuf) {
    let name = format!("{prefix}-{}", uuid::Uuid::new_v4().simple());
    let init = rt().block_on(env.store.init_tenant_with(&name, "admin", development)).unwrap();
    let f = env.home.join(format!("{name}.token"));
    std::fs::write(&f, format!("{}\n", init.token)).unwrap();
    (name, f)
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn err(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn host(url: &str) -> String {
    url.trim_start_matches("http://").to_string()
}

#[test]
fn register_without_a_token_writes_server_and_tenant_and_ignores_the_directory() {
    let env = env(0, 0);
    env.links();
    let r = repo(&env, "a");
    std::fs::write(r.join(".gitignore"), "target/").unwrap();
    std::fs::create_dir_all(r.join("i3cm/types")).unwrap();
    // from below the repository root, with -C
    let out = env.run("i3cm", &["-C", s(&r), "-C", "i3cm/types", "studio", "register", "--server", &format!("{}/", env.url), "--tenant", "dev"], &[], None);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let f = std::fs::read_to_string(r.join(".i3cm/studio.toml")).unwrap();
    assert!(f.contains(&format!("server = \"{}\"", env.url)) && f.contains("tenant = \"dev\"") && !f.contains("token ="), "{f}");
    assert_eq!(std::fs::read_to_string(r.join(".gitignore")).unwrap(), "target/\n.i3cm/\n");
    assert!(err(&out).contains("no token for tenant dev"), "{out:?}");
    // again: the .gitignore stays as it is, the change of tenant is reported
    let out = env.run("i3cm", &["-C", s(&r), "studio", "register", "--server", &env.url, "--tenant", "dev2"], &[], None);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(text(&out).contains("(tenant dev), now"), "{out:?}");
    assert_eq!(std::fs::read_to_string(r.join(".gitignore")).unwrap(), "target/\n.i3cm/\n");
    // a token in the repository file is refused
    std::fs::write(r.join(".i3cm/studio.toml"), "tenant = \"dev\"\ntoken = \"x\"\n").unwrap();
    let out = env.run("i3cm", &["-C", s(&r), "studio", "status"], &[("I3CM_SERVER", &env.url)], None);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(err(&out).contains("tokens never go into the repository"), "{out:?}");
}

#[test]
fn register_checks_and_stores_the_token_per_server_and_tenant() {
    let env = env(0, 0);
    env.links();
    let (dev, dev_token) = tenant(&env, "dev", true);
    let (prod, prod_token) = tenant(&env, "prod", false);

    // not a development tenant: refused, nothing written
    let r = repo(&env, "p");
    let out = env.run("i3cm", &["-C", s(&r), "studio", "register", "--server", &env.url, "--tenant", &prod, "--token-file", s(&prod_token)], &[], None);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(err(&out).contains("not a development tenant"), "{out:?}");
    assert!(!r.join(".i3cm").exists() && !r.join(".gitignore").exists());
    // a token of another tenant than the one named: refused
    let out = env.run("i3cm", &["-C", s(&r), "studio", "register", "--server", &env.url, "--tenant", "other", "--token-file", s(&dev_token)], &[], None);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(err(&out).contains(&format!("belongs to tenant {dev}")), "{out:?}");
    assert!(!r.join(".i3cm").exists());

    let r = repo(&env, "d");
    let out = env.run("i3cm", &["-C", s(&r), "studio", "register", "--server", &env.url, "--tenant", &dev, "--token-file", s(&dev_token)], &[], None);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let tf = env.home.join("config/i3cm/tokens").join(host(&env.url)).join(&dev);
    assert_eq!(std::fs::read_to_string(&tf).unwrap(), std::fs::read_to_string(&dev_token).unwrap());
    assert_eq!(std::fs::metadata(&tf).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(tf.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    assert!(!std::fs::read_to_string(r.join(".i3cm/studio.toml")).unwrap().contains(std::fs::read_to_string(&dev_token).unwrap().trim()));
    // a second register finds the stored token and checks it again, quietly
    let out = env.run("i3cm", &["-C", s(&r), "studio", "register", "--tenant", &dev, "--server", &env.url], &[], None);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(!err(&out).contains("no token"), "{out:?}");
}

#[test]
fn each_repository_works_with_its_own_tenant() {
    let env = env(0, 0);
    env.links();
    env.init(); // config.toml: tenant "t" with the token of the test tenant
    let (d1, t1) = tenant(&env, "one", true);
    let (d2, t2) = tenant(&env, "two", true);
    let a = repo(&env, "a");
    let b = repo(&env, "b");
    for (r, d, t) in [(&a, &d1, &t1), (&b, &d2, &t2)] {
        let out = env.run("i3cm", &["-C", s(r), "studio", "register", "--tenant", d, "--token-file", s(t)], &[], None);
        assert_eq!(out.status.code(), Some(0), "the server comes from config.toml: {out:?}");
    }
    for (r, d) in [(&a, &d1), (&b, &d2)] {
        let out = env.run("i3cm", &["-C", s(r), "studio", "status"], &[], None);
        let first = text(&out).lines().next().unwrap_or("").to_string();
        assert!(first.contains(&format!("tenant {d} (from {})", r.join(".i3cm/studio.toml").display())), "{out:?}");
        assert!(err(&out).contains("i3cm: tenant from") && err(&out).contains("covers the one in"), "{out:?}");
        assert_ne!(out.status.code(), Some(3), "the token's tenant is the configured one: {out:?}");
    }
    // the token of config.toml is not used for another tenant
    let c = repo(&env, "c");
    std::fs::create_dir_all(c.join(".i3cm")).unwrap();
    std::fs::write(c.join(".i3cm/studio.toml"), "tenant = \"elsewhere\"\n").unwrap();
    let out = env.run("i3cm", &["-C", s(&c), "studio", "load"], &[], None);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(err(&out).contains("no token for tenant elsewhere"), "{out:?}");
    // a token of another tenant (here from the environment) stops before anything is written
    let out = env.run("i3cm", &["-C", s(&a), "studio", "save"], &[("I3CM_TOKEN_FILE", s(&t2))], None);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(err(&out).contains(&format!("belongs to tenant {d2}, not to {d1}")), "{out:?}");
    // config.toml names tenant "t", but the token belongs to the test tenant: stopped as well
    let out = env.run("i3cm", &["-C", s(&repo(&env, "plain")), "studio", "status"], &[], None);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    // i3cm config in a repository shows where studio takes its values from
    let out = env.run("i3cm", &["config"], &[], None);
    assert!(text(&out).contains("tenant  t "), "outside a repository: {out:?}");
}

#[test]
fn dash_c_is_for_studio_and_scaffold() {
    let env = env(0, 0);
    env.links();
    let r = repo(&env, "a");
    let out = env.run("i3cm", &["-C", s(&r), "ls", "/"], &[], None);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(err(&out).contains("-C applies to studio and scaffold"), "{out:?}");
    let out = env.run("i3cm", &["-C", s(&r.join("missing")), "studio", "status"], &[], None);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(err(&out).contains("not a directory"), "{out:?}");
    let out = env.run("i3cm", &["-C"], &[], None);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let out = env.run("i3cm", &["-C", s(&r), "scaffold", "ui"], &[], None);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(r.join("i3cm/ui/probe.html").exists());
}
