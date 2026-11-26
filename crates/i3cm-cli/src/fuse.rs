//! The FUSE driver (read-only). It shows the API faithfully: a directory lists exactly the
//! children from `children` (a node as `<name>/` plus its index `<name>.md`, a file as `<name>`),
//! a file holds exactly the bytes from `file` or `index`, and `stat` matches. Directories are
//! fetched completely (paged internally) and the files of a directory are read in one request,
//! so a recursive reader costs requests per directory, not per file.

use crate::api::{Api, Child};
use i3cm_tools::{fs_name, project, Slot};
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

#[derive(Debug, Clone)]
enum Kind {
    Root,
    Dir { aspect: String, node: String },
    File { aspect: String, node: String, name: String },
    Index { aspect: String, node: String, dir: String },
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
        let mut st = State { next_dir: DIR_BASE, ..Default::default() };
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
            perm: if i.dir { 0o555 } else { 0o444 },
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
        let pairs: Vec<(&str, bool)> = children.iter().map(|c| (c.name.as_str(), c.file)).collect();
        let mut out = vec![];
        for (name, slot) in project(&pairs) {
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
                    let id = c.ino.unwrap_or_else(|| i3cm_tools::ino(&["i", aspect, &c.node]));
                    let kind = Kind::Index { aspect: aspect.into(), node: c.node.clone(), dir: c.name.clone() };
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
                st.listings.insert(dir, (Instant::now(), out.clone()));
                out
            }
            Kind::Dir { aspect, node } => {
                let children = self.api.children(&aspect, &node).map_err(|_| Errno::EIO)?;
                let mut st = self.st.lock().unwrap();
                let out = self.register(&mut st, dir, &path, &aspect, &children);
                st.listings.insert(dir, (Instant::now(), out.clone()));
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
            let i = st.inodes.get(&id).ok_or(Errno::ENOENT)?.clone();
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
    /// The entry `name` in directory `parent`, as the kernel's lookup sees it.
    pub fn lookup_name(&self, parent: u64, name: &str) -> Result<FileAttr, Errno> {
        let found = if self.is_id_root(parent) { self.by_id(parent, name) } else { self.listing(parent)?.into_iter().find(|(n, _)| n == name).map(|x| x.1) };
        let id = found.ok_or(Errno::ENOENT)?;
        let mut st = self.st.lock().unwrap();
        let i = st.inodes.get_mut(&id).ok_or(Errno::ENOENT)?;
        i.parent = parent;
        Ok(self.attr(id, i))
    }

    pub fn getattr_of(&self, ino: u64) -> Result<FileAttr, Errno> {
        let st = self.st.lock().unwrap();
        st.inodes.get(&ino).map(|i| self.attr(ino, i)).ok_or(Errno::ENOENT)
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

    fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, _kill: bool, reply: ReplyOpen) {
        match self.st.lock().unwrap().inodes.get(&ino.0) {
            Some(i) if !i.dir => reply.opened(FileHandle(0), FopenFlags::FOPEN_KEEP_CACHE),
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

/// Mount read-only at `mountpoint` and serve until unmounted.
pub fn mount(api: Api, mountpoint: &str, ttl: Duration) -> std::io::Result<()> {
    let mut cfg = fuser::Config::default();
    cfg.mount_options = vec![fuser::MountOption::FSName("i3cm".into()), fuser::MountOption::Subtype("i3cm".into()), fuser::MountOption::RO, fuser::MountOption::NoExec];
    cfg.n_threads = Some(4);
    fuser::mount(Fs::new(api, ttl), mountpoint, &cfg)
}
