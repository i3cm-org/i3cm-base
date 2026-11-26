//! The FUSE driver's logic without the kernel: what lookup, getattr, readdir and read answer is
//! compared with the API, and the requests of a recursive walk are counted. (The same checks
//! through a real mount are in `fuse.rs`.)

mod common;
use i3cm_cli::api::Api;
use i3cm_cli::config::Config;
use i3cm_cli::fuse::Fs;
use i3cm_tools::{project, Slot};
use common::*;
use std::time::Duration;

fn driver(env: &Env) -> Fs {
    Fs::new(Api::new(&Config { server: env.url.clone(), tenant: "t".into(), token: env.token.clone(), lang: "de-DE".into() }), Duration::from_secs(600))
}

fn requests(env: &Env) -> u64 {
    rt().block_on(env.admin.metrics()).unwrap()["requests"].as_u64().unwrap()
}

/// Walk like `find` (readdir, then lookup and stat of every entry); with `read` also read every
/// file like `cat`. Returns (directories, files).
fn walk(fs: &Fs, dir: u64, read: bool, counts: &mut (u64, u64)) {
    for (name, _) in fs.entries(dir).unwrap() {
        let a = fs.lookup_name(dir, &name).unwrap();
        if a.kind == fuser::FileType::Directory {
            counts.0 += 1;
            walk(fs, a.ino.0, read, counts);
        } else {
            counts.1 += 1;
            if read {
                assert_eq!(fs.content(a.ino.0).unwrap().len() as u64, a.size);
            }
        }
    }
}

#[test]
fn requests_grow_with_directories_not_files() {
    let env = env(3, 10);
    let mut report = vec![];
    for read in [false, true] {
        let fs = driver(&env);
        let before = requests(&env);
        let top = fs.lookup_name(1, "base:mailbox").unwrap();
        let mut counts = (1, 0);
        walk(&fs, top.ino.0, read, &mut counts);
        let cost = requests(&env) - before;
        report.push(format!("{}: {} directories, {} files, {cost} requests", if read { "find + cat" } else { "find" }, counts.0, counts.1));
        assert!(cost <= 2 * counts.0 + 2, "{cost} requests for {} directories", counts.0);
    }
    println!("{}", report.join("\n"));
}

fn compare(env: &Env, fs: &Fs, dir: u64, aspect: &str, node: &str, depth: u32, seen: &mut u64) {
    let children = rt().block_on(env.admin.aspect_children(aspect, node)).unwrap();
    let pairs: Vec<(&str, bool)> = children.iter().map(|c| (c.name.as_str(), c.file)).collect();
    let expected = project(&pairs);
    let got: Vec<String> = fs.entries(dir).unwrap().into_iter().map(|e| e.0).collect();
    assert_eq!(got, expected.iter().map(|e| e.0.clone()).collect::<Vec<_>>(), "listing of {aspect} {node}");
    for (name, slot) in &expected {
        let a = fs.lookup_name(dir, name).unwrap();
        *seen += 1;
        match *slot {
            Slot::Dir(i) => {
                assert_eq!(a.kind, fuser::FileType::Directory);
                if depth > 0 {
                    compare(env, fs, a.ino.0, aspect, &children[i].node, depth - 1, seen);
                }
            }
            Slot::Index(i) | Slot::File(i) => {
                let c = &children[i];
                let api = match slot {
                    Slot::Index(_) => rt().block_on(env.admin.aspect_index(aspect, &c.node, Some(&c.name))).unwrap().into_bytes(),
                    _ => rt().block_on(env.admin.aspect_file(aspect, &c.node, &c.display_name)).unwrap(),
                };
                let bytes = fs.content(a.ino.0).unwrap();
                assert_eq!(*bytes, api, "bytes of {name}");
                assert_eq!(a.size, api.len() as u64, "size of {name}");
                assert_eq!(Some(a.ino.0), c.ino, "inode of {name}");
                let object = c.id.is_some() && (c.file || c.node.starts_with("o:"));
                assert_eq!(a.nlink, if object { 2 } else { 1 }, "nlink of {name}");
                assert_eq!(a.perm, 0o444);
            }
        }
    }
}

#[test]
fn the_driver_reproduces_the_api() {
    let env = env(2, 3);
    let fs = driver(&env);
    let root: Vec<String> = fs.entries(1).unwrap().into_iter().map(|e| e.0).collect();
    for a in ["_id", "_type", "base:mailbox", "base:recipients"] {
        assert!(root.contains(&a.to_string()) && root.contains(&format!("{a}.md")), "{root:?}");
    }
    let mut seen = 0;
    for (a, depth) in [("base:mailbox", 10), ("base:recipients", 10), ("_type", 3)] {
        let top = fs.lookup_name(1, a).unwrap();
        compare(&env, &fs, top.ino.0, a, "", depth, &mut seen);
        let idx = fs.lookup_name(1, &format!("{a}.md")).unwrap();
        assert_eq!(*fs.content(idx.ino.0).unwrap(), rt().block_on(env.admin.aspect_index(a, "", None)).unwrap().into_bytes());
    }
    println!("compared {seen} entries");

    // the same text in two views: same inode
    let mb = fs.lookup_name(1, "base:mailbox").unwrap().ino.0;
    let day = fs.lookup_name(mb, "2026-11-20").unwrap().ino.0;
    let (mail_name, _) = fs.entries(day).unwrap().into_iter().find(|e| !e.0.ends_with(".md")).unwrap();
    let mail = fs.lookup_name(day, &mail_name).unwrap().ino.0;
    let md = fs.lookup_name(mail, "mail.md").unwrap();
    let rc = fs.lookup_name(1, "base:recipients").unwrap().ino.0;
    let same = fs.entries(rc).unwrap().into_iter().filter(|e| !e.0.ends_with(".md")).find_map(|(g, _)| {
        let gi = fs.lookup_name(rc, &g).unwrap().ino.0;
        let m = fs.lookup_name(gi, &mail_name).ok()?;
        fs.lookup_name(m.ino.0, "mail.md").ok()
    });
    assert_eq!(same.unwrap().ino, md.ino);

    // `_id` lists nothing but reaches every object
    let idr = fs.lookup_name(1, "_id").unwrap().ino.0;
    assert!(fs.entries(idr).unwrap().is_empty());
    let id = rt().block_on(env.admin.aspect_entry("base:mailbox", &format!("o:{}", uuid_of(&env, &mail_name)), None)).unwrap().id.unwrap();
    let obj = fs.lookup_name(idr, &id.to_string()).unwrap();
    let text = fs.lookup_name(obj.ino.0, "mail.md").unwrap();
    assert_eq!(text.ino, md.ino);
    let index = fs.lookup_name(idr, &format!("{id}.md")).unwrap();
    assert!(String::from_utf8_lossy(&fs.content(index.ino.0).unwrap()).contains(&format!("_id: \"{id}\"")));
    assert!(fs.lookup_name(idr, "not-an-id").is_err());
}

/// Id of the mail with this display name (via the tool API).
fn uuid_of(env: &Env, mail_name: &str) -> uuid::Uuid {
    let v = rt().block_on(env.admin.tool_exec("/base:mailbox/2026-11-20", &["head", "-n", "2", &format!("{mail_name}.md")])).unwrap();
    let line = v["stdout"].as_str().unwrap().lines().nth(1).unwrap().to_string();
    uuid::Uuid::parse_str(line.trim_start_matches("_id: \"").trim_end_matches('"')).unwrap()
}
