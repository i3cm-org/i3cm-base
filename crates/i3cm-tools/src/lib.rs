//! i3cm-tools: the read-only Unix tools of i3cm (`ls`, `cat`, `head`, `tail`, `wc`, `stat`,
//! `grep`, `find`). One option parser, one engine and one renderer serve the command line
//! program and the server alike:
//!
//! * [`parse`] turns an argv into a [`Call`] (unknown options are always an error);
//! * [`run`] executes a call against a [`Tree`] and returns structured [`Rec`]ords;
//! * [`render`] formats records like the GNU tool would.

mod engine;
mod parse;
mod render;

pub use engine::{run, Fut, Node, Tree};
pub use parse::{check, parse, supported};
pub use render::render;


use serde::{Deserialize, Serialize};

/// The tools, by their Unix names.
pub const TOOLS: &[&str] = &["ls", "cat", "head", "tail", "wc", "stat", "grep", "find"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Ls,
    Cat,
    Head,
    Tail,
    Wc,
    Stat,
    Grep,
    Find,
}

impl Tool {
    pub fn from_name(name: &str) -> Option<Tool> {
        let n = name.rsplit('/').next().unwrap_or(name);
        let n = n.strip_prefix("i3cm-").unwrap_or(n);
        Some(match n {
            "ls" => Tool::Ls,
            "cat" => Tool::Cat,
            "head" => Tool::Head,
            "tail" => Tool::Tail,
            "wc" => Tool::Wc,
            "stat" => Tool::Stat,
            "grep" => Tool::Grep,
            "find" => Tool::Find,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        TOOLS[self as usize]
    }
}

/// A test of a `find` expression; all tests must hold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "test", content = "arg")]
pub enum FindTest {
    Name(String),
    Iname(String),
    Path(String),
    /// `f` or `d`.
    Type(String),
}

/// `-exec cmd args… {} ;` (one call per file) or `+` (one call for all files).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exec {
    pub argv: Vec<String>,
    pub batch: bool,
}

/// Options of all tools; each tool accepts only its own (see [`supported`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Options {
    /// ls `-l`
    pub long: bool,
    /// ls `-a`
    pub all: bool,
    /// ls `-R`, grep `-r`
    pub recursive: bool,
    /// head/tail `-n`
    pub lines: Option<u64>,
    /// head/tail `-c`
    pub bytes: Option<u64>,
    /// tail `-n +N`/`-c +N`: count from the start
    pub from_start: bool,
    /// wc `-l`
    pub count_lines: bool,
    /// wc `-w`
    pub count_words: bool,
    /// wc `-c`
    pub count_bytes: bool,
    /// grep `-e` or the first operand
    pub patterns: Vec<String>,
    /// grep `-i`
    pub ignore_case: bool,
    /// grep `-n`
    pub line_number: bool,
    /// grep `-l`
    pub files_with_matches: bool,
    /// grep `-c`
    pub count: bool,
    /// grep `-v`
    pub invert: bool,
    /// grep `-w`
    pub word: bool,
    /// grep `-F`
    pub fixed: bool,
    /// grep `-E`
    pub extended: bool,
    /// find tests
    pub tests: Vec<FindTest>,
    pub maxdepth: Option<u32>,
    pub mindepth: Option<u32>,
    pub exec: Option<Exec>,
}

/// A tool call: paths are relative to `cwd` (a path in the tree, `/` is the root).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
    pub cwd: String,
    pub paths: Vec<String>,
    /// How each path is shown in the output (defaults to the path as given).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub display: Vec<String>,
    pub options: Options,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub tool: Tool,
    #[serde(flatten)]
    pub req: Request,
}

/// An entry of the tree as `ls -l` and `stat` show it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    pub nlink: u32,
    /// Seconds since the epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ino: Option<u64>,
}

/// One structured result record; the order of records is the order of the output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Rec {
    /// A directory listing (or the files named on the command line).
    Listing {
        #[serde(skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        long: bool,
        entries: Vec<Entry>,
    },
    /// Bytes of a file (cat, head, tail), base64 in JSON.
    Data {
        #[serde(skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    Counts {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lines: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        words: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
    },
    Stat {
        path: String,
        entry: Entry,
    },
    /// A matching line (grep).
    Match {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        line: Option<u64>,
        text: String,
    },
    /// A binary file with a match (grep).
    Binary {
        path: String,
    },
    /// A path (grep -l, find).
    Name {
        path: String,
    },
    /// A count of matching lines (grep -c).
    Count {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        count: u64,
    },
    /// A message for stderr.
    Error {
        message: String,
    },
}

/// Result of a call: records plus the exit status as with GNU.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub status: i32,
    pub records: Vec<Rec>,
}

/// A rejected call (unknown option, missing operand).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolError {
    pub error_id: String,
    pub message: String,
    pub supported: Vec<String>,
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.supported.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{} (supported: {})", self.message, self.supported.join(" "))
        }
    }
}

mod b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

/// File name of an entry shown under a display name: no `/`, no NUL, not empty, not `.`/`..`,
/// at most 255 bytes.
pub fn fs_name(display: &str) -> String {
    let mut s: String = display.chars().map(|c| if c == '/' || c == '\0' { '_' } else { c }).collect();
    if s.is_empty() || s == "." || s == ".." {
        s = format!("_{s}");
    }
    if s.len() > 255 {
        let mut cut = 250;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

/// One entry of a complete directory listing as input for [`project`]: its name in paths, whether
/// it is a file, the name by the fallback rule of its aspect (if any) and a stable key (the node
/// id) that orders entries whose names still collide.
#[derive(Debug, Clone, Copy)]
pub struct Named<'a> {
    pub name: &'a str,
    pub file: bool,
    pub fallback: Option<&'a str>,
    pub key: &'a str,
}

impl<'a> Named<'a> {
    pub fn new(name: &'a str, file: bool, key: &'a str) -> Self {
        Named { name, file, fallback: None, key }
    }
}

/// Unique names for a complete listing `(name, fallback, key)`, in input order. An entry whose
/// name occurs more than once takes its fallback name if it has one; names that still collide
/// keep the name for the smallest key, the others get ` ~2`, ` ~3`, … in key order. The result
/// depends only on the entries, never on their order, so a name always means the same entry.
pub fn unique_names(items: &[(&str, Option<&str>, &str)]) -> Vec<String> {
    use std::collections::{BTreeMap, HashMap, HashSet};
    let mut count: HashMap<&str, usize> = HashMap::new();
    for (n, _, _) in items {
        *count.entry(n).or_default() += 1;
    }
    let cand: Vec<String> = items
        .iter()
        .map(|(n, f, _)| match f {
            Some(f) if count[n] > 1 => f.to_string(),
            _ => n.to_string(),
        })
        .collect();
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, c) in cand.iter().enumerate() {
        groups.entry(c.as_str()).or_default().push(i);
    }
    let mut taken: HashSet<String> = cand.iter().cloned().collect();
    let mut out = cand.clone();
    for (c, mut idx) in groups {
        if idx.len() < 2 {
            continue;
        }
        idx.sort_by(|a, b| items[*a].2.cmp(items[*b].2).then(a.cmp(b)));
        let mut k = 2;
        for &i in &idx[1..] {
            let mut name = format!("{c} ~{k}");
            while taken.contains(&name) {
                k += 1;
                name = format!("{c} ~{k}");
            }
            taken.insert(name.clone());
            out[i] = name;
            k += 1;
        }
    }
    out
}

/// What an entry of a directory is: a node's directory, the index beside it, or a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Dir(usize),
    Index(usize),
    File(usize),
}

/// Directory entries for a complete listing, in order: a node gives the directory `<name>` and its
/// index `<name>.md`, a file gives `<name>`. Names are made unique as in [`unique_names`]; the slot
/// points into `children`.
pub fn project(children: &[Named]) -> Vec<(String, Slot)> {
    let base = unique_names(&children.iter().map(|c| (c.name, c.fallback, c.key)).collect::<Vec<_>>());
    let mut seq: Vec<(String, Slot, String)> = vec![];
    for (i, (b, c)) in base.iter().zip(children).enumerate() {
        if c.file {
            seq.push((b.clone(), Slot::File(i), format!("{}\0f\0{}", c.key, c.name)));
        } else {
            seq.push((b.clone(), Slot::Dir(i), format!("{}\0d", c.key)));
            seq.push((format!("{b}.md"), Slot::Index(i), format!("{}\0i", c.key)));
        }
    }
    let names = unique_names(&seq.iter().map(|x| (x.0.as_str(), None, x.2.as_str())).collect::<Vec<_>>());
    seq.into_iter().zip(names).map(|((_, slot, _), n)| (n, slot)).collect()
}

/// The directory name of child `i` in a result of [`project`] (also the name its index links from).
pub fn dir_name(entries: &[(String, Slot)], i: usize) -> Option<&str> {
    entries.iter().find(|e| e.1 == Slot::Dir(i)).map(|e| e.0.as_str())
}

/// Stable inode number from a key (content: `["c", id]`, text: `["t", id, textType]`, object
/// file: `["i", aspect, id]`); below 2^53 so that JSON clients keep it exact.
pub fn ino(parts: &[&str]) -> u64 {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    let d = h.finalize();
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[..8]);
    (u64::from_be_bytes(b) & ((1 << 53) - 1)).max(2)
}

/// Normalise `p` against `cwd` into an absolute tree path (`/a/b`), resolving `.` and `..`.
pub fn join(cwd: &str, p: &str) -> String {
    let mut parts: Vec<&str> = vec![];
    let full = if p.starts_with('/') { p.to_string() } else { format!("{cwd}/{p}") };
    for seg in full.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Shell-style pattern match (`*`, `?`, `[...]`, `\`), `*` also matching `/`.
pub fn glob(pat: &str, s: &str, icase: bool) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = if icase {
        (pat.to_lowercase().chars().collect(), s.to_lowercase().chars().collect())
    } else {
        (pat.chars().collect(), s.chars().collect())
    };
    fn class(p: &[char], c: char) -> Option<(bool, usize)> {
        // returns (matched, length of the class in p) for p starting at '['
        let mut i = 1;
        let neg = matches!(p.get(1), Some('!') | Some('^'));
        if neg {
            i += 1;
        }
        let mut hit = false;
        let mut first = true;
        while i < p.len() && (p[i] != ']' || first) {
            first = false;
            if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
                if p[i] <= c && c <= p[i + 2] {
                    hit = true;
                }
                i += 3;
            } else {
                if p[i] == c {
                    hit = true;
                }
                i += 1;
            }
        }
        if i >= p.len() {
            return None;
        }
        Some((hit != neg, i + 1))
    }
    fn m(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|k| m(&p[1..], &t[k..])),
            Some('?') => !t.is_empty() && m(&p[1..], &t[1..]),
            Some('[') => match (t.first(), class(p, *t.first().unwrap_or(&'\0'))) {
                (Some(_), Some((ok, len))) => ok && m(&p[len..], &t[1..]),
                (Some(c), None) => *c == '[' && m(&p[1..], &t[1..]),
                _ => false,
            },
            Some('\\') if p.len() > 1 => t.first() == Some(&p[1]) && m(&p[2..], &t[1..]),
            Some(c) => t.first() == Some(c) && m(&p[1..], &t[1..]),
        }
    }
    m(&p, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_paths_and_globs() {
        assert_eq!(fs_name("a/b"), "a_b");
        assert_eq!(fs_name(".."), "_..");
        let n = |v: &[(&str, &str)]| unique_names(&v.iter().map(|(a, k)| (*a, None, *k)).collect::<Vec<_>>());
        assert_eq!(n(&[("x", "1"), ("y", "2"), ("x", "3"), ("x", "4")]), vec!["x", "y", "x ~2", "x ~3"]);
        // numbering follows the key, not the order of the listing
        assert_eq!(n(&[("x", "9"), ("x", "1"), ("x", "5")]), vec!["x ~3", "x", "x ~2"]);
        // a taken name is skipped
        assert_eq!(n(&[("x", "1"), ("x", "2"), ("x ~2", "0")]), vec!["x", "x ~3", "x ~2"]);
        // the fallback applies only to colliding names, then the key decides
        let f = unique_names(&[("a", Some("a (1)"), "k1"), ("a", Some("a (2)"), "k2"), ("b", Some("b (3)"), "k3"), ("a", Some("a (2)"), "k0")]);
        assert_eq!(f, vec!["a (1)", "a (2) ~2", "b", "a (2)"]);
        let p = project(&[Named::new("a", false, "o:1"), Named::new("f", true, "o:2"), Named::new("a", false, "o:3")]);
        let names: Vec<&str> = p.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(names, vec!["a", "a.md", "f", "a ~2", "a ~2.md"]);
        assert_eq!(p[4].1, Slot::Index(2));
        assert_eq!(join("/a/b", "../c/./d"), "/a/c/d");
        assert_eq!(join("/a", "/x/.."), "/");
        assert!(glob("*.md", "mail.md", false));
        assert!(glob("*.MD", "mail.md", true));
        assert!(!glob("*.md", "mail.eml", false));
        assert!(glob("file[0-9].pdf", "file1.pdf", false));
        assert!(glob("*/Anh*/*", "x/Anhänge/f", false));
        assert_ne!(ino(&["c", "1"]), ino(&["c", "2"]));
        assert!(ino(&["c", "1"]) < (1 << 53));
    }
}
