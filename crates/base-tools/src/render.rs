//! Rendering of records as the GNU tools print them: (stdout bytes, stderr text).

use crate::{Entry, Rec, Response};
use chrono::{DateTime, Utc};

fn mode(e: &Entry) -> &'static str {
    if e.dir { "dr-xr-xr-x" } else { "-r--r--r--" }
}

fn ls_time(t: Option<i64>, now: i64) -> String {
    let t = t.unwrap_or(0);
    let Some(d) = DateTime::<Utc>::from_timestamp(t, 0) else { return "Jan  1  1970".into() };
    // as GNU ls: time of day for the last six months, the year otherwise
    if (now - t).abs() < 182 * 24 * 3600 {
        d.format("%b %e %H:%M").to_string()
    } else {
        d.format("%b %e  %Y").to_string()
    }
}

fn digits(n: u64) -> usize {
    n.to_string().len()
}

/// Format records like the real tool: stdout bytes and stderr text.
pub fn render(r: &Response) -> (Vec<u8>, String) {
    let now = Utc::now().timestamp();
    let mut out: Vec<u8> = vec![];
    let mut err = String::new();
    let mut first_block = true;
    let width = {
        let mut max = 0;
        let mut fields = 0;
        let mut recs = 0;
        for x in &r.records {
            if let Rec::Counts { lines, words, bytes, .. } = x {
                recs += 1;
                let v: Vec<u64> = [lines, words, bytes].iter().filter_map(|o| **o).collect();
                fields = fields.max(v.len());
                max = max.max(v.into_iter().max().unwrap_or(0));
            }
        }
        if recs <= 1 && fields <= 1 { 1 } else { digits(max) }
    };
    for x in &r.records {
        match x {
            Rec::Listing { header, long, entries } => {
                if let Some(h) = header {
                    if !first_block {
                        out.push(b'\n');
                    }
                    out.extend(format!("{h}:\n").as_bytes());
                }
                first_block = false;
                if *long {
                    if header.is_some() || entries.iter().any(|e| e.dir) {
                        let blocks: u64 = entries.iter().map(|e| e.size.div_ceil(1024)).sum();
                        out.extend(format!("total {blocks}\n").as_bytes());
                    }
                    let w = entries.iter().map(|e| digits(e.size)).max().unwrap_or(1);
                    let lw = entries.iter().map(|e| digits(e.nlink as u64)).max().unwrap_or(1);
                    for e in entries {
                        out.extend(format!("{} {:>lw$} base base {:>w$} {} {}\n", mode(e), e.nlink, e.size, ls_time(e.mtime, now), e.name).as_bytes());
                    }
                } else {
                    for e in entries {
                        out.extend(e.name.as_bytes());
                        out.push(b'\n');
                    }
                }
            }
            Rec::Data { header, data } => {
                if let Some(h) = header {
                    if !first_block {
                        out.push(b'\n');
                    }
                    out.extend(format!("==> {h} <==\n").as_bytes());
                }
                first_block = false;
                out.extend(data);
            }
            Rec::Counts { path, lines, words, bytes } => {
                let mut cols: Vec<String> = [lines, words, bytes].iter().filter_map(|o| **o).map(|n| format!("{n:>width$}")).collect();
                if let Some(p) = path {
                    cols.push(p.clone());
                }
                out.extend(cols.join(" ").as_bytes());
                out.push(b'\n');
            }
            Rec::Stat { path, entry } => {
                let kind = if entry.dir { "directory" } else { "regular file" };
                let t = DateTime::<Utc>::from_timestamp(entry.mtime.unwrap_or(0), 0).unwrap_or_default();
                let perm = if entry.dir { "0555" } else { "0444" };
                out.extend(
                    format!(
                        "  File: {path}\n  Size: {:<15} Blocks: {:<10} IO Block: 4096   {kind}\nDevice: 0,0\tInode: {:<11} Links: {}\nAccess: ({perm}/{})  Uid: (base)   Gid: (base)\nModify: {}\n",
                        entry.size,
                        entry.size.div_ceil(512),
                        entry.ino.unwrap_or(0),
                        entry.nlink,
                        mode(entry),
                        t.format("%Y-%m-%d %H:%M:%S%.9f %z")
                    )
                    .as_bytes(),
                );
            }
            Rec::Match { path, line, text } => {
                let mut s = String::new();
                if let Some(p) = path {
                    s.push_str(p);
                    s.push(':');
                }
                if let Some(n) = line {
                    s.push_str(&format!("{n}:"));
                }
                s.push_str(text);
                s.push('\n');
                out.extend(s.as_bytes());
            }
            Rec::Binary { path } => out.extend(format!("Binary file {path} matches\n").as_bytes()),
            Rec::Name { path } => out.extend(format!("{path}\n").as_bytes()),
            Rec::Count { path, count } => match path {
                Some(p) => out.extend(format!("{p}:{count}\n").as_bytes()),
                None => out.extend(format!("{count}\n").as_bytes()),
            },
            Rec::Error { message } => {
                err.push_str(message);
                err.push('\n');
            }
        }
    }
    (out, err)
}
