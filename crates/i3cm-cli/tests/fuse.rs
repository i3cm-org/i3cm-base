//! FUSE acceptance: the driver reproduces the API faithfully, and a recursive reader costs
//! requests per directory, not per file. Needs /dev/fuse and fusermount3 (run with
//! `cargo test -p i3cm-cli --test fuse -- --ignored --test-threads 1`).

mod common;
use i3cm_tools::{project, Named, Slot};
use common::*;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

fn mount(env: &Env, name: &str, ttl: &str) -> (PathBuf, Child) {
    let mp = env.home.join(name);
    std::fs::create_dir_all(&mp).unwrap();
    let child = Command::new(env.home.join("bin/i3cm-mount"))
        .arg(&mp)
        .args(["--ttl", ttl])
        .env("XDG_CONFIG_HOME", env.home.join("config"))
        .env("LANG", "de_DE.UTF-8")
        .spawn()
        .unwrap();
    let key = format!(" {} ", mp.display());
    for _ in 0..100 {
        if std::fs::read_to_string("/proc/self/mountinfo").unwrap().lines().any(|l| l.contains(&key) && (l.contains(" - fuse.i3cm ") || l.contains(" - fuse i3cm "))) {
            return (mp, child);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("mount did not appear");
}

fn umount(mp: &Path, mut child: Child) {
    for (cmd, args) in [("fusermount3", vec!["-u"]), ("fusermount", vec!["-u"]), ("umount", vec![])] {
        if Command::new(cmd).args(&args).arg(mp).status().map(|s| s.success()).unwrap_or(false) {
            break;
        }
    }
    let _ = child.wait();
}

fn requests(env: &Env) -> u64 {
    rt().block_on(env.admin.metrics()).unwrap()["requests"].as_u64().unwrap()
}

fn sh(cmd: &str) -> String {
    let out = Command::new("sh").args(["-c", cmd]).output().unwrap();
    assert!(out.status.success(), "{cmd}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
#[ignore]
fn requests_grow_with_directories_not_files() {
    let env = env(3, 10);
    env.links();
    env.init();
    let mut report = vec![];
    for (label, cmd) in [
        ("find", "find '{}' -printf '%y\\n'"),
        ("ls -lR", "ls -lR '{}' >/dev/null; find '{}' -printf '%y\\n'"),
        ("cat all files", "find '{}' -type f -exec cat {{}} + >/dev/null; find '{}' -printf '%y\\n'"),
    ] {
        let (mp, child) = mount(&env, "mnt", "600");
        let dir = mp.join("sample:mailbox");
        let before = requests(&env);
        let out = sh(&cmd.replace("'{}'", &format!("'{}'", dir.display())).replace("{{}}", "{}"));
        let cost = requests(&env) - before;
        let dirs = out.lines().filter(|l| *l == "d").count() as u64;
        let files = out.lines().filter(|l| *l == "f").count() as u64;
        umount(&mp, child);
        report.push(format!("{label}: {dirs} directories, {files} files, {cost} requests"));
        assert!(cost <= 2 * dirs + 2, "{label}: {cost} requests for {dirs} directories");
    }
    println!("{}", report.join("\n"));
}

/// Compare the mounted tree below `fs` with the API, `depth` levels deep.
fn compare(env: &Env, aspect: &str, node: &str, fs: &Path, depth: u32, seen: &mut u64) {
    let children = rt().block_on(env.admin.aspect_children(aspect, node)).unwrap();
    let named: Vec<Named> = children.iter().map(|c| Named { name: &c.name, file: c.file, fallback: c.fallback_name.as_deref(), key: &c.node }).collect();
    let expected = project(&named);
    let mut want: Vec<&str> = expected.iter().map(|e| e.0.as_str()).collect();
    let mut got: Vec<String> = std::fs::read_dir(fs).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    want.sort();
    got.sort();
    assert_eq!(got, want, "listing of {}", fs.display());
    for (name, slot) in &expected {
        let p = fs.join(name);
        let meta = std::fs::symlink_metadata(&p).unwrap();
        *seen += 1;
        match *slot {
            Slot::Dir(i) => {
                assert!(meta.is_dir(), "{}", p.display());
                if depth > 0 {
                    compare(env, aspect, &children[i].node, &p, depth - 1, seen);
                }
            }
            Slot::Index(i) | Slot::File(i) => {
                let c = &children[i];
                let api = match slot {
                    Slot::Index(i) => rt().block_on(env.admin.aspect_index(aspect, &c.node, i3cm_tools::dir_name(&expected, *i))).unwrap().into_bytes(),
                    _ => rt().block_on(env.admin.aspect_file(aspect, &c.node, &c.display_name)).unwrap(),
                };
                let bytes = std::fs::read(&p).unwrap();
                assert_eq!(bytes, api, "bytes of {}", p.display());
                assert_eq!(meta.len(), bytes.len() as u64, "size of {}", p.display());
                // an index under another name than the listing assumed (collision) has its own inode
                let want = match slot {
                    Slot::Index(i) if i3cm_tools::dir_name(&expected, *i) != Some(c.name.as_str()) => Some(i3cm_tools::ino(&["i", aspect, &c.node, i3cm_tools::dir_name(&expected, *i).unwrap()])),
                    _ => c.ino,
                };
                assert_eq!(Some(meta.ino()), want, "inode of {}", p.display());
                let object = c.id.is_some() && (c.file || c.node.starts_with("o:"));
                assert_eq!(meta.nlink(), if object { 2 } else { 1 }, "nlink of {}", p.display());
            }
        }
    }
}

#[test]
#[ignore]
fn the_driver_reproduces_the_api() {
    let env = env(2, 3);
    env.links();
    env.init();
    let (mp, child) = mount(&env, "mnt2", "600");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut root: Vec<String> = std::fs::read_dir(&mp).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        root.sort();
        for a in ["_id", "_type", "sample:mailbox", "sample:recipients"] {
            assert!(root.contains(&a.to_string()) && root.contains(&format!("{a}.md")), "{root:?}");
        }
        let mut seen = 0;
        compare(&env, "sample:mailbox", "", &mp.join("sample:mailbox"), 10, &mut seen);
        compare(&env, "sample:recipients", "", &mp.join("sample:recipients"), 10, &mut seen);
        compare(&env, "_type", "", &mp.join("_type"), 3, &mut seen);
        println!("compared {seen} entries");

        // equal content, equal inode: the text of a mail in two views (du and tar count it once)
        let day = mp.join("sample:mailbox/2026-11-20");
        let mail = std::fs::read_dir(&day).unwrap().map(|e| e.unwrap().path()).find(|p| p.is_dir()).unwrap();
        let name = mail.file_name().unwrap().to_string_lossy().into_owned();
        let other = std::fs::read_dir(mp.join("sample:recipients")).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_dir()).map(|g| g.join(&name)).find(|p| p.exists()).unwrap();
        assert_eq!(std::fs::metadata(mail.join("mail.md")).unwrap().ino(), std::fs::metadata(other.join("mail.md")).unwrap().ino());
        let found = sh(&format!("find '{}' -samefile '{}'", mp.join("sample:recipients").display(), mail.join("mail.md").display()));
        assert_eq!(found.lines().count(), 1, "{found}");

        // an object by its id
        let id = rt().block_on(env.admin.aspect_children("sample:mailbox", &rt().block_on(env.admin.aspect_children("sample:mailbox", "")).unwrap()[0].node)).unwrap()[0].id.unwrap();
        assert!(std::fs::read_to_string(mp.join(format!("_id/{id}/mail.md"))).unwrap().starts_with("# Hallo"));
        assert!(std::fs::read_to_string(mp.join(format!("_id/{id}.md"))).unwrap().contains(&format!("_id: \"{id}\"")));

        // our tools on the mount go to the server directly: one request per call
        let path = format!("{}:/usr/bin:/bin", env.home.join("bin").display());
        let before = requests(&env);
        let out = env.run("grep", &["-rl", "Rechnung 1-2", mp.join("sample:mailbox").to_str().unwrap()], &[("PATH", &path)], None);
        assert_eq!(requests(&env) - before, 1);
        assert_eq!(text(&out).lines().count(), 1, "{out:?}");
        assert!(text(&out).starts_with(mp.to_str().unwrap()), "paths are shown as given: {out:?}");
        let before = requests(&env);
        let out = env.run("ls", &["-l", day.to_str().unwrap()], &[("PATH", &path)], None);
        assert_eq!(requests(&env) - before, 1);
        assert!(text(&out).contains(".md"), "{out:?}");
        // an unknown option on the mount is an error, not a silent hand-over
        let out = env.run("grep", &["-P", "x", day.to_str().unwrap()], &[("PATH", &path)], None);
        assert_eq!(out.status.code(), Some(2));
        // a group directory takes no new files (only folders with dropType do)
        assert!(std::fs::write(day.join("x.txt"), b"x").is_err());
    }));
    umount(&mp, child);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}


/// Writing (SPEC-004): texts, the front matter of an index, new files in a folder with
/// `dropType`, `sed -i` on the server; a rejected write leaves a message in `/.i3cm-errors`.
#[test]
#[ignore]
fn writing_through_the_mount() {
    let env = env(1, 2);
    env.links();
    env.init();
    let (mp, child) = mount(&env, "mnt3", "1");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let day = mp.join("sample:mailbox/2026-11-20");
        let mut mails: Vec<PathBuf> = std::fs::read_dir(&day).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
        mails.sort();
        let mail = mails[0].clone();
        let name = mail.file_name().unwrap().to_string_lossy().into_owned();
        let node = rt().block_on(env.admin.aspect_children("sample:mailbox", &rt().block_on(env.admin.aspect_children("sample:mailbox", "")).unwrap()[0].node)).unwrap();
        let id = node.iter().find(|n| n.name == name).unwrap().id.unwrap();
        // a text
        std::fs::write(mail.join("mail.md"), b"# Neu\n").unwrap();
        assert_eq!(rt().block_on(env.admin.get_text(id, "sample:mail.md")).unwrap(), "# Neu\n");
        assert_eq!(std::fs::read_to_string(mail.join("mail.md")).unwrap(), "# Neu\n");
        // the front matter of the index
        let idx = day.join(format!("{name}.md"));
        let s = std::fs::read_to_string(&idx).unwrap();
        let line = s.lines().find(|l| l.starts_with("subject: ")).unwrap().to_string();
        std::fs::write(&idx, s.replace(&line, "subject: \"Geaendert\"")).unwrap();
        let obj = rt().block_on(env.admin.get_object(id, None)).unwrap();
        assert_eq!(obj.parts.iter().find(|p| p.type_name == "sample:Mail").unwrap().attributes["subject"], serde_json::json!("Geaendert"));
        // a rejected write: I/O error, message in /.i3cm-errors, nothing stored
        let idx2 = std::fs::read_dir(&day).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().map(|e| e == "md").unwrap_or(false) && p.to_string_lossy().contains("Geaendert")).unwrap();
        let s2 = std::fs::read_to_string(&idx2).unwrap();
        let bad = s2.replacen("_id: ", "_id: \"x\"\n_old: ", 1);
        // the error comes with fsync or close (std::fs::write ignores errors on close)
        let res = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().write(true).truncate(true).open(&idx2)?;
            f.write_all(bad.as_bytes())?;
            f.sync_all()
        })();
        assert!(res.is_err(), "{res:?}");
        let errors = std::fs::read_to_string(mp.join(".i3cm-errors")).unwrap();
        assert!(errors.contains("reserved"), "{errors}");
        // a new file in the attachments folder becomes an attachment
        let mail = std::fs::read_dir(&day).unwrap().map(|e| e.unwrap().path()).find(|p| p.is_dir() && p.to_string_lossy().contains("Geaendert")).unwrap();
        let folder = std::fs::read_dir(&mail).unwrap().map(|e| e.unwrap().path()).find(|p| p.is_dir()).unwrap();
        std::fs::write(folder.join("neu.pdf"), b"%PDF neu").unwrap();
        let ends = rt().block_on(env.admin.ends(id, "attachments", 100, 0)).unwrap();
        assert_eq!(ends.len(), 3);
        // sed -i on the mount runs on the server as one order
        let path = format!("{}:/usr/bin:/bin", env.home.join("bin").display());
        let out = env.run("sed", &["-i", "s/Neu/Neuer/", mail.join("mail.md").to_str().unwrap()], &[("PATH", &path)], None);
        assert!(out.status.success(), "{out:?}");
        assert_eq!(rt().block_on(env.admin.get_text(id, "sample:mail.md")).unwrap(), "# Neuer\n");
    }));
    umount(&mp, child);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
