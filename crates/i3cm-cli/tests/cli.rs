//! The program without FUSE: i3cm-init, the i3cm-* tools, PATH hand-over, option errors.

mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn init_stores_the_token_privately_and_tools_use_it() {
    let env = env(1, 2);
    env.links();
    env.init();
    let f = env.config_file();
    let tf = f.with_file_name("token");
    assert_eq!(std::fs::metadata(&tf).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(f.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::read_to_string(&tf).unwrap().trim(), env.token);
    let cfg = std::fs::read_to_string(&f).unwrap();
    assert!(!cfg.contains(&env.token) && cfg.contains("lang = \"de-DE\"") && cfg.contains("server = "), "the token is not in config.toml: {cfg}");

    let out = env.run("i3cm-ls", &["/base:mailbox"], &[], None);
    assert_eq!(text(&out), "2026-11-20\n2026-11-20.md\n");
    let out = env.run("i3cm-grep", &["-rn", "Rechnung", "."], &[("I3CM_PWD", "/base:mailbox/2026-11-20")], None);
    assert_eq!(text(&out).lines().count(), 2, "{out:?}");
    assert!(text(&out).contains("./s0@example.org - Betreff 0-0/mail.md:3:Rechnung 0-0\n"), "{out:?}");
    let out = env.run("i3cm", &["cat", "/base:mailbox/2026-11-20/s1@example.org - Betreff 0-1/mail.eml"], &[], None);
    assert_eq!(text(&out), "From: s1\r\n\r\nHallo 0-1");
    // a binary file comes back byte for byte
    let pdf = env.run("i3cm-cat", &["/base:mailbox/2026-11-20/s1@example.org - Betreff 0-1/Anhänge/file1.pdf/file1.pdf"], &[], None);
    assert_eq!(pdf.stdout, b"%PDF 0-1-1");

    // the environment ranks above the token file and says that it covers it
    let other = env.home.join("other-token");
    std::fs::write(&other, "bt_wrong\n").unwrap();
    let out = env.run("i3cm-ls", &["/"], &[("I3CM_TOKEN_FILE", other.to_str().unwrap())], None);
    assert!(!out.status.success(), "the wrong token from the environment wins");
    assert!(String::from_utf8_lossy(&out.stderr).contains("token from I3CM_TOKEN_FILE covers"), "{out:?}");
    let out = env.run("i3cm-ls", &["/"], &[], None);
    assert!(out.status.success() && out.stderr.is_empty(), "{out:?}");

    // a wrong token is rejected by i3cm-init and nothing is written
    let out = env.run("i3cm-init", &["--server", &env.url, "--tenant", "t"], &[("XDG_CONFIG_HOME", env.home.join("other").to_str().unwrap())], Some("bt_wrong\n"));
    assert!(!out.status.success());
    assert!(!env.home.join("other/i3cm/config.toml").exists());
}

#[test]
fn unknown_options_fail_with_the_list_before_any_request() {
    let env = env(0, 0);
    env.links();
    let out = env.run("i3cm-grep", &["-P", "x", "/"], &[("I3CM_SERVER", "http://127.0.0.1:1")], None);
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
