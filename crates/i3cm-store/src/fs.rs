//! The data as a file tree for the tools, laid out as the FUSE driver shows it: the root holds
//! a directory per aspect (with its index `<aspect>.md`); below, a node is a directory `<name>/`
//! with its index `<name>.md` beside it, a file is a file. `/_id/<id>` reaches every object.

use crate::aspect::{NodeOut, Page, VIEW_ID};
use crate::{Ctx, Store};
use i3cm_core::fhirpath::Item;
use i3cm_core::ErrorKind;
use i3cm_tools::{dir_name, fs_name, project, Entry, Fut, Named, Node, Slot, Tree};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use uuid::Uuid;

const PAGE: i64 = 1000;

/// Reference of a node in the tree (stored as JSON in [`Node::handle`]).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "h", rename_all = "lowercase")]
enum H {
    Root,
    Dir { aspect: String, node: String },
    File { aspect: String, node: String, name: String },
    Index { aspect: String, node: String, dir: String },
}

fn handle(h: &H) -> String {
    serde_json::to_string(h).unwrap_or_default()
}

fn mtime(n: &NodeOut) -> Option<i64> {
    n.changed_at.map(|t| t.timestamp())
}

fn child_path(parent: &str, name: &str) -> String {
    if parent == "/" { format!("/{name}") } else { format!("{parent}/{name}") }
}

pub struct FsTree<'a> {
    store: &'a Store,
    ctx: &'a Ctx,
    lang: String,
    cache: Mutex<HashMap<String, Vec<Node>>>,
}

impl<'a> FsTree<'a> {
    pub fn new(store: &'a Store, ctx: &'a Ctx, lang: &str) -> Self {
        FsTree { store, ctx, lang: lang.to_string(), cache: Mutex::new(HashMap::new()) }
    }

    fn root() -> Node {
        Node { path: "/".into(), entry: Entry { name: "/".into(), dir: true, nlink: 1, ..Default::default() }, handle: handle(&H::Root), grep: false, key: None }
    }

    fn list_root(&self) -> Vec<Node> {
        let aspects = self.store.aspect_list(&self.lang);
        let mut out = vec![];
        for a in aspects {
            let name = a["name"].as_str().unwrap_or_default().to_string();
            let fname = fs_name(&name);
            out.push(Node {
                path: child_path("/", &fname),
                entry: Entry { name: fname.clone(), dir: true, nlink: 1, ..Default::default() },
                handle: handle(&H::Dir { aspect: name.clone(), node: String::new() }),
                grep: false,
                key: None,
            });
            let iname = format!("{fname}.md");
            out.push(Node {
                path: child_path("/", &iname),
                entry: Entry { name: iname, dir: false, size: a["size"].as_u64().unwrap_or(0), nlink: 1, mtime: None, ino: a["ino"].as_u64() },
                handle: handle(&H::Index { aspect: name, node: String::new(), dir: fname }),
                grep: true,
                key: None,
            });
        }
        out
    }

    /// Nodes for the entries of a complete listing (shared with the FUSE driver's layout).
    /// `renamed`: sizes of indexes shown under another name than in the listing (key: node, NUL, name).
    pub fn nodes(&self, parent: &str, aspect: &str, children: &[NodeOut], renamed: &HashMap<String, u64>) -> Vec<Node> {
        let named: Vec<Named> = children.iter().map(|c| Named { name: &c.name, file: c.file, fallback: c.fallback_name.as_deref(), key: &c.node }).collect();
        let entries = project(&named);
        let mut out = vec![];
        for (name, slot) in entries.iter().cloned() {
            let path = child_path(parent, &name);
            match slot {
                Slot::Dir(i) => {
                    let c = &children[i];
                    let key = c.id.filter(|_| c.node.starts_with("o:")).map(|id| format!("o:{id}"));
                    out.push(Node {
                        path,
                        entry: Entry { name, dir: true, size: 0, nlink: 1, mtime: mtime(c), ino: None },
                        handle: handle(&H::Dir { aspect: aspect.into(), node: c.node.clone() }),
                        grep: false,
                        key,
                    });
                }
                Slot::Index(i) => {
                    let c = &children[i];
                    let obj = c.id.is_some() && c.node.starts_with("o:");
                    let dir = dir_name(&entries, i).unwrap_or(&c.name).to_string();
                    // under another name than assumed in the listing (collision) the links differ
                    let (size, ino) = if dir == c.name { (c.size.unwrap_or(0), c.ino) } else { (renamed.get(&format!("{}\0{dir}", c.node)).copied().unwrap_or(0), Some(i3cm_tools::ino(&["i", aspect, &c.node, &dir]))) };
                    out.push(Node {
                        path,
                        entry: Entry { name, dir: false, size, nlink: if obj { 2 } else { 1 }, mtime: mtime(c), ino },
                        handle: handle(&H::Index { aspect: aspect.into(), node: c.node.clone(), dir }),
                        grep: true,
                        key: None,
                    });
                }
                Slot::File(i) => {
                    let c = &children[i];
                    let (grep, key) = match &c.src {
                        Some(Item::Text(id, tt)) => (self.store.model.text_type(tt).map(|t| t.grep).unwrap_or(true), Some(format!("t:{id}:{tt}"))),
                        _ => (false, None),
                    };
                    out.push(Node {
                        path,
                        entry: Entry { name, dir: false, size: c.size.unwrap_or(0), nlink: 2, mtime: mtime(c), ino: c.ino },
                        handle: handle(&H::File { aspect: aspect.into(), node: c.node.clone(), name: c.display_name.clone() }),
                        grep,
                        key,
                    });
                }
            }
        }
        out
    }

    async fn list_dir(&self, parent: &str, aspect: &str, node: &str) -> Result<Vec<Node>, String> {
        let mut all = vec![];
        let mut offset = 0;
        loop {
            // keyset where the listing supports it (group levels), offset otherwise
            let after = all.last().map(|c: &crate::aspect::NodeOut| c.node.clone());
            let page = self.store.aspect_children_paged(self.ctx, aspect, node, &self.lang, Page { offset, limit: Some(PAGE), after }).await.map_err(|e| e.description)?;
            let n = page.len() as i64;
            all.extend(page);
            if n < PAGE {
                break;
            }
            offset += PAGE;
        }
        // indexes under another name than the listing assumed (collisions) have other links
        let named: Vec<Named> = all.iter().map(|c| Named { name: &c.name, file: c.file, fallback: c.fallback_name.as_deref(), key: &c.node }).collect();
        let entries = project(&named);
        let mut renamed = HashMap::new();
        for (_, slot) in &entries {
            if let Slot::Index(i) = slot {
                let dir = dir_name(&entries, *i).unwrap_or_default();
                if dir != all[*i].name {
                    let idx = self.store.aspect_index(self.ctx, aspect, &all[*i].node, Some(dir), &self.lang).await.map_err(|e| e.description)?;
                    renamed.insert(format!("{}\0{dir}", all[*i].node), idx.len() as u64);
                }
            }
        }
        Ok(self.nodes(parent, aspect, &all, &renamed))
    }

    /// `/_id/<id>` and `/_id/<id>.md` (the entry of `_id` lists nothing).
    async fn by_id(&self, seg: &str) -> Result<Option<Node>, String> {
        let (u, index) = match seg.strip_suffix(".md") {
            Some(u) => (u, true),
            None => (seg, false),
        };
        let Ok(id) = Uuid::parse_str(u) else { return Ok(None) };
        let node = format!("o:{id}");
        let n = match self.store.aspect_entry(self.ctx, VIEW_ID, &node, Some(u), &self.lang).await {
            Ok(n) => n,
            Err(e) if e.kind == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.description),
        };
        let path = format!("/{VIEW_ID}/{seg}");
        Ok(Some(if index {
            Node {
                path,
                entry: Entry { name: seg.into(), dir: false, size: n.size.unwrap_or(0), nlink: 2, mtime: mtime(&n), ino: n.ino },
                handle: handle(&H::Index { aspect: VIEW_ID.into(), node, dir: u.into() }),
                grep: true,
                key: None,
            }
        } else {
            Node {
                path,
                entry: Entry { name: seg.into(), dir: true, nlink: 1, mtime: mtime(&n), ..Default::default() },
                handle: handle(&H::Dir { aspect: VIEW_ID.into(), node }),
                grep: false,
                key: Some(format!("o:{id}")),
            }
        }))
    }
}

impl Tree for FsTree<'_> {
    fn lookup<'b>(&'b self, path: &'b str) -> Fut<'b, Result<Option<Node>, String>> {
        Box::pin(async move {
            let mut cur = Self::root();
            for seg in path.split('/').filter(|s| !s.is_empty()) {
                if !cur.entry.dir {
                    return Ok(None);
                }
                if let Ok(H::Dir { aspect, node }) = serde_json::from_str::<H>(&cur.handle)
                    && aspect == VIEW_ID && node.is_empty() {
                        match self.by_id(seg).await? {
                            Some(n) => {
                                cur = n;
                                continue;
                            }
                            None => return Ok(None),
                        }
                    }
                let list = self.list(&cur).await?;
                match list.into_iter().find(|n| n.entry.name == seg) {
                    Some(n) => cur = n,
                    None => return Ok(None),
                }
            }
            Ok(Some(cur))
        })
    }

    fn list<'b>(&'b self, dir: &'b Node) -> Fut<'b, Result<Vec<Node>, String>> {
        Box::pin(async move {
            if let Some(l) = self.cache.lock().unwrap().get(&dir.path) {
                return Ok(l.clone());
            }
            let l = match serde_json::from_str::<H>(&dir.handle).map_err(|e| e.to_string())? {
                H::Root => self.list_root(),
                H::Dir { aspect, node } => self.list_dir(&dir.path, &aspect, &node).await?,
                _ => return Err("Not a directory".into()),
            };
            self.cache.lock().unwrap().insert(dir.path.clone(), l.clone());
            Ok(l)
        })
    }

    fn read<'b>(&'b self, file: &'b Node) -> Fut<'b, Result<Vec<u8>, String>> {
        Box::pin(async move {
            match serde_json::from_str::<H>(&file.handle).map_err(|e| e.to_string())? {
                H::File { aspect, node, name } => self.store.aspect_file(self.ctx, &aspect, &node, &name).await.map(|x| x.0).map_err(|e| e.description),
                H::Index { aspect, node, dir } => self.store.aspect_index(self.ctx, &aspect, &node, Some(&dir), &self.lang).await.map(String::into_bytes).map_err(|e| e.description),
                _ => Err("Is a directory".into()),
            }
        })
    }

    fn candidates<'b>(&'b self, literal: &'b str, icase: bool) -> Fut<'b, Option<HashSet<String>>> {
        Box::pin(async move { self.store.text_candidates(self.ctx, literal, icase).await.ok() })
    }

    fn write<'b>(&'b self, files: &'b [(Node, Vec<u8>)]) -> Fut<'b, Result<i64, String>> {
        Box::pin(async move {
            let mut items = vec![];
            for (n, data) in files {
                let h: H = serde_json::from_str(&n.handle).map_err(|_| format!("{}: not writable", n.path))?;
                items.push(match h {
                    H::File { aspect, node, name } => crate::write::WriteItem { aspect, node, name: Some(name), data: data.clone(), drop: false },
                    H::Index { aspect, node, .. } => crate::write::WriteItem { aspect, node, name: None, data: data.clone(), drop: false },
                    _ => return Err(format!("{}: not writable", n.path)),
                });
            }
            let mut ctx = self.ctx.clone();
            self.store.write_files(&mut ctx, &items, &self.lang).await.map(|r| r.txn).map_err(|e| format!("{} ({})", e.description, e.error_id))
        })
    }
}
