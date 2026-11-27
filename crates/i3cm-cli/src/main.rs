//! `i3cm`: one program, the role follows the name it is called by.
//!
//! * `grep`, `ls`, `cat`, … (via links first in `PATH`): on paths in a i3cm mount the tool
//!   calls the server directly; on other paths it hands over to the next program of that name
//!   in `PATH`.
//! * `i3cm-grep`, `i3cm-ls`, …: without a mount, on tree paths (`/mail:mailbox/…`), relative
//!   to `I3CM_PWD` (default `/`).
//! * `i3cm-init`: store server, tenant and token. `i3cm-mount`: mount read-only (FUSE).
//! * `i3cm links <dir>`: create the links for all names in `<dir>`.
//! * `i3cm-cd <path>`: remember the working path of the `i3cm-*` tools for this shell session.
//! * `sort`, `uniq`, `cut`: run here as usual; files in a mount are fetched in one request.
//! * `i3cm config`: every setting with its source. `i3cm --version`: version and revision.
//! * `i3cm studio load|save|status [--force]`: the module repository's `i3cm/` and its copy in a
//!   development tenant (no merging; refuses to throw away changes of the other side).
//! * `i3cm scaffold ops --lang rust | ui [--force]`: frames for the IDE in the module repository.

use i3cm_cli::{api, config, fuse, scaffold, studio};
use i3cm_tools::{join, parse, render, Call, Tool, TOOLS};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::exit;

const MOUNT_TYPE: &str = "fuse.i3cm";
const MOUNT_SOURCE: &str = "i3cm";
/// Programs that run here on fetched data (not on the server).
const LOCAL: &[&str] = &["sort", "uniq", "cut"];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let name = Path::new(args.first().map(|s| s.as_str()).unwrap_or("i3cm")).file_name().and_then(|s| s.to_str()).unwrap_or("i3cm").to_string();
    let code = match name.as_str() {
        "i3cm-init" => init(&args[1..]),
        "i3cm-mount" => mount(&args[1..]),
        "i3cm-cd" => cd(&args[1..]),
        n if LOCAL.contains(&n) || n.strip_prefix("i3cm-").is_some_and(|x| LOCAL.contains(&x)) => local_tool(n, &args),
        n if n.starts_with("i3cm-") && Tool::from_name(n).is_some() => tree_tool(n, &args),
        n if Tool::from_name(n).is_some() => mounted_tool(n, &args),
        _ => base(&args[1..]),
    };
    exit(code)
}

fn usage() -> i32 {
    eprintln!(
        "usage: i3cm links <dir>            create links for all tools and i3cm-* names\n       \
         i3cm init --server URL --tenant NAME [--token-file FILE] [--lang LANG]\n       \
         i3cm mount <mountpoint> [--ttl SECONDS]\n       \
         i3cm studio load|save|status [--force]   module repository <-> development tenant\n       \
         i3cm scaffold ops --lang rust | ui [--force]\n       \
         i3cm <tool> args…                 tool on tree paths (tools: {})",
        TOOLS.join(" ")
    );
    2
}

fn base(args: &[String]) -> i32 {
    match args.first().map(|s| s.as_str()) {
        Some("links") if args.len() == 2 => links(&args[1]),
        Some("init") => init(&args[1..]),
        Some("mount") => mount(&args[1..]),
        Some("--version") => {
            println!("i3cm {} ({})", env!("CARGO_PKG_VERSION"), env!("I3CM_REV"));
            0
        }
        Some("cd") => cd(&args[1..]),
        Some("studio") => studio_cmd(&args[1..]),
        Some("scaffold") => scaffold_cmd(&args[1..]),
        Some("config") => {
            for (k, v, src) in config::describe() {
                println!("{k:<7} {v:<40} {src}");
            }
            0
        }
        Some(t) if Tool::from_name(t).is_some() => {
            let mut argv = vec![format!("i3cm-{t}")];
            argv.extend(args[1..].iter().cloned());
            tree_tool(&argv[0].clone(), &argv)
        }
        _ => usage(),
    }
}

/// `i3cm studio load|save|status [--force]` from the current directory (or below it).
fn studio_cmd(args: &[String]) -> i32 {
    let force = args.iter().any(|a| a == "--force");
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => return fail("i3cm studio", &e),
    };
    let api = api::Api::new(&cfg);
    let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let r = match args.first().map(|s| s.as_str()) {
        Some("load") => studio::load(&api, &here, force),
        Some("save") => studio::save(&api, &here, force),
        Some("status") => studio::status(&api, &here),
        _ => return fail("i3cm studio", "usage: i3cm studio load|save|status [--force]"),
    };
    r.unwrap_or_else(|e| fail("i3cm studio", &e))
}

/// `i3cm scaffold ops --lang rust | ui [--force]` in the module repository.
fn scaffold_cmd(args: &[String]) -> i32 {
    let force = args.iter().any(|a| a == "--force");
    let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(repo) = studio::find_repo(&here) else {
        return fail("i3cm scaffold", "no i3cm/module.ts here or above");
    };
    let module = std::fs::read_to_string(repo.join("i3cm/module.ts")).ok().and_then(|s| studio::module_name(&s));
    let Some(module) = module else {
        return fail("i3cm scaffold", "module.ts names no module");
    };
    let files = match args.first().map(|s| s.as_str()) {
        Some("ops") => match flag(args, "--lang") {
            Some("rust") => scaffold::ops_rust(&module),
            Some(l) => return fail("i3cm scaffold", &format!("unknown language {l} (known: {})", scaffold::LANGUAGES.join(" "))),
            None => return fail("i3cm scaffold", &format!("--lang is required (known: {})", scaffold::LANGUAGES.join(" "))),
        },
        Some("ui") => scaffold::ui(&module, &scaffold::aspects_in(&scaffold::aspect_sources(&repo))),
        _ => return fail("i3cm scaffold", "usage: i3cm scaffold ops --lang rust | ui [--force]"),
    };
    match scaffold::write(&repo, &files, force) {
        Ok(written) => {
            for p in written {
                println!("{}", p.display());
            }
            0
        }
        Err(e) => fail("i3cm scaffold", &e),
    }
}

/// Links for every name the program answers to.
fn links(dir: &str) -> i32 {
    let me = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail("i3cm", &e.to_string()),
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        return fail("i3cm", &format!("{dir}: {e}"));
    }
    let names = TOOLS
        .iter()
        .chain(LOCAL.iter())
        .flat_map(|t| [t.to_string(), format!("i3cm-{t}")])
        .chain(["i3cm".to_string(), "i3cm-init".to_string(), "i3cm-mount".to_string(), "i3cm-cd".to_string()]);
    for n in names {
        let p = Path::new(dir).join(&n);
        match std::fs::symlink_metadata(&p) {
            Ok(m) if !m.file_type().is_symlink() => continue, // never replace a real file (e.g. the program itself)
            Ok(_) => std::fs::remove_file(&p).unwrap_or_default(),
            Err(_) => {}
        }
        if let Err(e) = std::os::unix::fs::symlink(&me, &p) {
            return fail("i3cm", &format!("{}: {e}", p.display()));
        }
    }
    let abs = std::fs::canonicalize(dir).unwrap_or_else(|_| PathBuf::from(dir));
    println!("links created; put them first in PATH:\n  export PATH=\"{}:$PATH\"", abs.display());
    0
}

fn fail(name: &str, msg: &str) -> i32 {
    eprintln!("{name}: {msg}");
    2
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

fn init(args: &[String]) -> i32 {
    let (Some(server), Some(tenant)) = (flag(args, "--server"), flag(args, "--tenant")) else {
        return fail("i3cm-init", "usage: i3cm-init --server URL --tenant NAME [--token-file FILE] [--lang LANG]  (token from stdin without --token-file)");
    };
    let token = match config::token_from(flag(args, "--token-file")) {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => return fail("i3cm-init", "empty token"),
        Err(e) => return fail("i3cm-init", &e),
    };
    let cfg = config::Config { server: server.into(), tenant: tenant.into(), token, lang: flag(args, "--lang").map(String::from).unwrap_or_else(config::lang_from_env) };
    let me = match api::Api::new(&cfg).me() {
        Ok(v) => v,
        Err(e) => return fail("i3cm-init", &format!("token rejected: {e}")),
    };
    match config::save(&cfg) {
        Ok(p) => {
            let who = me["types"][0]["displayName"].as_str().unwrap_or("?");
            println!("i3cm-init: {} (tenant {}) as {who}; stored in {} and {}", cfg.server, cfg.tenant, p.display(), config::token_path().display());
            0
        }
        Err(e) => fail("i3cm-init", &e),
    }
}

fn mount(args: &[String]) -> i32 {
    let Some(mp) = args.iter().find(|a| !a.starts_with("--") && Some(a.as_str()) != flag(args, "--ttl")) else {
        return fail("i3cm-mount", "usage: i3cm-mount <mountpoint> [--ttl SECONDS]");
    };
    let ttl = flag(args, "--ttl").and_then(|s| s.parse().ok()).unwrap_or(5);
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => return fail("i3cm-mount", &e),
    };
    match fuse::mount(api::Api::new(&cfg), mp, std::time::Duration::from_secs(ttl)) {
        Ok(()) => 0,
        Err(e) => fail("i3cm-mount", &e.to_string()),
    }
}

/// Run a call on the server and print like the real tool; returns the exit status.
fn run_remote(name: &str, call: &Call) -> i32 {
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => return fail(name, &e),
    };
    match api::Api::new(&cfg).tool(call) {
        Ok(res) => {
            let (out, err) = render(&res);
            let _ = std::io::stdout().write_all(&out);
            let _ = std::io::stdout().flush();
            eprint!("{err}");
            res.status
        }
        Err(e) => fail(name, &e),
    }
}

/// `i3cm-<tool>`: paths in the tree, relative to `I3CM_PWD`.
fn tree_tool(name: &str, args: &[String]) -> i32 {
    let mut call = match parse(args) {
        Ok(c) => c,
        Err(e) => return fail_tool(&e),
    };
    call.req.cwd = config::pwd();
    run_remote(name, &call)
}

/// `i3cm-cd <path>`: check that it is a directory of the tree and remember it.
fn cd(args: &[String]) -> i32 {
    let target = join(&config::pwd(), args.first().map(|s| s.as_str()).unwrap_or("/"));
    let call = Call { tool: Tool::Stat, req: i3cm_tools::Request { cwd: "/".into(), paths: vec![target.clone()], display: vec![], options: Default::default() } };
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => return fail("i3cm-cd", &e),
    };
    match api::Api::new(&cfg).tool(&call) {
        Ok(res) => match res.records.first() {
            Some(i3cm_tools::Rec::Stat { entry, .. }) if entry.dir => match config::write_pwd(&target) {
                Ok(()) => {
                    println!("{target}");
                    0
                }
                Err(e) => fail("i3cm-cd", &e),
            },
            Some(i3cm_tools::Rec::Stat { .. }) => fail("i3cm-cd", &format!("{target}: Not a directory")),
            _ => fail("i3cm-cd", &format!("{target}: No such file or directory")),
        },
        Err(e) => fail("i3cm-cd", &e),
    }
}

/// Options of the local programs that take a value (to tell them from file operands).
fn takes_value(prog: &str, opt: &str) -> bool {
    let short: &[&str] = match prog {
        "sort" => &["-k", "-t", "-o", "-S", "-T"],
        "cut" => &["-d", "-f", "-c", "-b"],
        "uniq" => &["-f", "-s", "-w"],
        _ => &[],
    };
    short.contains(&opt)
}

/// `sort`, `uniq`, `cut`: files in a mount (or tree paths for `i3cm-*`) are fetched in one
/// request and given to the program of that name on standard input.
fn local_tool(name: &str, args: &[String]) -> i32 {
    let prog = name.strip_prefix("i3cm-").unwrap_or(name).to_string();
    let tree_mode = name.starts_with("i3cm-");
    let ms = if tree_mode { vec![] } else { mounts() };
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".into());
    let (mut opts, mut files) = (vec![], vec![]);
    let mut i = 1;
    let mut done = false;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if !done && a == "--" {
            done = true;
            continue;
        }
        if !done && a.starts_with('-') && a != "-" {
            opts.push(a.clone());
            if takes_value(&prog, a) && i < args.len() {
                opts.push(args[i].clone());
                i += 1;
            }
            continue;
        }
        files.push(a.clone());
    }
    let tree_paths: Vec<Option<String>> = files.iter().map(|f| if tree_mode { Some(join(&config::pwd(), f)) } else { in_mount(&ms, &join(&cwd, f)).map(|x| x.1) }).collect();
    if files.is_empty() || tree_paths.iter().all(|x| x.is_none()) {
        if tree_mode {
            return fail(name, "missing file operand");
        }
        return handoff(&prog, args);
    }
    if tree_paths.iter().any(|x| x.is_none()) {
        return fail(name, "paths inside and outside a i3cm mount cannot be mixed");
    }
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => return fail(name, &e),
    };
    let call = Call { tool: Tool::Cat, req: i3cm_tools::Request { cwd: "/".into(), paths: tree_paths.into_iter().flatten().collect(), display: files.clone(), options: Default::default() } };
    let res = match api::Api::new(&cfg).tool(&call) {
        Ok(r) => r,
        Err(e) => return fail(name, &e),
    };
    let mut input = vec![];
    for r in &res.records {
        match r {
            i3cm_tools::Rec::Data { data, .. } => input.extend_from_slice(data),
            i3cm_tools::Rec::Error { message } => eprintln!("{message}"),
            _ => {}
        }
    }
    if res.status != 0 {
        return res.status;
    }
    // the next program of that name in PATH, fed on stdin
    let me = std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(p).ok());
    for dir in std::env::var("PATH").unwrap_or_default().split(':').filter(|d| !d.is_empty()) {
        let cand = Path::new(dir).join(&prog);
        if !cand.is_file() || std::fs::canonicalize(&cand).ok() == me {
            continue;
        }
        let mut child = match std::process::Command::new(&cand).args(&opts).stdin(std::process::Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => return fail(name, &format!("{}: {e}", cand.display())),
        };
        let mut stdin = child.stdin.take().expect("stdin");
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let status = child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1);
        let _ = writer.join();
        return status;
    }
    eprintln!("{name}: no '{prog}' in PATH");
    127
}

fn fail_tool(e: &i3cm_tools::ToolError) -> i32 {
    eprintln!("{e}");
    2
}

/// Mount points of i3cm file systems (from /proc/self/mountinfo).
fn mounts() -> Vec<String> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let unescape = |s: &str| s.replace("\\040", " ").replace("\\011", "\t").replace("\\012", "\n").replace("\\134", "\\");
    text.lines()
        .filter_map(|l| {
            let (left, right) = l.split_once(" - ")?;
            let mut r = right.split(' ');
            let (fstype, source) = (r.next()?, r.next().unwrap_or(""));
            // the kernel shows `fuse.i3cm`, or plain `fuse` with source `i3cm` when the subtype is dropped
            (fstype == MOUNT_TYPE || (fstype == "fuse" && source == MOUNT_SOURCE)).then(|| unescape(left.split(' ').nth(4).unwrap_or("")))
        })
        .collect()
}

/// (mount point, tree path) if `abs` lies in a mount.
fn in_mount(mounts: &[String], abs: &str) -> Option<(String, String)> {
    mounts
        .iter()
        .filter_map(|m| {
            if abs == m {
                Some((m.clone(), "/".to_string()))
            } else {
                abs.strip_prefix(&format!("{}/", m.trim_end_matches('/'))).map(|rest| (m.clone(), format!("/{rest}")))
            }
        })
        .max_by_key(|(m, _)| m.len())
}

/// Hand over to the next program of the same name in PATH (not this one).
fn handoff(name: &str, args: &[String]) -> i32 {
    let me = std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(p).ok());
    for dir in std::env::var("PATH").unwrap_or_default().split(':').filter(|d| !d.is_empty()) {
        let cand = Path::new(dir).join(name);
        let Ok(meta) = std::fs::metadata(&cand) else { continue };
        if !meta.is_file() || std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 == 0 {
            continue;
        }
        if std::fs::canonicalize(&cand).ok() == me {
            continue;
        }
        let e = std::process::Command::new(&cand).arg0(name).args(&args[1..]).exec();
        return fail(name, &format!("{}: {e}", cand.display()));
    }
    eprintln!("{name}: no other '{name}' in PATH");
    127
}

/// A tool called by its plain name: server for paths in a mount, otherwise hand over.
fn mounted_tool(name: &str, args: &[String]) -> i32 {
    let ms = mounts();
    if ms.is_empty() {
        return handoff(name, args);
    }
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".into());
    let cwd_in = in_mount(&ms, &cwd);
    let mut call = match parse(args) {
        Ok(c) => c,
        Err(e) => {
            // an unknown option is an error only for calls on the mount
            let touches = cwd_in.is_some() || args[1..].iter().filter(|a| !a.starts_with('-')).any(|a| in_mount(&ms, &join(&cwd, a)).is_some());
            return if touches { fail_tool(&e) } else { handoff(name, args) };
        }
    };
    let implicit = call.req.paths.is_empty();
    if implicit {
        let needs_cwd = match call.tool {
            Tool::Ls | Tool::Find => true,
            Tool::Grep => call.req.options.recursive,
            _ => false,
        };
        match (&cwd_in, needs_cwd) {
            (Some((_, v)), true) => {
                call.req.paths = vec![v.clone()];
                call.req.display = vec![".".into()];
            }
            _ => return handoff(name, args),
        }
    } else {
        let mut inside = vec![];
        for p in &call.req.paths {
            inside.push(in_mount(&ms, &join(&cwd, p)));
        }
        if inside.iter().all(|x| x.is_none()) {
            return handoff(name, args);
        }
        if inside.iter().any(|x| x.is_none()) || inside.iter().any(|x| x.as_ref().map(|y| &y.0) != inside[0].as_ref().map(|y| &y.0)) {
            return fail(name, "paths inside and outside a i3cm mount cannot be mixed");
        }
        call.req.display = call.req.paths.clone();
        call.req.paths = inside.into_iter().map(|x| x.unwrap().1).collect();
    }
    call.req.cwd = cwd_in.map(|x| x.1).unwrap_or_else(|| "/".into());
    // find -exec with a foreign program: the server finds, the program runs here per file
    if let Some(exec) = call.req.options.exec.clone().filter(|e| Tool::from_name(&e.argv[0]).is_none()) {
        call.req.options.exec = None;
        let cfg = match config::load() {
            Ok(c) => c,
            Err(e) => return fail(name, &e),
        };
        let res = match api::Api::new(&cfg).tool(&call) {
            Ok(r) => r,
            Err(e) => return fail(name, &e),
        };
        let mut found = vec![];
        for r in &res.records {
            match r {
                i3cm_tools::Rec::Name { path } => found.push(path.clone()),
                i3cm_tools::Rec::Error { message } => eprintln!("{message}"),
                _ => {}
            }
        }
        let mut status = res.status;
        let run = |argv: Vec<String>| std::process::Command::new(&argv[0]).args(&argv[1..]).status().map(|s| s.success()).unwrap_or(false);
        if exec.batch {
            if !found.is_empty() && !run(exec.argv.iter().flat_map(|a| if a == "{}" { found.clone() } else { vec![a.clone()] }).collect()) {
                status = 1;
            }
        } else {
            for f in &found {
                run(exec.argv.iter().map(|a| a.replace("{}", f)).collect());
            }
        }
        return status;
    }
    run_remote(name, &call)
}
