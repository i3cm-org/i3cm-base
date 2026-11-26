//! The FUSE driver. It shows the API faithfully: a directory lists exactly the
//! children from `children` (a node as `<name>/` plus its index `<name>.md`, a file as `<name>`),
//! a file holds exactly the bytes from `file` or `index`, and `stat` matches. Directories are
//! fetched completely (paged internally) and the files of a directory are read in one request,
//! so a recursive reader costs requests per directory, not per file.
//!
//! Writing (SPEC-004 section 5): a written file is buffered and sent when it is closed (or
//! flushed); each send is one order on the server, so rights and checks apply as always. A
//! rejected write returns `EIO`, and the server's message is kept in `/.i3cm-errors`. A new
//! file in a folder whose view declares `dropType` creates an object.

use crate::api::{Api, Child};
use i3cm_tools::{fs_name, project, Named, Slot};
use fuser::{Errno, FileAttr, FileHandle, FileType, FopenFlags, Generation, INodeNo, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen, Request};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Directory node ids live above the inode numbers of the server (< 2^53).
const DIR_BASE: u64 = 1 << 60;
/// Read files of a directory together up to this size each and in total.
const BATCH_FILE: u64 = 1 << 20;
const BATCH_TOTAL: u64 = 16 << 20;
/// The error log at the root of the mount.
const ERR_INO: u64 = 2;
const ERR_NAME: &str = ".i3cm-errors";
/// Inodes of files created in this mount but not yet sent.
const NEW_BASE: u64 = 1 << 59;

#[derive(Debug, Clone)]
enum Kind {
    Root,
    Dir { aspect: String, node: String },
    File { aspect: String, node: String, name: String },
    Index { aspect: String, node: String, dir: String },
    /// Created here, sent on close (drop into a folder).
    New { aspect: String, folder: String, name: String },
    Errors,
}

/// A file being written.
#[derive(Default)]
struct WBuf {
    data: Vec<u8>,
    dirty: bool,
    opens: u32,
}

#[derive(Debug, Clone)]
struct Ino {
    kind: Kind,
    dir: bool,
    size: u64,
    nlink: u32,
    mtime: SystemTime,
    /// Directory the entry was last seen in (for reading a directory's files together).
    parent: u64,
    /// Path of a directory (gives it a stable node id).
    path: String,
}

#[derive(Default)]
struct State {
    inodes: HashMap<u64, Ino>,
    dir_ids: HashMap<String, u64>,
    next_dir: u64,
    listings: HashMap<u64, (Instant, Vec<(String, u64)>)>,
    data: HashMap<u64, (Instant, Arc<Vec<u8>>)>,
    wbuf: HashMap<u64, WBuf>,
    errors: Vec<String>,
    next_new: u64,
}

pub struct Fs {
    api: Api,
    ttl: Duration,
    uid: u32,
    gid: u32,
    started: SystemTime,
    st: Mutex<State>,
}

fn time(s: &Option<String>, default: SystemTime) -> SystemTime {
    s.as_deref()
        .and_then(chrono_like_parse)
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64))
        .unwrap_or(default)
}

/// Seconds of an RFC 3339 time `YYYY-MM-DDTHH:MM:SS[.f](Z|±HH:MM)`.
fn chrono_like_parse(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let n = |s: &str| s.parse::<i64>().ok();
    let (y, mo, d) = (n(&t[0..4])?, n(&t[5..7])?, n(&t[8..10])?);
    let (h, mi, s) = (n(&t[11..13])?, n(&t[14..16])?, n(&t[17..19])?);
    // days from civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let mut secs = days * 86400 + h * 3600 + mi * 60 + s;
    let rest = &t[19..];
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(off) = rest.get(0..6).filter(|o| o.starts_with('+') || o.starts_with('-')) {
        let sign = if off.starts_with('-') { -1 } else { 1 };
        secs -= sign * (n(&off[1..3])? * 3600 + n(&off[4..6])? * 60);
    }
    Some(secs)
}

impl Fs {
    pub fn new(api: Api, ttl: Duration) -> Fs {
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let mut st = State { next_dir: DIR_BASE, next_new: NEW_BASE, ..Default::default() };
        st.inodes.insert(1, Ino { kind: Kind::Root, dir: true, size: 0, nlink: 1, mtime: SystemTime::now(), parent: 1, path: "/".into() });
        Fs { api, ttl, uid, gid, started: SystemTime::now(), st: Mutex::new(st) }
    }

    fn attr(&self, id: u64, i: &Ino) -> FileAttr {
        FileAttr {
            ino: INodeNo(id),
            size: i.size,
            blocks: i.size.div_ceil(512),
            atime: i.mtime,
            mtime: i.mtime,
            ctime: i.mtime,
            crtime: i.mtime,
            kind: if i.dir { FileType::Directory } else { FileType::RegularFile },
            perm: if i.dir { 0o755 } else if matches!(i.kind, Kind::Errors) { 0o444 } else { 0o644 },
            nlink: i.nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn dir_id(st: &mut State, path: &str) -> u64 {
        if let Some(id) = st.dir_ids.get(path) {
            return *id;
        }
        st.next_dir += 1;
        let id = st.next_dir;
        st.dir_ids.insert(path.to_string(), id);
        id
    }

    fn child_path(parent: &str, name: &str) -> String {
        if parent == "/" { format!("/{name}") } else { format!("{parent}/{name}") }
    }

    /// Register the entries of a listing and return (name, node id).
    fn register(&self, st: &mut State, parent: u64, ppath: &str, aspect: &str, children: &[Child]) -> Vec<(String, u64)> {
        let named: Vec<Named> = children.iter().map(|c| Named { name: &c.name, file: c.file, fallback: c.fallback_name.as_deref(), key: &c.node }).collect();
        let entries = project(&named);
        let mut out = vec![];
        for (name, slot) in entries.iter().cloned() {
            let (id, ino) = match slot {
                Slot::Dir(i) => {
                    let c = &children[i];
                    let path = Self::child_path(ppath, &name);
                    let id = Self::dir_id(st, &path);
                    (id, Ino { kind: Kind::Dir { aspect: aspect.into(), node: c.node.clone() }, dir: true, size: 0, nlink: 1, mtime: time(&c.changed_at, self.started), parent, path })
                }
                Slot::Index(i) => {
                    let c = &children[i];
                    let obj = c.id.is_some() && c.node.starts_with("o:");
                    let dir = i3cm_tools::dir_name(&entries, i).unwrap_or(&c.name).to_string();
                    // under another name than the server assumed, the index links differ: own inode
                    let id = if dir == c.name { c.ino.unwrap_or_else(|| i3cm_tools::ino(&["i", aspect, &c.node])) } else { i3cm_tools::ino(&["i", aspect, &c.node, &dir]) };
                    let kind = Kind::Index { aspect: aspect.into(), node: c.node.clone(), dir };
                    (id, Ino { kind, dir: false, size: c.size.unwrap_or(0), nlink: if obj { 2 } else { 1 }, mtime: time(&c.changed_at, self.started), parent, path: String::new() })
                }
                Slot::File(i) => {
                    let c = &children[i];
                    let id = c.ino.unwrap_or_else(|| i3cm_tools::ino(&["f", aspect, &c.node, &c.display_name]));
                    let kind = Kind::File { aspect: aspect.into(), node: c.node.clone(), name: c.display_name.clone() };
                    (id, Ino { kind, dir: false, size: c.size.unwrap_or(0), nlink: 2, mtime: time(&c.changed_at, self.started), parent, path: String::new() })
                }
            };
            st.inodes.insert(id, ino);
            out.push((name, id));
        }
        out
    }

    /// The (cached) listing of a directory.
    fn listing(&self, dir: u64) -> Result<Vec<(String, u64)>, Errno> {
        let (kind, path) = {
            let st = self.st.lock().unwrap();
            if let Some((t, l)) = st.listings.get(&dir)
                && t.elapsed() < self.ttl {
                    return Ok(l.clone());
                }
            let i = st.inodes.get(&dir).ok_or(Errno::ENOENT)?;
            if !i.dir {
                return Err(Errno::ENOTDIR);
            }
            (i.kind.clone(), i.path.clone())
        };
        let list = match kind {
            Kind::Root => {
                let aspects = self.api.aspects().map_err(|_| Errno::EIO)?;
                let mut st = self.st.lock().unwrap();
                let mut out = vec![];
                for a in aspects {
                    let name = a["name"].as_str().unwrap_or_default().to_string();
                    let fname = fs_name(&name);
                    let path = Self::child_path("/", &fname);
                    let id = Self::dir_id(&mut st, &path);
                    st.inodes.insert(id, Ino { kind: Kind::Dir { aspect: name.clone(), node: String::new() }, dir: true, size: 0, nlink: 1, mtime: self.started, parent: 1, path });
                    out.push((fname.clone(), id));
                    let iid = a["ino"].as_u64().unwrap_or_else(|| i3cm_tools::ino(&["i", &name, ""]));
                    let kind = Kind::Index { aspect: name, node: String::new(), dir: fname.clone() };
                    st.inodes.insert(iid, Ino { kind, dir: false, size: a["size"].as_u64().unwrap_or(0), nlink: 1, mtime: self.started, parent: 1, path: String::new() });
                    out.push((format!("{fname}.md"), iid));
                }
                if !st.errors.is_empty() {
                    let size = st.errors.iter().map(|e| e.len() as u64 + 1).sum();
                    st.inodes.insert(ERR_INO, Ino { kind: Kind::Errors, dir: false, size, nlink: 1, mtime: SystemTime::now(), parent: 1, path: String::new() });
                    out.push((ERR_NAME.into(), ERR_INO));
                }
                st.listings.insert(dir, (Instant::now(), out.clone()));
                out
            }
            Kind::Dir { aspect, node } => {
                let children = self.api.children(&aspect, &node).map_err(|_| Errno::EIO)?;
                let out = {
                    let mut st = self.st.lock().unwrap();
                    let out = self.register(&mut st, dir, &path, &aspect, &children);
                    st.listings.insert(dir, (Instant::now(), out.clone()));
                    out
                };
                self.fetch_renamed_indexes(&aspect, &children, &out);
                out
            }
            _ => return Err(Errno::ENOTDIR),
        };
        Ok(list)
    }

    /// `/_id/<id>` and `/_id/<id>.md`: the entry of `_id` lists nothing but reaches every object.
    fn by_id(&self, parent: u64, name: &str) -> Option<u64> {
        let (u, index) = match name.strip_suffix(".md") {
            Some(u) => (u, true),
            None => (name, false),
        };
        if u.len() != 36 || !u.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return None;
        }
        let node = format!("o:{u}");
        let c = self.api.entry("_id", &node, u).ok()?;
        let mut st = self.st.lock().unwrap();
        let mtime = time(&c.changed_at, self.started);
        if index {
            let id = c.ino?;
            st.inodes.insert(id, Ino { kind: Kind::Index { aspect: "_id".into(), node, dir: u.into() }, dir: false, size: c.size.unwrap_or(0), nlink: 2, mtime, parent, path: String::new() });
            Some(id)
        } else {
            let path = format!("/_id/{u}");
            let id = Self::dir_id(&mut st, &path);
            st.inodes.insert(id, Ino { kind: Kind::Dir { aspect: "_id".into(), node }, dir: true, size: 0, nlink: 1, mtime, parent, path });
            Some(id)
        }
    }

    fn is_id_root(&self, dir: u64) -> bool {
        matches!(self.st.lock().unwrap().inodes.get(&dir).map(|i| &i.kind), Some(Kind::Dir { aspect, node }) if aspect == "_id" && node.is_empty())
    }

    /// Bytes of a file; on a miss the small files of its directory are fetched together.
    fn data(&self, id: u64) -> Result<Arc<Vec<u8>>, Errno> {
        let (ino, parent) = {
            let st = self.st.lock().unwrap();
            if let Some((t, d)) = st.data.get(&id)
                && t.elapsed() < self.ttl {
                    return Ok(d.clone());
                }
            if let Some(b) = st.wbuf.get(&id) {
                return Ok(Arc::new(b.data.clone()));
            }
            let i = st.inodes.get(&id).ok_or(Errno::ENOENT)?.clone();
            if matches!(i.kind, Kind::Errors) {
                return Ok(Arc::new(st.errors.iter().map(|e| format!("{e}\n")).collect::<String>().into_bytes()));
            }
            if matches!(i.kind, Kind::New { .. }) {
                return Ok(Arc::new(vec![]));
            }
            let p = i.parent;
            (i, p)
        };
        if ino.size <= BATCH_FILE {
            self.prefetch(parent);
            if let Some((_, d)) = self.st.lock().unwrap().data.get(&id) {
                return Ok(d.clone());
            }
        }
        let bytes = match &ino.kind {
            Kind::File { aspect, node, name } => self.api.file(aspect, node, name),
            Kind::Index { aspect, node, dir } => self.api.index(aspect, node, dir),
            _ => return Err(Errno::EISDIR),
        }
        .map_err(|_| Errno::EIO)?;
        let d = Arc::new(bytes);
        self.st.lock().unwrap().data.insert(id, (Instant::now(), d.clone()));
        Ok(d)
    }

    /// Indexes shown under another name than the server assumed (collisions) have other links and
    /// so another size: fetch them in one request so that `stat` is exact.
    fn fetch_renamed_indexes(&self, aspect: &str, children: &[Child], out: &[(String, u64)]) {
        let mut items: Vec<(u64, Value)> = vec![];
        {
            let st = self.st.lock().unwrap();
            for (_, id) in out {
                if let Some(Ino { kind: Kind::Index { node, dir, .. }, .. }) = st.inodes.get(id)
                    && children.iter().any(|c| &c.node == node && !c.file && &c.name != dir) {
                        items.push((*id, json!({"node": node, "name": dir, "index": true})));
                    }
            }
        }
        if items.is_empty() {
            return;
        }
        let body: Vec<Value> = items.iter().map(|x| x.1.clone()).collect();
        if let Ok(res) = self.api.files(aspect, &body) {
            let mut st = self.st.lock().unwrap();
            for ((id, _), d) in items.iter().zip(res) {
                if let Some(d) = d {
                    if let Some(i) = st.inodes.get_mut(id) {
                        i.size = d.len() as u64;
                    }
                    st.data.insert(*id, (Instant::now(), Arc::new(d)));
                }
            }
        }
    }

    /// Fetch all small, not yet cached files of a directory in one request per aspect.
    fn prefetch(&self, dir: u64) {
        let Ok(list) = self.listing(dir) else { return };
        let mut want: Vec<(u64, String, Value)> = vec![];
        {
            let st = self.st.lock().unwrap();
            let mut total = 0;
            for (_, id) in &list {
                if st.data.get(id).map(|(t, _)| t.elapsed() < self.ttl).unwrap_or(false) {
                    continue;
                }
                let Some(i) = st.inodes.get(id) else { continue };
                if i.dir || i.size > BATCH_FILE || total + i.size > BATCH_TOTAL {
                    continue;
                }
                total += i.size;
                match &i.kind {
                    Kind::File { aspect, node, name } => want.push((*id, aspect.clone(), json!({"node": node, "name": name}))),
                    Kind::Index { aspect, node, dir } => want.push((*id, aspect.clone(), json!({"node": node, "name": dir, "index": true}))),
                    _ => {}
                }
            }
        }
        let mut by_aspect: HashMap<String, Vec<(u64, Value)>> = HashMap::new();
        for (id, a, item) in want {
            by_aspect.entry(a).or_default().push((id, item));
        }
        for (aspect, items) in by_aspect {
            let body: Vec<Value> = items.iter().map(|x| x.1.clone()).collect();
            if let Ok(res) = self.api.files(&aspect, &body) {
                let mut st = self.st.lock().unwrap();
                for ((id, _), d) in items.iter().zip(res) {
                    if let Some(d) = d {
                        st.data.insert(*id, (Instant::now(), Arc::new(d)));
                    }
                }
            }
        }
    }
}

impl Fs {
    /// Start or continue writing `ino`: the buffer holds the current bytes.
    fn wbuf_open(&self, ino: u64) -> Result<(), Errno> {
        if self.st.lock().unwrap().wbuf.contains_key(&ino) {
            return Ok(());
        }
        let kind = self.st.lock().unwrap().inodes.get(&ino).map(|i| i.kind.clone()).ok_or(Errno::ENOENT)?;
        match kind {
            Kind::File { .. } | Kind::Index { .. } | Kind::New { .. } => {}
            _ => return Err(Errno::EACCES),
        }
        let cur = self.data(ino)?;
        self.st.lock().unwrap().wbuf.insert(ino, WBuf { data: cur.as_ref().clone(), dirty: false, opens: 0 });
        Ok(())
    }

    /// Send a dirty buffer to the server (one order). On failure the message goes to the log.
    fn commit(&self, ino: u64) -> Result<(), Errno> {
        let (kind, data, path) = {
            let st = self.st.lock().unwrap();
            let Some(b) = st.wbuf.get(&ino) else { return Ok(()) };
            if !b.dirty {
                return Ok(());
            }
            let i = st.inodes.get(&ino).ok_or(Errno::ENOENT)?;
            (i.kind.clone(), b.data.clone(), i.path.clone())
        };
        let res = match &kind {
            Kind::File { aspect, node, name } => self.api.put_file(aspect, node, name, data),
            Kind::Index { aspect, node, .. } => self.api.put_index(aspect, node, data),
            Kind::New { aspect, folder, name } => self.api.drop_file(aspect, folder, name, data),
            _ => Err("not writable".into()),
        };
        let mut st = self.st.lock().unwrap();
        // what was read before is stale now
        st.data.clear();
        st.listings.clear();
        match res {
            Ok(()) => {
                if let Some(b) = st.wbuf.get_mut(&ino) {
                    b.dirty = false;
                }
                if matches!(kind, Kind::New { .. }) {
                    st.wbuf.remove(&ino);
                    st.inodes.remove(&ino);
                }
                Ok(())
            }
            Err(e) => {
                let what = match &kind {
                    Kind::File { name, .. } | Kind::New { name, .. } => name.clone(),
                    Kind::Index { dir, .. } => format!("{dir}.md"),
                    _ => path,
                };
                st.errors.push(format!("{} {what}: {e}", chrono_now()));
                if st.errors.len() > 100 {
                    st.errors.remove(0);
                }
                if let Some(b) = st.wbuf.get_mut(&ino) {
                    b.dirty = false; // the attempt is over; the file shows the server's state again
                }
                st.wbuf.remove(&ino);
                Err(Errno::EIO)
            }
        }
    }

    /// The entry `name` in directory `parent`, as the kernel's lookup sees it.
    pub fn lookup_name(&self, parent: u64, name: &str) -> Result<FileAttr, Errno> {
        let pending = {
            let st = self.st.lock().unwrap();
            st.inodes.iter().find(|(_, i)| i.parent == parent && matches!(&i.kind, Kind::New { name: n, .. } if n == name)).map(|(id, _)| *id)
        };
        let found = if let Some(p) = pending {
            Some(p)
        } else if self.is_id_root(parent) {
            self.by_id(parent, name)
        } else {
            self.listing(parent)?.into_iter().find(|(n, _)| n == name).map(|x| x.1)
        };
        let id = found.ok_or(Errno::ENOENT)?;
        let mut st = self.st.lock().unwrap();
        let i = st.inodes.get_mut(&id).ok_or(Errno::ENOENT)?;
        i.parent = parent;
        Ok(self.attr(id, i))
    }

    pub fn getattr_of(&self, ino: u64) -> Result<FileAttr, Errno> {
        let st = self.st.lock().unwrap();
        let i = st.inodes.get(&ino).ok_or(Errno::ENOENT)?;
        let mut a = self.attr(ino, i);
        if let Some(b) = st.wbuf.get(&ino) {
            a.size = b.data.len() as u64;
            a.blocks = a.size.div_ceil(512);
        }
        Ok(a)
    }

    /// Names and node ids of a directory (without `.` and `..`).
    pub fn entries(&self, dir: u64) -> Result<Vec<(String, u64)>, Errno> {
        self.listing(dir)
    }

    /// All bytes of a file.
    pub fn content(&self, ino: u64) -> Result<Arc<Vec<u8>>, Errno> {
        self.data(ino)
    }
}

impl fuser::Filesystem for Fs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(name) = name.to_str() else { return reply.error(Errno::ENOENT) };
        match self.lookup_name(parent.0, name) {
            Ok(a) => reply.entry(&self.ttl, &a, Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.getattr_of(ino.0) {
            Ok(a) => reply.attr(&self.ttl, &a),
            Err(e) => reply.error(e),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, _kill: bool, reply: ReplyOpen) {
        if flags.acc_mode() != fuser::OpenAccMode::O_RDONLY {
            return match self.wbuf_open(ino.0) {
                Ok(()) => {
                    if let Some(b) = self.st.lock().unwrap().wbuf.get_mut(&ino.0) {
                        b.opens += 1;
                    }
                    reply.opened(FileHandle(1), FopenFlags::FOPEN_DIRECT_IO)
                }
                Err(e) => reply.error(e),
            };
        }
        let kind = self.st.lock().unwrap().inodes.get(&ino.0).map(|i| (i.dir, matches!(i.kind, Kind::Errors)));
        match kind {
            Some((false, true)) => reply.opened(FileHandle(0), FopenFlags::FOPEN_DIRECT_IO),
            // no kernel page cache across opens: after a write the next open reads fresh bytes (the
            // driver's own cache still saves requests)
            Some((false, false)) => reply.opened(FileHandle(0), FopenFlags::empty()),
            Some(_) => reply.error(Errno::EISDIR),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn read(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, size: u32, _flags: OpenFlags, _lock: Option<fuser::LockOwner>, reply: ReplyData) {
        match self.data(ino.0) {
            Ok(d) => {
                let start = (offset as usize).min(d.len());
                let end = (start + size as usize).min(d.len());
                reply.data(&d[start..end]);
            }
            Err(e) => reply.error(e),
        }
    }

    fn write(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, data: &[u8], _wf: fuser::WriteFlags, _flags: OpenFlags, _lock: Option<fuser::LockOwner>, reply: fuser::ReplyWrite) {
        if let Err(e) = self.wbuf_open(ino.0) {
            return reply.error(e);
        }
        let mut st = self.st.lock().unwrap();
        let b = st.wbuf.get_mut(&ino.0).expect("buffer");
        let end = offset as usize + data.len();
        if b.data.len() < end {
            b.data.resize(end, 0);
        }
        b.data[offset as usize..end].copy_from_slice(data);
        b.dirty = true;
        reply.written(data.len() as u32);
    }

    #[allow(clippy::too_many_arguments)]
    fn setattr(&self, _req: &Request, ino: INodeNo, _mode: Option<u32>, _uid: Option<u32>, _gid: Option<u32>, size: Option<u64>, _atime: Option<fuser::TimeOrNow>, _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>, _fh: Option<FileHandle>, _crtime: Option<SystemTime>, _chgtime: Option<SystemTime>, _bkuptime: Option<SystemTime>, _flags: Option<fuser::BsdFileFlags>, _kill: bool, reply: ReplyAttr) {
        if let Some(n) = size {
            if let Err(e) = self.wbuf_open(ino.0) {
                return reply.error(e);
            }
            let mut st = self.st.lock().unwrap();
            let b = st.wbuf.get_mut(&ino.0).expect("buffer");
            if b.data.len() as u64 != n {
                b.data.resize(n as usize, 0);
                b.dirty = true;
            }
        }
        match self.getattr_of(ino.0) {
            Ok(a) => reply.attr(&self.ttl, &a),
            Err(e) => reply.error(e),
        }
    }

    fn flush(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, _lock: fuser::LockOwner, reply: fuser::ReplyEmpty) {
        match self.commit(ino.0) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn fsync(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, _datasync: bool, reply: fuser::ReplyEmpty) {
        match self.commit(ino.0) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn release(&self, _req: &Request, ino: INodeNo, fh: FileHandle, _flags: OpenFlags, _lock: Option<fuser::LockOwner>, _flush: bool, reply: fuser::ReplyEmpty) {
        if fh.0 == 1 {
            let _ = self.commit(ino.0);
            let mut st = self.st.lock().unwrap();
            let last = st.wbuf.get_mut(&ino.0).map(|b| {
                b.opens = b.opens.saturating_sub(1);
                b.opens == 0 && !b.dirty
            });
            if last == Some(true) {
                st.wbuf.remove(&ino.0);
            }
        }
        reply.ok();
    }

    fn create(&self, _req: &Request, parent: INodeNo, name: &OsStr, _mode: u32, _umask: u32, _flags: i32, _kill: bool, _owner: fuser::Owner, reply: fuser::ReplyCreate) {
        let Some(name) = name.to_str() else { return reply.error(Errno::EINVAL) };
        let mut st = self.st.lock().unwrap();
        let Some(Kind::Dir { aspect, node }) = st.inodes.get(&parent.0).map(|i| i.kind.clone()) else { return reply.error(Errno::EACCES) };
        if !node.starts_with("f:") {
            // only folders of a view can take new files (and only those with dropType, checked by the server)
            return reply.error(Errno::EACCES);
        }
        st.next_new += 1;
        let id = st.next_new;
        let ino = Ino { kind: Kind::New { aspect, folder: node, name: name.to_string() }, dir: false, size: 0, nlink: 1, mtime: SystemTime::now(), parent: parent.0, path: String::new() };
        let attr = self.attr(id, &ino);
        st.inodes.insert(id, ino);
        st.wbuf.insert(id, WBuf { data: vec![], dirty: true, opens: 1 });
        reply.created(&self.ttl, &attr, Generation(0), FileHandle(1), FopenFlags::FOPEN_DIRECT_IO);
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let list = match self.listing(ino.0) {
            Ok(l) => l,
            Err(e) => return reply.error(e),
        };
        let parent = self.st.lock().unwrap().inodes.get(&ino.0).map(|i| i.parent).unwrap_or(1);
        let mut all: Vec<(u64, FileType, String)> = vec![(ino.0, FileType::Directory, ".".into()), (parent, FileType::Directory, "..".into())];
        {
            let st = self.st.lock().unwrap();
            for (n, id) in list {
                let dir = st.inodes.get(&id).map(|i| i.dir).unwrap_or(false);
                all.push((id, if dir { FileType::Directory } else { FileType::RegularFile }, n));
            }
        }
        for (i, (id, kind, name)) in all.into_iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(id), (i + 1) as u64, kind, &name) {
                break;
            }
        }
        reply.ok();
    }
}

fn chrono_now() -> String {
    let s = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("@{s}")
}

/// Mount at `mountpoint` and serve until unmounted (reading and writing, SPEC-004).
pub fn mount(api: Api, mountpoint: &str, ttl: Duration) -> std::io::Result<()> {
    let mut cfg = fuser::Config::default();
    cfg.mount_options = vec![fuser::MountOption::FSName("i3cm".into()), fuser::MountOption::Subtype("i3cm".into()), fuser::MountOption::NoExec];
    cfg.n_threads = Some(4);
    fuser::mount(Fs::new(api, ttl), mountpoint, &cfg)
}
