//! Writing through views (writable FUSE, `sed -i`).
//!
//! Every write becomes one order: rights, end state and (later) hooks apply as always. Several
//! files written together (e.g. `sed -i` over many files) are one order, all or nothing.
//! Writable are the content of an object, its texts and the attributes in the front matter of
//! its index; a file copied into a folder with `dropType` creates a new object.

use crate::context::{db, Ctx};
use crate::order::{Op, Order, OrderResult};
use crate::read::types_of;
use crate::Store;
use i3cm_core::fhirpath::Item;
use i3cm_core::{BaseError, Result};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

/// One file to write: `name` is a file of the node, `None` its index (`<Name>.md`). With
/// `drop`, `node` is a folder and `name` the new file.
#[derive(Debug, Clone)]
pub struct WriteItem {
    pub aspect: String,
    pub node: String,
    pub name: Option<String>,
    pub data: Vec<u8>,
    pub drop: bool,
}

fn media_type_of(name: &str) -> &'static str {
    match name.rsplit('.').next().map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("md") => "text/markdown",
        Some("html") | Some("htm") => "text/html",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("eml") => "message/rfc822",
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        _ => "application/octet-stream",
    }
}

/// Split an index into front matter (`key: <json>` lines) and the rest.
fn split_index(s: &str) -> Result<(BTreeMap<String, Value>, String)> {
    let bad = |why: &str| BaseError::invalid("index.bad_front_matter", "the front matter has lines `key: <JSON value>` between two `---` lines").with("reason", why);
    let rest = s.strip_prefix("---\n").ok_or_else(|| bad("no front matter"))?;
    let end = rest.find("\n---\n").ok_or_else(|| bad("front matter not closed"))?;
    let mut out = BTreeMap::new();
    for line in rest[..end].lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (k, v) = line.split_once(':').ok_or_else(|| bad("line without `:`"))?;
        let v: Value = serde_json::from_str(v.trim()).map_err(|_| bad("value is not JSON").with("key", k.trim()))?;
        out.insert(k.trim().to_string(), v);
    }
    Ok((out, rest[end + 5..].to_string()))
}

impl Store {
    /// The content or text behind a file of a view node.
    pub async fn file_item(&self, ctx: &Ctx, aspect: &str, node: &str, name: &str) -> Result<Item> {
        let files = self.files_of_node(ctx, aspect, node).await?;
        files.into_iter().find(|(f, _)| f == name).map(|(_, it)| it).ok_or_else(|| BaseError::not_found("aspect.no_file", "no such file").with("name", name))
    }

    /// Write files of views as one order.
    pub async fn write_files(&self, ctx: &mut Ctx, items: &[WriteItem], lang: &str) -> Result<OrderResult> {
        let mut ops = vec![];
        for (i, w) in items.iter().enumerate() {
            if w.drop {
                ops.extend(self.drop_ops(ctx, w, i).await?);
                continue;
            }
            match &w.name {
                None => ops.extend(self.index_ops(ctx, w, lang).await?),
                Some(name) => match match self.file_write_item(ctx, &w.aspect, &w.node, name).await? {
                    Some(it) => it,
                    None => self.file_item(ctx, &w.aspect, &w.node, name).await?,
                } {
                    Item::Content(id) => {
                        let (_, media, filename) = self.get_content(ctx, id).await?;
                        let sha = self.put_blob(&w.data).await?;
                        ops.push(Op::SetContent { id: id.to_string(), sha256: sha, media_type: media, size: w.data.len() as i64, filename });
                    }
                    Item::Text(id, tt) => {
                        let content = String::from_utf8(w.data.clone()).map_err(|_| BaseError::invalid("text.not_utf8", "texts are UTF-8").with("name", name.clone()))?;
                        ops.push(Op::SetText { id: id.to_string(), text_type: tt, content });
                    }
                    _ => return Err(BaseError::invalid("aspect.not_writable", "this file is not writable").with("name", name.clone())),
                },
            }
        }
        if ops.is_empty() {
            return Err(BaseError::invalid("write.unchanged", "nothing to write"));
        }
        self.execute(ctx, &Order { dry_run: false, ops }).await
    }

    async fn put_blob(&self, data: &[u8]) -> Result<String> {
        let c = self.client().await?;
        self.store_bytes(&**c, data).await
    }

    /// Attribute changes from the front matter of an object index. References in the body
    /// are not writable yet: a changed body is rejected.
    async fn index_ops(&self, ctx: &Ctx, w: &WriteItem, lang: &str) -> Result<Vec<Op>> {
        let Some(id) = w.node.strip_prefix("o:").and_then(|s| Uuid::parse_str(s).ok()) else {
            return Err(BaseError::invalid("aspect.not_writable", "only indexes of objects are writable"));
        };
        let new = String::from_utf8(w.data.clone()).map_err(|_| BaseError::invalid("index.not_utf8", "the index is UTF-8"))?;
        let cur = self.aspect_index(ctx, &w.aspect, &w.node, None, lang).await?;
        let (new_fm, new_body) = split_index(&new)?;
        let (cur_fm, cur_body) = split_index(&cur)?;
        if new_body.trim_end() != cur_body.trim_end() {
            return Err(BaseError::invalid("index.body_readonly", "only the front matter of an index is writable; references are changed with orders"));
        }
        for k in ["_id", "_type", "_room"] {
            if new_fm.get(k) != cur_fm.get(k) {
                return Err(BaseError::invalid("index.reserved_field", "reserved fields are not writable").with("field", k));
            }
        }
        let c = self.client().await?;
        let set = types_of(&**c, ctx.tenant, id, None).await?;
        drop(c);
        let tables = self.model.table_types(&set.keys().cloned().collect());
        let mut by_type: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
        let keys: std::collections::BTreeSet<&String> = new_fm.keys().chain(cur_fm.keys()).filter(|k| !k.starts_with('_')).collect();
        for k in keys {
            let (a, b) = (cur_fm.get(k), new_fm.get(k));
            if a == b {
                continue;
            }
            let owner = tables
                .iter()
                .find(|t| self.model.type_def(t).map(|d| d.attributes.iter().any(|x| &x.name == k)).unwrap_or(false))
                .ok_or_else(|| BaseError::invalid("attribute.unknown", "no type part of the object defines this attribute").with("attribute", k.clone()))?;
            by_type.entry(owner.clone()).or_default().insert(k.clone(), b.cloned().unwrap_or(Value::Null));
        }
        Ok(if by_type.is_empty() { vec![] } else { vec![Op::Update { id: id.to_string(), types: by_type }] })
    }

    /// A file copied into a folder with `dropType`: a new object with the bytes as content,
    /// in the room of the folder's object, linked over the folder's reference end.
    async fn drop_ops(&self, ctx: &Ctx, w: &WriteItem, i: usize) -> Result<Vec<Op>> {
        let name = w.name.clone().unwrap_or_default();
        let (parent, label) = w
            .node
            .strip_prefix("f:")
            .and_then(|f| f.split_once(':'))
            .and_then(|(id, l)| Uuid::parse_str(id).ok().map(|u| (u, l.to_string())))
            .ok_or_else(|| BaseError::invalid("aspect.not_droppable", "files can only be copied into folders"))?;
        let a = self.model.aspect(&w.aspect).ok_or_else(|| BaseError::not_found("aspect.unknown", "unknown aspect"))?;
        let c = self.client().await?;
        let set = types_of(&**c, ctx.tenant, parent, None).await?;
        let room: Option<Uuid> = c
            .query_opt("SELECT room_id FROM object_state WHERE tenant_id=$1 AND object_id=$2 AND valid_to IS NULL AND deleted_at IS NULL", &[&ctx.tenant, &parent])
            .await
            .map_err(db)?
            .map(|r| r.get(0));
        drop(c);
        let (node_type, def) = self.model.aspect_node(a, &set.keys().cloned().collect()).ok_or_else(|| BaseError::invalid("aspect.not_droppable", "no node definition"))?;
        let sn = def.sub_nodes.iter().find(|s| s.label == label && s.folder).ok_or_else(|| BaseError::invalid("aspect.not_droppable", "unknown folder"))?;
        let dt = sn.drop_type.clone().ok_or_else(|| BaseError::invalid("aspect.not_droppable", "this folder does not take new files").with("folder", label.clone()))?;
        let step = i3cm_core::paths::step(&self.model, &node_type, &sn.path)?;
        let local = format!("drop{i}");
        let sha = self.put_blob(&w.data).await?;
        // required name-like text attributes of the new object take the file name (agent
        // decision); other required attributes make the drop fail with the usual error
        let mut values = Map::new();
        for d in self.model.ancestors(&dt) {
            for at in &d.attributes {
                if at.required && matches!(at.name.as_str(), "filename" | "name" | "title") && matches!(at.vt, i3cm_core::catalog::ValueType::String) {
                    values.insert(at.name.clone(), Value::String(name.clone()));
                }
            }
        }
        let mut types = BTreeMap::new();
        types.insert(dt, values);
        let (from, to) = if step.from_holder { (parent.to_string(), local.clone()) } else { (local.clone(), parent.to_string()) };
        Ok(vec![
            Op::Create { local: Some(local.clone()), room: room.map(|r| r.to_string()), types },
            Op::SetContent { id: local, sha256: sha, media_type: media_type_of(&name).into(), size: w.data.len() as i64, filename: Some(name) },
            Op::Link { reference: step.reference, from, to, rank: None },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_round_trip() {
        let (fm, body) = split_index("---\n_id: \"x\"\nsubject: \"Hi: there\"\ndays: 3\n---\n# Hi\n").unwrap();
        assert_eq!(fm["subject"], Value::String("Hi: there".into()));
        assert_eq!(fm["days"], Value::from(3));
        assert_eq!(body, "# Hi\n");
        assert!(split_index("no front matter").is_err());
    }
}
