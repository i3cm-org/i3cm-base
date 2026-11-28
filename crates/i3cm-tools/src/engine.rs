//! The engine: runs a call against a tree (the server's view of the data) and produces records.

use crate::{glob, join, parse, Call, Entry, FindTest, Rec, Response, Tool};
use regex::bytes::Regex;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;

pub type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A node of the tree: a directory or a file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Node {
    /// Absolute tree path (`/aspect/…`).
    pub path: String,
    pub entry: Entry,
    /// Opaque reference for the tree implementation.
    pub handle: String,
    /// Searched by `grep -r` (relevant texts and object files, not binary content).
    pub grep: bool,
    /// Files: key for the search prefilter; directories: identity for loop detection.
    pub key: Option<String>,
}

/// What the engine needs from the data.
pub trait Tree: Send + Sync {
    /// The node at an absolute tree path, or `None`.
    fn lookup<'a>(&'a self, path: &'a str) -> Fut<'a, Result<Option<Node>, String>>;
    /// All entries of a directory (complete, in display order).
    fn list<'a>(&'a self, dir: &'a Node) -> Fut<'a, Result<Vec<Node>, String>>;
    /// The bytes of a file.
    fn read<'a>(&'a self, file: &'a Node) -> Fut<'a, Result<Vec<u8>, String>>;
    /// Keys of files that may contain `literal` (prefilter), `None` if unknown.
    fn candidates<'a>(&'a self, _literal: &'a str, _icase: bool) -> Fut<'a, Option<HashSet<String>>> {
        Box::pin(async { None })
    }
    /// Write files back as one order (sed -i); returns the transaction.
    fn write<'a>(&'a self, _files: &'a [(Node, Vec<u8>)]) -> Fut<'a, Result<i64, String>> {
        Box::pin(async { Err("writing is not supported here".to_string()) })
    }
}

struct Run<'a> {
    tree: &'a dyn Tree,
    call: &'a Call,
    out: Vec<Rec>,
    status: i32,
}

fn child_display(parent: &str, name: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

fn basename(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        return "/".into();
    }
    t.rsplit('/').next().unwrap_or(t).to_string()
}

/// Run a call. The paths of `call.req` are relative to `call.req.cwd`.
pub fn run<'a>(tree: &'a dyn Tree, call: &'a Call) -> Fut<'a, Response> {
    Box::pin(async move {
        let mut r = Run { tree, call, out: vec![], status: 0 };
        match call.tool {
            Tool::Ls => r.ls().await,
            Tool::Cat | Tool::Head | Tool::Tail => r.bytes().await,
            Tool::Wc => r.wc().await,
            Tool::Stat => r.stat().await,
            Tool::Grep => r.grep().await,
            Tool::Find => r.find().await,
            Tool::Tree => r.tree().await,
            Tool::Sed => r.sed().await,
        }
        Response { status: r.status, records: r.out }
    })
}

impl<'a> Run<'a> {
    fn name(&self) -> &'static str {
        self.call.tool.name()
    }

    fn err(&mut self, msg: String, status: i32) {
        self.out.push(Rec::Error { message: format!("{}: {msg}", self.name()) });
        self.status = self.status.max(status);
    }

    /// (display, node) for each operand; missing ones are reported.
    async fn operands(&mut self, default_dot: bool, missing_status: i32) -> Vec<(String, Node)> {
        let req = &self.call.req;
        let mut paths = req.paths.clone();
        let mut display = req.display.clone();
        if paths.is_empty() && default_dot {
            paths.push(".".into());
            display = vec![".".into()];
        }
        let mut out = vec![];
        for (i, p) in paths.iter().enumerate() {
            let disp = display.get(i).cloned().unwrap_or_else(|| p.clone());
            let cwd = if req.cwd.is_empty() { "/" } else { req.cwd.as_str() };
            match self.tree.lookup(&join(cwd, p)).await {
                Ok(Some(n)) => out.push((disp, n)),
                Ok(None) => {
                    let msg = match self.call.tool {
                        Tool::Ls => format!("cannot access '{disp}': No such file or directory"),
                        Tool::Find | Tool::Stat => format!("'{disp}': No such file or directory"),
                        _ => format!("{disp}: No such file or directory"),
                    };
                    let msg = if self.call.tool == Tool::Stat { format!("cannot stat {msg}") } else { msg };
                    self.err(msg, missing_status);
                }
                Err(e) => self.err(format!("{disp}: {e}"), 2),
            }
        }
        out
    }

    async fn list(&mut self, n: &Node, disp: &str) -> Option<Vec<Node>> {
        match self.tree.list(n).await {
            Ok(l) => Some(l),
            Err(e) => {
                self.err(format!("{disp}: {e}"), 2);
                None
            }
        }
    }

    async fn read(&mut self, n: &Node, disp: &str) -> Option<Vec<u8>> {
        match self.tree.read(n).await {
            Ok(d) => Some(d),
            Err(e) => {
                self.err(format!("{disp}: {e}"), 2);
                None
            }
        }
    }

    // ---- ls ----

    async fn ls(&mut self) {
        let o = &self.call.req.options;
        let (long, all, recursive) = (o.long, o.all, o.recursive);
        let ops = self.operands(true, 2).await;
        let mut files: Vec<Entry> = vec![];
        let mut dirs: Vec<(String, Node)> = vec![];
        for (d, n) in ops {
            if n.entry.dir {
                dirs.push((d, n));
            } else {
                files.push(Entry { name: d, ..n.entry.clone() });
            }
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        dirs.sort_by(|a, b| a.0.cmp(&b.0));
        let headers = dirs.len() + usize::from(!files.is_empty()) > 1 || recursive;
        if !files.is_empty() {
            self.out.push(Rec::Listing { header: None, long, entries: files });
        }
        for (d, n) in dirs {
            self.ls_dir(&n, &d, headers, long, all, recursive, &mut vec![]).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ls_dir<'b>(&'b mut self, n: &'b Node, disp: &'b str, header: bool, long: bool, all: bool, recursive: bool, anc: &'b mut Vec<String>) -> Fut<'b, ()>
    where
        'a: 'b,
    {
        Box::pin(async move {
            let Some(list) = self.list(n, disp).await else { return };
            let mut entries: Vec<Entry> = list.iter().filter(|c| all || !c.entry.name.starts_with('.')).map(|c| c.entry.clone()).collect();
            if all {
                let dot = Entry { name: ".".into(), dir: true, nlink: 1, ..Default::default() };
                entries.push(Entry { name: "..".into(), ..dot.clone() });
                entries.push(dot);
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            self.out.push(Rec::Listing { header: header.then(|| disp.to_string()), long, entries });
            if recursive {
                let mut subs: Vec<&Node> = list.iter().filter(|c| c.entry.dir && (all || !c.entry.name.starts_with('.'))).collect();
                subs.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
                if let Some(k) = &n.key {
                    anc.push(k.clone());
                }
                for c in subs {
                    let cd = child_display(disp, &c.entry.name);
                    if c.key.as_ref().map(|k| anc.contains(k)).unwrap_or(false) {
                        self.err(format!("{cd}: not listing already-listed directory"), 1);
                        continue;
                    }
                    self.ls_dir(c, &cd, true, long, all, true, anc).await;
                }
                if n.key.is_some() {
                    anc.pop();
                }
            }
        })
    }

    // ---- cat, head, tail ----

    async fn bytes(&mut self) {
        let o = self.call.req.options.clone();
        let ops = self.operands(false, 1).await;
        let headers = self.call.tool != Tool::Cat && self.call.req.paths.len() > 1;
        for (d, n) in ops {
            if n.entry.dir {
                let m = match self.call.tool {
                    Tool::Cat => format!("{d}: Is a directory"),
                    _ => format!("error reading '{d}': Is a directory"),
                };
                self.err(m, 1);
                continue;
            }
            let Some(data) = self.read(&n, &d).await else { continue };
            let data = match self.call.tool {
                Tool::Head => head(&data, o.lines, o.bytes),
                Tool::Tail => tail(&data, o.lines, o.bytes, o.from_start),
                _ => data,
            };
            self.out.push(Rec::Data { header: headers.then(|| d.clone()), data });
        }
    }

    // ---- wc ----

    async fn wc(&mut self) {
        let o = self.call.req.options.clone();
        let none = !(o.count_lines || o.count_words || o.count_bytes);
        let (l, w, c) = (none || o.count_lines, none || o.count_words, none || o.count_bytes);
        let ops = self.operands(false, 1).await;
        let mut total = (0u64, 0u64, 0u64);
        let mut n_files = 0;
        for (d, n) in ops {
            if n.entry.dir {
                self.err(format!("{d}: Is a directory"), 1);
                continue;
            }
            let Some(data) = self.read(&n, &d).await else { continue };
            let lines = data.iter().filter(|b| **b == b'\n').count() as u64;
            let words = String::from_utf8_lossy(&data).split_whitespace().count() as u64;
            let bytes = data.len() as u64;
            total = (total.0 + lines, total.1 + words, total.2 + bytes);
            n_files += 1;
            self.out.push(Rec::Counts { path: Some(d), lines: l.then_some(lines), words: w.then_some(words), bytes: c.then_some(bytes) });
        }
        if self.call.req.paths.len() > 1 {
            let _ = n_files;
            self.out.push(Rec::Counts { path: Some("total".into()), lines: l.then_some(total.0), words: w.then_some(total.1), bytes: c.then_some(total.2) });
        }
    }

    // ---- stat ----

    async fn stat(&mut self) {
        for (d, n) in self.operands(false, 1).await {
            self.out.push(Rec::Stat { path: d, entry: n.entry });
        }
    }

    // ---- grep ----

    async fn grep(&mut self) {
        let o = self.call.req.options.clone();
        let re = match grep_regex(&o.patterns, o.fixed, o.extended, o.word, o.ignore_case) {
            Ok(r) => r,
            Err(e) => {
                self.err(e, 2);
                return;
            }
        };
        let cand = match literal(&o.patterns, o.fixed, o.extended) {
            Some(lit) if !o.invert && lit.chars().count() >= 3 => self.tree.candidates(&lit, o.ignore_case).await,
            _ => None,
        };
        let ops = self.operands(o.recursive, 2).await;
        let with_name = o.recursive || self.call.req.paths.len() > 1;
        let mut any = false;
        for (d, n) in ops {
            if n.entry.dir {
                if o.recursive {
                    any |= self.grep_dir(&n, &d, &re, with_name, &cand, &mut vec![]).await;
                } else {
                    self.err(format!("{d}: Is a directory"), 2);
                }
            } else if let Some(data) = self.read(&n, &d).await {
                any |= self.grep_data(&data, &d, &re, with_name);
            }
        }
        if self.status < 2 {
            self.status = if any { 0 } else { 1 };
        }
    }

    fn grep_dir<'b>(&'b mut self, n: &'b Node, disp: &'b str, re: &'b Regex, with_name: bool, cand: &'b Option<HashSet<String>>, anc: &'b mut Vec<String>) -> Fut<'b, bool>
    where
        'a: 'b,
    {
        Box::pin(async move {
            let mut any = false;
            let Some(list) = self.list(n, disp).await else { return false };
            if let Some(k) = &n.key {
                anc.push(k.clone());
            }
            for c in &list {
                let cd = child_display(disp, &c.entry.name);
                if c.entry.dir {
                    if c.key.as_ref().map(|k| anc.contains(k)).unwrap_or(false) {
                        continue; // loop in the view (an object below itself)
                    }
                    any |= self.grep_dir(c, &cd, re, with_name, cand, anc).await;
                } else if c.grep {
                    if let (Some(set), Some(k)) = (cand, &c.key)
                        && !set.contains(k) {
                            continue;
                        }
                    if let Some(data) = self.read(c, &cd).await {
                        any |= self.grep_data(&data, &cd, re, with_name);
                    }
                }
            }
            if n.key.is_some() {
                anc.pop();
            }
            any
        })
    }

    fn grep_data(&mut self, data: &[u8], disp: &str, re: &Regex, with_name: bool) -> bool {
        let o = &self.call.req.options;
        let binary = data.contains(&0);
        let mut lines: Vec<&[u8]> = data.split(|b| *b == b'\n').collect();
        if data.ends_with(b"\n") || data.is_empty() {
            lines.pop();
        }
        let mut count = 0u64;
        let (after, before) = (o.after_context.unwrap_or(0) as usize, o.before_context.unwrap_or(0) as usize);
        let context = after > 0 || before > 0;
        // last line printed (match or context), for `--` between groups
        let mut last_out: Option<usize> = None;
        let mut pending_after = 0usize;
        for (i, l) in lines.iter().enumerate() {
            let hit = re.is_match(l) != o.invert;
            if !hit {
                if context && pending_after > 0 && !binary && !o.files_with_matches && !o.count {
                    pending_after -= 1;
                    self.out.push(Rec::Context { path: with_name.then(|| disp.to_string()), line: o.line_number.then_some(i as u64 + 1), text: String::from_utf8_lossy(l).into_owned() });
                    last_out = Some(i);
                }
                continue;
            }
            count += 1;
            if o.files_with_matches || o.count {
                continue;
            }
            if binary {
                self.out.push(Rec::Binary { path: disp.to_string() });
                break;
            }
            if context {
                let from = i.saturating_sub(before).max(last_out.map(|x| x + 1).unwrap_or(0));
                if let Some(lo) = last_out
                    && from > lo + 1 {
                        self.out.push(Rec::Separator);
                    }
                if last_out.is_none() && self.out.iter().any(|r| matches!(r, Rec::Match { .. } | Rec::Context { .. })) {
                    self.out.push(Rec::Separator);
                }
                for (j, cl) in lines.iter().enumerate().take(i).skip(from) {
                    self.out.push(Rec::Context { path: with_name.then(|| disp.to_string()), line: o.line_number.then_some(j as u64 + 1), text: String::from_utf8_lossy(cl).into_owned() });
                }
                pending_after = after;
                last_out = Some(i);
            }
            self.out.push(Rec::Match { path: with_name.then(|| disp.to_string()), line: o.line_number.then_some(i as u64 + 1), text: String::from_utf8_lossy(l).into_owned() });
        }
        if o.files_with_matches {
            if count > 0 {
                self.out.push(Rec::Name { path: disp.to_string() });
            }
        } else if o.count {
            self.out.push(Rec::Count { path: with_name.then(|| disp.to_string()), count });
        }
        count > 0
    }

    // ---- tree ----

    async fn tree(&mut self) {
        let ops = self.operands(true, 2).await;
        let (mut dirs, mut files) = (0u64, 0u64);
        for (d, n) in ops {
            self.out.push(Rec::TreeLine { prefix: String::new(), name: d.clone(), dir: n.entry.dir });
            if n.entry.dir {
                self.tree_dir(&n, &d, String::new(), 1, &mut dirs, &mut files, &mut vec![]).await;
            }
        }
        self.out.push(Rec::TreeSummary { dirs, files });
    }

    #[allow(clippy::too_many_arguments)]
    fn tree_dir<'b>(&'b mut self, n: &'b Node, disp: &'b str, prefix: String, depth: u32, dirs: &'b mut u64, files: &'b mut u64, anc: &'b mut Vec<String>) -> Fut<'b, ()>
    where
        'a: 'b,
    {
        Box::pin(async move {
            let o = self.call.req.options.clone();
            if o.level.is_some_and(|l| depth > l) {
                return;
            }
            let Some(list) = self.list(n, disp).await else { return };
            let mut list: Vec<Node> = list.into_iter().filter(|c| (o.all || !c.entry.name.starts_with('.')) && (!o.dirs_only || c.entry.dir)).collect();
            list.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
            if let Some(k) = &n.key {
                anc.push(k.clone());
            }
            let count = list.len();
            for (i, c) in list.iter().enumerate() {
                let last = i + 1 == count;
                self.out.push(Rec::TreeLine { prefix: format!("{prefix}{}", if last { "└── " } else { "├── " }), name: c.entry.name.clone(), dir: c.entry.dir });
                if c.entry.dir {
                    *dirs += 1;
                    if c.key.as_ref().map(|k| anc.contains(k)).unwrap_or(false) {
                        continue;
                    }
                    let cd = child_display(disp, &c.entry.name);
                    self.tree_dir(c, &cd, format!("{prefix}{}", if last { "    " } else { "│   " }), depth + 1, dirs, files, anc).await;
                } else {
                    *files += 1;
                }
            }
            if n.key.is_some() {
                anc.pop();
            }
        })
    }

    // ---- sed (GNU sed --sandbox on the server; -i writes back as one order) ----

    async fn sed(&mut self) {
        let o = self.call.req.options.clone();
        let ops = self.operands(false, 2).await;
        let mut results: Vec<(String, Node, Vec<u8>)> = vec![];
        for (d, n) in ops {
            if n.entry.dir {
                self.err(format!("couldn't edit {d}: not a regular file"), 4);
                continue;
            }
            let Some(data) = self.read(&n, &d).await else { continue };
            match run_sed(&o.scripts, o.quiet, o.extended, &data) {
                Ok(out) => results.push((d, n, out)),
                Err(e) => {
                    self.err(e, 1);
                    return;
                }
            }
        }
        if self.status != 0 {
            return;
        }
        if o.in_place {
            let changed: Vec<(Node, Vec<u8>)> = results.iter().map(|(_, n, d)| (n.clone(), d.clone())).collect();
            let paths: Vec<String> = results.iter().map(|(d, _, _)| d.clone()).collect();
            if changed.is_empty() {
                return;
            }
            match self.tree.write(&changed).await {
                Ok(txn) => self.out.push(Rec::Written { paths, txn }),
                Err(e) => self.err(format!("nothing written: {e}"), 4),
            }
        } else {
            for (_, _, d) in results {
                self.out.push(Rec::Data { header: None, data: d });
            }
        }
    }

    // ---- find ----

    async fn find(&mut self) {
        let ops = self.operands(true, 1).await;
        let mut found = vec![];
        for (d, n) in ops {
            self.visit(&n, &d, &basename(&d), 0, &mut found, &mut vec![]).await;
        }
        let Some(exec) = self.call.req.options.exec.clone() else {
            self.out.extend(found.into_iter().map(|p| Rec::Name { path: p.0 }));
            return;
        };
        if Tool::from_name(&exec.argv[0]).is_none() {
            self.err(format!("-exec '{}': only the base tools run on the server", exec.argv[0]), 1);
            return;
        }
        // the tool gets the tree paths and shows them as find does
        let tree_paths: Vec<String> = found.iter().map(|f| f.1.clone()).collect();
        let calls: Vec<Vec<String>> = if exec.batch {
            vec![exec.argv.iter().flat_map(|a| if a == "{}" { tree_paths.clone() } else { vec![a.clone()] }).collect()]
        } else {
            tree_paths.iter().map(|f| exec.argv.iter().map(|a| a.replace("{}", f)).collect()).collect()
        };
        if found.is_empty() {
            return;
        }
        for argv in calls {
            match parse(&argv) {
                Ok(mut sub) => {
                    sub.req.cwd = self.call.req.cwd.clone();
                    sub.req.display = sub.req.paths.iter().map(|p| found.iter().find(|f| &f.1 == p).map(|f| f.0.clone()).unwrap_or_else(|| p.clone())).collect();
                    let res = run(self.tree, &sub).await;
                    self.out.extend(res.records);
                    if exec.batch && res.status != 0 {
                        self.status = self.status.max(1);
                    }
                }
                Err(e) => {
                    self.out.push(Rec::Error { message: e.to_string() });
                    self.status = 1;
                    return;
                }
            }
        }
    }

    fn matches(&self, n: &Node, disp: &str, name: &str) -> bool {
        self.call.req.options.tests.iter().all(|t| match t {
            FindTest::Name(p) => glob(p, name, false),
            FindTest::Iname(p) => glob(p, name, true),
            FindTest::Path(p) => glob(p, disp, false),
            FindTest::Type(t) => (t == "d") == n.entry.dir,
        })
    }

    fn visit<'b>(&'b mut self, n: &'b Node, disp: &'b str, name: &'b str, depth: u32, found: &'b mut Vec<(String, String)>, anc: &'b mut Vec<String>) -> Fut<'b, ()>
    where
        'a: 'b,
    {
        Box::pin(async move {
            let o = &self.call.req.options;
            if depth >= o.mindepth.unwrap_or(0) && self.matches(n, disp, name) {
                found.push((disp.to_string(), n.path.clone()));
            }
            if !n.entry.dir || depth >= o.maxdepth.unwrap_or(u32::MAX) {
                return;
            }
            let Some(list) = self.list(n, disp).await else { return };
            if let Some(k) = &n.key {
                anc.push(k.clone());
            }
            for c in &list {
                let cd = child_display(disp, &c.entry.name);
                if c.entry.dir && c.key.as_ref().map(|k| anc.contains(k)).unwrap_or(false) {
                    self.err(format!("File system loop detected; '{cd}' is part of the same file system loop as an ancestor."), 1);
                    continue;
                }
                self.visit(c, &cd, &c.entry.name, depth + 1, found, anc).await;
            }
            if n.key.is_some() {
                anc.pop();
            }
        })
    }
}

fn head(data: &[u8], lines: Option<u64>, bytes: Option<u64>) -> Vec<u8> {
    if let Some(b) = bytes {
        return data[..data.len().min(b as usize)].to_vec();
    }
    let n = lines.unwrap_or(10) as usize;
    let mut seen = 0;
    for (i, b) in data.iter().enumerate() {
        if *b == b'\n' {
            seen += 1;
            if seen == n {
                return data[..=i].to_vec();
            }
        }
    }
    if n == 0 {
        return vec![];
    }
    data.to_vec()
}

fn tail(data: &[u8], lines: Option<u64>, bytes: Option<u64>, from_start: bool) -> Vec<u8> {
    if let Some(b) = bytes {
        let b = b as usize;
        return if from_start { data[b.saturating_sub(1).min(data.len())..].to_vec() } else { data[data.len().saturating_sub(b)..].to_vec() };
    }
    let n = lines.unwrap_or(10) as usize;
    if from_start {
        let mut line = 1;
        for (i, b) in data.iter().enumerate() {
            if line >= n {
                return data[i..].to_vec();
            }
            if *b == b'\n' {
                line += 1;
            }
        }
        return vec![];
    }
    if n == 0 {
        return vec![];
    }
    // count line ends from the back; a missing final newline still ends a line
    let body = if data.ends_with(b"\n") { &data[..data.len() - 1] } else { data };
    let mut seen = 0;
    for i in (0..body.len()).rev() {
        if body[i] == b'\n' {
            seen += 1;
            if seen == n {
                return data[i + 1..].to_vec();
            }
        }
    }
    data.to_vec()
}

/// Basic regular expressions (grep default) to the extended syntax of the `regex` crate.
fn bre_to_ere(p: &str) -> String {
    let mut out = String::new();
    let mut it = p.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some(m @ ('+' | '?' | '(' | ')' | '{' | '}' | '|')) => out.push(m),
                Some(x) => {
                    out.push('\\');
                    out.push(x);
                }
                None => out.push_str("\\\\"),
            },
            '+' | '?' | '(' | ')' | '{' | '}' | '|' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn grep_regex(patterns: &[String], fixed: bool, extended: bool, word: bool, icase: bool) -> Result<Regex, String> {
    let parts: Vec<String> = patterns
        .iter()
        .flat_map(|p| p.split('\n').map(String::from).collect::<Vec<_>>())
        .map(|p| if fixed { regex::escape(&p) } else if extended { p } else { bre_to_ere(&p) })
        .map(|p| if word { format!(r"\b(?:{p})\b") } else { format!("(?:{p})") })
        .collect();
    let src = format!("{}{}", if icase { "(?i)" } else { "" }, parts.join("|"));
    Regex::new(&src).map_err(|e| format!("invalid pattern: {e}"))
}

/// The pattern as a plain literal, if it is one (for the search prefilter).
fn literal(patterns: &[String], fixed: bool, extended: bool) -> Option<String> {
    let [p] = patterns else { return None };
    if p.contains('\n') {
        return None;
    }
    let meta: &[char] = if extended { &['.', '[', ']', '*', '^', '$', '\\', '+', '?', '(', ')', '{', '}', '|'] } else { &['.', '[', ']', '*', '^', '$', '\\'] };
    (fixed || !p.contains(meta)).then(|| p.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render, Options, Request};
    use std::collections::BTreeMap;

    /// A tree from a map path -> content (directories end with `/`).
    struct MapTree(BTreeMap<String, Option<Vec<u8>>>);

    impl MapTree {
        fn new(files: &[(&str, &str)]) -> MapTree {
            let mut m = BTreeMap::new();
            m.insert("/".to_string(), None);
            for (p, c) in files {
                let mut cur = String::new();
                let segs: Vec<&str> = p.trim_start_matches('/').split('/').collect();
                for s in &segs[..segs.len() - 1] {
                    cur = format!("{cur}/{s}");
                    m.insert(cur.clone(), None);
                }
                m.insert(p.to_string(), Some(c.as_bytes().to_vec()));
            }
            MapTree(m)
        }
        fn node(&self, p: &str) -> Option<Node> {
            let c = self.0.get(p)?;
            let name = p.rsplit('/').next().unwrap_or("").to_string();
            Some(Node {
                path: p.into(),
                entry: Entry { name, dir: c.is_none(), size: c.as_ref().map(|x| x.len() as u64).unwrap_or(0), nlink: 1, mtime: Some(0), ino: None },
                handle: p.into(),
                grep: !p.ends_with(".bin"),
                key: None,
            })
        }
    }

    impl Tree for MapTree {
        fn lookup<'a>(&'a self, path: &'a str) -> Fut<'a, Result<Option<Node>, String>> {
            Box::pin(async move { Ok(self.node(path)) })
        }
        fn list<'a>(&'a self, dir: &'a Node) -> Fut<'a, Result<Vec<Node>, String>> {
            Box::pin(async move {
                let pre = if dir.path == "/" { "/".to_string() } else { format!("{}/", dir.path) };
                Ok(self.0.keys().filter(|k| k.starts_with(&pre) && k.len() > pre.len() && !k[pre.len()..].contains('/')).filter_map(|k| self.node(k)).collect())
            })
        }
        fn read<'a>(&'a self, file: &'a Node) -> Fut<'a, Result<Vec<u8>, String>> {
            Box::pin(async move { Ok(self.0.get(&file.path).cloned().flatten().unwrap_or_default()) })
        }
    }

    async fn sh(tree: &MapTree, cwd: &str, cmd: &str) -> (String, String, i32) {
        let argv: Vec<String> = cmd.split_whitespace().map(String::from).collect();
        let mut call = match parse(&argv) {
            Ok(c) => c,
            Err(e) => return (String::new(), e.to_string(), 2),
        };
        call.req.cwd = cwd.into();
        let r = run(tree, &call).await;
        let (out, err) = render(&r);
        (String::from_utf8(out).unwrap(), err, r.status)
    }

    fn tree() -> MapTree {
        MapTree::new(&[("/m/a/mail.md", "Hallo\nRechnung 42\n"), ("/m/a/x.bin", "Rechnung\0"), ("/m/b/mail.md", "rechnung\nende"), ("/m/top.md", "eins\nzwei\ndrei\n")])
    }

    #[tokio::test]
    async fn grep_recursive_and_options() {
        let t = tree();
        assert_eq!(sh(&t, "/m", "grep -r Rechnung .").await, ("./a/mail.md:Rechnung 42\n".into(), String::new(), 0));
        assert_eq!(sh(&t, "/", "grep -rin rechnung m").await.0, "m/a/mail.md:2:Rechnung 42\nm/b/mail.md:1:rechnung\n");
        assert_eq!(sh(&t, "/m", "grep -rl rechnung .").await.0, "./b/mail.md\n");
        assert_eq!(sh(&t, "/m", "grep -c ^[ed] top.md").await.0, "2\n");
        assert_eq!(sh(&t, "/m", "grep Rechnung a/x.bin").await.0, "Binary file a/x.bin matches\n");
        assert_eq!(sh(&t, "/m", "grep nothing top.md").await.2, 1);
        assert_eq!(sh(&t, "/m", "grep x a").await, (String::new(), "grep: a: Is a directory\n".into(), 2));
        assert_eq!(sh(&t, "/m", "grep -E ns|zw top.md").await.0, "eins\nzwei\n");
        assert_eq!(sh(&t, "/m", "grep ^\\(e\\|d\\) top.md").await.0, "eins\ndrei\n");
    }

    #[tokio::test]
    async fn ls_cat_head_tail_wc_stat() {
        let t = tree();
        assert_eq!(sh(&t, "/m", "ls").await.0, "a\nb\ntop.md\n");
        assert_eq!(sh(&t, "/", "ls -R m/a").await.0, "m/a:\nmail.md\nx.bin\n");
        assert_eq!(sh(&t, "/m", "cat top.md b/mail.md").await.0, "eins\nzwei\ndrei\nrechnung\nende");
        assert_eq!(sh(&t, "/m", "head -n 1 top.md").await.0, "eins\n");
        assert_eq!(sh(&t, "/m", "tail -2 top.md").await.0, "zwei\ndrei\n");
        assert_eq!(sh(&t, "/m", "tail -n 1 b/mail.md").await.0, "ende");
        assert_eq!(sh(&t, "/m", "tail -n +2 top.md").await.0, "zwei\ndrei\n");
        assert_eq!(sh(&t, "/m", "head -n 1 top.md b/mail.md").await.0, "==> top.md <==\neins\n\n==> b/mail.md <==\nrechnung\n");
        assert_eq!(sh(&t, "/m", "wc -l top.md").await.0, "3 top.md\n");
        assert_eq!(sh(&t, "/m", "wc top.md b/mail.md").await.0, " 3  3 15 top.md\n 1  2 13 b/mail.md\n 4  5 28 total\n");
        assert!(sh(&t, "/m", "stat top.md").await.0.contains("Size: 15"));
        assert_eq!(sh(&t, "/m", "cat nope").await, (String::new(), "cat: nope: No such file or directory\n".into(), 1));
    }

    #[tokio::test]
    async fn find_and_exec() {
        let t = tree();
        assert_eq!(sh(&t, "/m", "find . -name *.md").await.0, "./a/mail.md\n./b/mail.md\n./top.md\n");
        assert_eq!(sh(&t, "/m", "find . -maxdepth 1 -type d").await.0, ".\n./a\n./b\n");
        assert_eq!(sh(&t, "/", "find m -path */b/* -type f").await.0, "m/b/mail.md\n");
        assert_eq!(sh(&t, "/m", "find . -name mail.md -exec grep -l Rechnung {} +").await.0, "./a/mail.md\n");
        // `;` runs the tool per file: grep then prints no file names
        assert_eq!(sh(&t, "/m", "find . -name mail.md -exec grep -i rechnung {} ;").await.0, "Rechnung 42\nrechnung\n");
        assert_eq!(sh(&t, "/m", "find . -exec awk x {} ;").await.2, 1);
    }

    #[tokio::test]
    async fn typed_call_without_parser() {
        let t = tree();
        let call = Call { tool: Tool::Grep, req: Request { cwd: "/".into(), paths: vec!["m".into()], display: vec!["/mnt/m".into()], options: Options { recursive: true, patterns: vec!["zwei".into()], ..Default::default() } } };
        let (out, _) = render(&run(&t, &call).await);
        assert_eq!(String::from_utf8(out).unwrap(), "/mnt/m/top.md:zwei\n");
    }
}


/// Run GNU sed in sandbox mode (no `e`, `r`, `w` commands) on `data`.
fn run_sed(scripts: &[String], quiet: bool, extended: bool, data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("sed");
    cmd.arg("--sandbox");
    if quiet {
        cmd.arg("-n");
    }
    if extended {
        cmd.arg("-E");
    }
    for s in scripts {
        cmd.arg("-e").arg(s);
    }
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("sed not available on the server: {e}"))?;
    let mut stdin = child.stdin.take().expect("stdin");
    let input = data.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().trim_start_matches("sed: ").to_string());
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod new_tool_tests {
    use super::*;

    struct One;
    impl Tree for One {
        fn lookup<'a>(&'a self, path: &'a str) -> Fut<'a, Result<Option<Node>, String>> {
            Box::pin(async move {
                Ok(match path {
                    "/" | "/d" => Some(Node { path: path.into(), entry: crate::Entry { name: "d".into(), dir: true, ..Default::default() }, ..Default::default() }),
                    "/d/f.txt" => Some(Node { path: path.into(), entry: crate::Entry { name: "f.txt".into(), size: 1, ..Default::default() }, grep: true, ..Default::default() }),
                    _ => None,
                })
            })
        }
        fn list<'a>(&'a self, dir: &'a Node) -> Fut<'a, Result<Vec<Node>, String>> {
            Box::pin(async move { Ok(if dir.path == "/d" || dir.path == "/" { vec![Node { path: "/d/f.txt".into(), entry: crate::Entry { name: "f.txt".into(), ..Default::default() }, grep: true, ..Default::default() }] } else { vec![] }) })
        }
        fn read<'a>(&'a self, _f: &'a Node) -> Fut<'a, Result<Vec<u8>, String>> {
            Box::pin(async { Ok(b"a\nb\nX1\nc\nd\ne\nX2\nf\n".to_vec()) })
        }
    }

    async fn out(argv: &str) -> (String, i32) {
        let call = parse(&argv.split_whitespace().map(String::from).collect::<Vec<_>>()).unwrap();
        let mut call = call;
        call.req.cwd = "/".into();
        let r = run(&One, &call).await;
        (String::from_utf8(crate::render(&r).0).unwrap(), r.status)
    }

    #[tokio::test]
    async fn grep_context() {
        assert_eq!(out("grep -n -A1 X /d/f.txt").await.0, "3:X1\n4-c\n--\n7:X2\n8-f\n");
        assert_eq!(out("grep -C1 X /d/f.txt").await.0, "b\nX1\nc\n--\ne\nX2\nf\n");
        assert_eq!(out("grep -B3 X2 /d/f.txt").await.0, "c\nd\ne\nX2\n");
    }

    #[tokio::test]
    async fn tree_listing() {
        assert_eq!(out("tree /d").await.0, "/d\n└── f.txt\n\n0 directories, 1 file\n");
    }

    #[tokio::test]
    async fn sed_prints_and_rejects_writes_without_support() {
        assert_eq!(out("sed -n s/X/Y/p /d/f.txt").await.0, "Y1\nY2\n");
        // the sandbox forbids writing files from the script
        assert_ne!(out("sed w/tmp/x /d/f.txt").await.1, 0);
        // -i needs a tree that can write
        assert_ne!(out("sed -i s/X/Y/ /d/f.txt").await.1, 0);
    }
}
