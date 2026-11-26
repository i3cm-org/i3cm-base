//! The program without FUSE: base-init, the base-* tools, PATH hand-over, option errors.

mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn init_stores_the_token_privately_and_tools_use_it() {
    let env = env(1, 2);
    env.links();
    env.init();
    let f = env.config_file();
    assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(f.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    let cfg = std::fs::read_to_string(&f).unwrap();
    assert!(cfg.contains(&env.token) && cfg.contains("lang = \"de-DE\""), "{cfg}");

    let out = env.run("base-ls", &["/base:mailbox"], &[], None);
    assert_eq!(text(&out), "2026-11-20\n2026-11-20.md\n");
    let out = env.run("base-grep", &["-rn", "Rechnung", "."], &[("BASE_PWD", "/base:mailbox/2026-11-20")], None);
    assert_eq!(text(&out).lines().count(), 2, "{out:?}");
    assert!(text(&out).contains("./s0@example.org - Betreff 0-0/mail.md:3:Rechnung 0-0\n"), "{out:?}");
    let out = env.run("base", &["cat", "/base:mailbox/2026-11-20/s1@example.org - Betreff 0-1/mail.eml"], &[], None);
    assert_eq!(text(&out), "From: s1\r\n\r\nHallo 0-1");
    // a binary file comes back byte for byte
    let pdf = env.run("base-cat", &["/base:mailbox/2026-11-20/s1@example.org - Betreff 0-1/Anhänge/file1.pdf/file1.pdf"], &[], None);
    assert_eq!(pdf.stdout, b"%PDF 0-1-1");

    // a wrong token is rejected by base-init and nothing is written
    let out = env.run("base-init", &["--server", &env.url, "--tenant", "t"], &[("XDG_CONFIG_HOME", env.home.join("other").to_str().unwrap())], Some("bt_wrong\n"));
    assert!(!out.status.success());
    assert!(!env.home.join("other/base-erp/config.toml").exists());
}

#[test]
fn unknown_options_fail_with_the_list_before_any_request() {
    let env = env(0, 0);
    env.links();
    let out = env.run("base-grep", &["-P", "x", "/"], &[("BASE_SERVER", "http://127.0.0.1:1")], None);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unsupported option '-P'") && err.contains("-r"), "{err}");
}

#[test]
fn plain_names_hand_over_outside_a_mount() {
    let env = env(0, 0);
    let links = env.links();
    // a fake system tool later in PATH
    let sys = env.home.join("sys");
    std::fs::create_dir_all(&sys).unwrap();
    std::fs::write(sys.join("ls"), "#!/bin/sh\necho \"system ls $*\"\n").unwrap();
    std::fs::set_permissions(sys.join("ls"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", links.display(), sys.display());
    let out = env.run("ls", &["-la", "/tmp"], &[("PATH", &path)], None);
    assert_eq!(text(&out), "system ls -la /tmp\n", "{out:?}");
    // options our ls does not know go to the system tool as well (not a mount)
    let out = env.run("ls", &["--color=auto", "/tmp"], &[("PATH", &path)], None);
    assert_eq!(text(&out), "system ls --color=auto /tmp\n");
    // without another program of that name
    let out = env.run("ls", &["/tmp"], &[("PATH", links.to_str().unwrap())], None);
    assert_eq!(out.status.code(), Some(127));
}
