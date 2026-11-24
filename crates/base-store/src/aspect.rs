//! Views: evaluation of paths and business aspects (tree of groups, objects, folders and files).

use crate::context::{db, Ctx};
use crate::read::ObjectView;
use crate::schema::{col, type_table};
use crate::sql::{param, refs, Param};
use crate::Store;
use base_core::catalog::{AspectDef, NodeDef};
use base_core::fhirpath::{check_profile, eval, parse, simple_prefix, BoxFut, Expr, Item, Nav, VIEW_FUNCTIONS};
use base_core::values::{display, Col, Val};
use base_core::{BaseError, ErrorKind, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const MAX_LIST: i64 = 1000;

/// Navigator over the store for one request; sees only what the caller may read.
pub struct StoreNav<'a> {
    store: &'a Store,
    ctx: &'a Ctx,
    cache: Mutex<HashMap<Uuid, Option<Arc<ObjectView>>>>,
}

impl<'a> StoreNav<'a> {
    pub fn new(store: &'a Store, ctx: &'a Ctx) -> Self {
        StoreNav { store, ctx, cache: Mutex::new(HashMap::new()) }
    }

    async fn view(&self, id: Uuid) -> Result<Option<Arc<ObjectView>>> {
        if let Some(v) = self.cache.lock().unwrap().get(&id) {
            return Ok(v.clone());
        }
        let v = match self.store.get_object(self.ctx, id, None, "en").await {
            Ok(v) => Some(Arc::new(v)),
            Err(e) if e.kind == ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        self.cache.lock().unwrap().insert(id, v.clone());
        Ok(v)
    }

    fn attrs(v: &ObjectView) -> Map<String, Value> {
        let mut m = Map::new();
        for p in &v.parts {
            m.extend(p.attributes.clone());
        }
        m
    }
}

fn push(out: &mut Vec<Item>, v: &Value) {
    match v {
        Value::Null => {}
        Value::Array(a) => a.iter().for_each(|x| push(out, x)),
        o => out.push(Item::Val(o.clone())),
    }
}

impl Nav for StoreNav<'_> {
    fn member<'b>(&'b self, focus: &'b Item, name: &'b str) -> BoxFut<'b, Result<Vec<Item>>> {
        Box::pin(async move {
            match focus {
                Item::Obj(id) => {
                    let Some(v) = self.view(*id).await? else { return Ok(vec![]) };
                    let mut out = vec![];
                    match name {
                        "_id" => out.push(Item::Val(Value::String(id.to_string()))),
                        "_room" => out.push(Item::Obj(v.room)),
                        "_createdBy" => out.extend(v.created_by.map(Item::Obj)),
                        "_createdAt" => out.push(Item::Val(Value::String(v.created_at.to_rfc3339()))),
                        "_content" => {
                            if v.content.is_some() {
                                out.push(Item::Content(*id))
                            }
                        }
                        "_texts" => out.push(Item::Texts(*id)),
                        _ => {
                            let attrs = Self::attrs(&v);
                            if let Some(x) = attrs.get(name) {
                                push(&mut out, x);
                            } else {
                                match self.store.ends(self.ctx, *id, name, MAX_LIST, 0, "en").await {
                                    Ok(list) => out.extend(list.into_iter().map(|s| Item::Obj(s.id))),
                                    Err(e) if e.error_id == "reference.unknown_end" => {}
                                    Err(e) => return Err(e),
                                }
                            }
                        }
                    }
                    Ok(out)
                }
                Item::Texts(id) => {
                    let Some(v) = self.view(*id).await? else { return Ok(vec![]) };
                    Ok(v.texts
                        .iter()
                        .filter(|t| t.as_str() == name || t.split_once(':').map(|x| x.1) == Some(name))
                        .map(|t| Item::Text(*id, t.clone()))
                        .collect())
                }
                _ => Ok(vec![]),
            }
        })
    }

    fn is_type<'b>(&'b self, obj: Uuid, t: &'b str) -> BoxFut<'b, Result<bool>> {
        Box::pin(async move {
            let Some(v) = self.view(obj).await? else { return Ok(false) };
            Ok(v.types.iter().any(|x| self.store.model.is_a(&x.type_name, t)))
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeOut {
    pub node: String,
    /// group, object, folder or file
    pub kind: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "displayDescription", skip_serializing_if = "String::is_empty", default)]
    pub display_description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledValues {
    pub key: String,
    pub label: String,
    pub values: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabOut {
    pub key: String,
    pub label: String,
    pub columns: Vec<String>,
    pub rows: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDetail {
    pub node: NodeOut,
    pub attributes: Vec<LabeledValues>,
    pub tabs: Vec<TabOut>,
    pub files: Vec<String>,
    pub functions: Vec<Value>,
}

enum NodeRef {
    Root,
    Group(Vec<String>),
    Object(Uuid),
    Folder(Uuid, String),
}

fn esc(k: &str) -> String {
    k.replace('%', "%25").replace('/', "%2F")
}
fn unesc(k: &str) -> String {
    k.replace("%2F", "/").replace("%25", "%")
}

fn parse_node(s: &str) -> Result<NodeRef> {
    let bad = || BaseError::invalid("aspect.bad_node", "unknown node id").with("node", s);
    if s.is_empty() {
        return Ok(NodeRef::Root);
    }
    if let Some(g) = s.strip_prefix("g:") {
        return Ok(NodeRef::Group(g.split('/').map(unesc).collect()));
    }
    if let Some(o) = s.strip_prefix("o:") {
        return Ok(NodeRef::Object(Uuid::parse_str(o).map_err(|_| bad())?));
    }
    if let Some(f) = s.strip_prefix("f:") {
        let (id, label) = f.split_once(':').ok_or_else(bad)?;
        return Ok(NodeRef::Folder(Uuid::parse_str(id).map_err(|_| bad())?, label.to_string()));
    }
    Err(bad())
}

fn expr(src: &str) -> Result<Expr> {
    let e = parse(src)?;
    check_profile(&e, VIEW_FUNCTIONS, src)?;
    Ok(e)
}

impl Store {
    fn aspect_def(&self, name: &str) -> Result<&AspectDef> {
        self.model.aspect(name).ok_or_else(|| BaseError::not_found("aspect.unknown", "unknown aspect").with("aspect", name))
    }

    /// Evaluate a path on an object (profile "views").
    pub async fn eval_path(&self, ctx: &Ctx, id: Uuid, src: &str) -> Result<Vec<Value>> {
        let e = expr(src)?;
        self.get_object(ctx, id, None, "en").await?;
        let nav = StoreNav::new(self, ctx);
        Ok(eval(&nav, &e, &[Item::Obj(id)]).await?.iter().map(|i| i.to_json()).collect())
    }

    pub fn aspect_list(&self, lang: &str) -> Vec<Value> {
        self.model
            .aspects()
            .map(|a| serde_json::json!({"name": a.name, "displayName": self.i18n.label(&a.name, lang, "_name").replace("_name", &a.name), "entry": a.entry.type_name}))
            .collect()
    }

    async fn object_out(&self, nav: &StoreNav<'_>, a: &AspectDef, id: Uuid, lang: &str) -> Result<Option<NodeOut>> {
        let Some(v) = nav.view(id).await? else { return Ok(None) };
        let set: BTreeSet<String> = v.types.iter().map(|t| t.type_name.clone()).collect();
        let t = self.model.aspect_node(a, &set).map(|(t, _)| t).or_else(|| v.types.iter().find(|x| !x.implied).map(|x| x.type_name.clone())).unwrap_or_default();
        let mut attrs = StoreNav::attrs(&v);
        for anc in self.model.ancestors(&t) {
            for x in &anc.attributes {
                attrs.entry(x.name.clone()).or_insert_with(|| Value::String(String::new()));
            }
        }
        let (n, d) = self.i18n.names_in(Some(&a.name), lang, &t, &attrs);
        Ok(Some(NodeOut { node: format!("o:{id}"), kind: "object".into(), display_name: n, display_description: d, count: None, type_name: Some(t), id: Some(id) }))
    }

    /// SQL expression for a simple group expression on the entry type's own table.
    fn group_sql(&self, a: &AspectDef, e: &Expr) -> Option<String> {
        let (attr, n) = simple_prefix(e)?;
        let def = self.model.type_def(&a.entry.type_name)?;
        let ad = def.attributes.iter().find(|x| x.name == attr && !x.list)?;
        let c = format!("x.{}", col(&attr));
        let txt = match Col::of(&ad.vt) {
            Col::Ts => format!("to_char({c} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"+00:00\"')"),
            Col::Json => return None,
            Col::Text => c,
            _ => format!("{c}::text"),
        };
        Some(match n {
            Some(n) => format!("substring({txt}, 1, {n})"),
            None => txt,
        })
    }

    /// Keys of grouping level `level` below `prefix` (with counts), or the objects at full depth.
    async fn group_level(&self, ctx: &Ctx, nav: &StoreNav<'_>, a: &AspectDef, prefix: &[String], lang: &str) -> Result<Vec<NodeOut>> {
        let exprs: Vec<Expr> = a.entry.group_by.iter().map(|g| expr(g)).collect::<Result<_>>()?;
        let level = prefix.len();
        if level > exprs.len() {
            return Err(BaseError::invalid("aspect.bad_node", "too many group levels"));
        }
        let sqls: Option<Vec<String>> = exprs.iter().map(|e| self.group_sql(a, e)).collect();
        let def = self.model.require_type(&a.entry.type_name)?;
        let rooms = self.readable_rooms_for(ctx, &a.entry.type_name);
        match sqls {
            Some(sqls) => {
                let mut params: Vec<Param> = vec![Box::new(ctx.tenant), Box::new(rooms)];
                let mut conds = String::new();
                for (i, k) in prefix.iter().enumerate() {
                    params.push(param(&Val::Text(k.clone()), Col::Text, false));
                    conds.push_str(&format!(" AND {} = ${}", sqls[i], params.len()));
                }
                let from = format!(
                    "FROM {} x JOIN object_state s ON s.tenant_id=x.tenant_id AND s.object_id=x.object_id AND s.valid_to IS NULL AND s.deleted_at IS NULL AND s.room_id = ANY($2) \
                     WHERE x.tenant_id=$1 AND x.valid_to IS NULL AND x.deleted_at IS NULL{conds}",
                    type_table(def)
                );
                let c = self.client().await?;
                if level < exprs.len() {
                    let sql = format!("SELECT k, count(*) FROM (SELECT {} AS k {from}) q WHERE k IS NOT NULL GROUP BY k ORDER BY k", sqls[level]);
                    let rows = c.query(&sql, &refs(&params)).await.map_err(db)?;
                    Ok(rows
                        .iter()
                        .map(|r| {
                            let k: String = r.get(0);
                            let path: Vec<String> = prefix.iter().cloned().chain([k.clone()]).map(|x| esc(&x)).collect();
                            NodeOut { node: format!("g:{}", path.join("/")), kind: "group".into(), display_name: k, display_description: String::new(), count: Some(r.get(1)), type_name: None, id: None }
                        })
                        .collect())
                } else {
                    let sql = format!("SELECT x.object_id {from} ORDER BY x.object_id LIMIT {MAX_LIST}");
                    let ids: Vec<Uuid> = c.query(&sql, &refs(&params)).await.map_err(db)?.iter().map(|r| r.get(0)).collect();
                    drop(c);
                    let mut out = vec![];
                    for id in ids {
                        out.extend(self.object_out(nav, a, id, lang).await?);
                    }
                    out.sort_by(|x, y| x.display_name.cmp(&y.display_name));
                    Ok(out)
                }
            }
            None => {
                // in-memory: evaluate the group expressions per object
                let q = crate::read::ListQuery { limit: Some(MAX_LIST), ..Default::default() };
                let all = self.list(ctx, &a.entry.type_name, &q, lang).await?;
                let mut groups: std::collections::BTreeMap<String, i64> = Default::default();
                let mut hits = vec![];
                'obj: for s in all {
                    let focus = [Item::Obj(s.id)];
                    for (i, k) in prefix.iter().enumerate() {
                        let v = eval(nav, &exprs[i], &focus).await?;
                        if v.first().map(|x| display(&x.to_json())) != Some(k.clone()) {
                            continue 'obj;
                        }
                    }
                    if level < exprs.len() {
                        if let Some(k) = eval(nav, &exprs[level], &focus).await?.first() {
                            *groups.entry(display(&k.to_json())).or_default() += 1;
                        }
                    } else {
                        hits.push(s.id);
                    }
                }
                if level < exprs.len() {
                    Ok(groups
                        .into_iter()
                        .map(|(k, n)| {
                            let path: Vec<String> = prefix.iter().cloned().chain([k.clone()]).map(|x| esc(&x)).collect();
                            NodeOut { node: format!("g:{}", path.join("/")), kind: "group".into(), display_name: k, display_description: String::new(), count: Some(n), type_name: None, id: None }
                        })
                        .collect())
                } else {
                    let mut out = vec![];
                    for id in hits {
                        out.extend(self.object_out(nav, a, id, lang).await?);
                    }
                    Ok(out)
                }
            }
        }
    }

    fn readable_rooms_for(&self, ctx: &Ctx, t: &str) -> Vec<Uuid> {
        ctx.rooms
            .rooms()
            .filter(|r| ctx.rooms.path(**r).map(|p| ctx.subject.may(base_core::Art::Read, t, &p)).unwrap_or(false))
            .copied()
            .collect()
    }

    async fn node_def<'b>(&self, nav: &StoreNav<'_>, a: &'b AspectDef, id: Uuid) -> Result<(Arc<ObjectView>, String, Option<&'b NodeDef>)> {
        let v = nav.view(id).await?.ok_or_else(|| BaseError::not_found("object.not_found", "object not found"))?;
        let set: BTreeSet<String> = v.types.iter().map(|t| t.type_name.clone()).collect();
        match self.model.aspect_node(a, &set) {
            Some((t, n)) => Ok((v, t, Some(n))),
            None => Ok((v.clone(), v.types.first().map(|t| t.type_name.clone()).unwrap_or_default(), None)),
        }
    }

    fn file_name(pattern: &str, attrs: &Map<String, Value>) -> String {
        let mut out = pattern.to_string();
        for (k, v) in attrs {
            out = out.replace(&format!("{{{k}}}"), &display(v));
        }
        out.replace('/', "_")
    }

    async fn objects_of(&self, nav: &StoreNav<'_>, id: Uuid, path: &str) -> Result<Vec<Uuid>> {
        let e = expr(path)?;
        Ok(eval(nav, &e, &[Item::Obj(id)]).await?.into_iter().filter_map(|i| if let Item::Obj(x) = i { Some(x) } else { None }).collect())
    }

    async fn files_of(&self, nav: &StoreNav<'_>, id: Uuid, n: &NodeDef, attrs: &Map<String, Value>) -> Result<Vec<(String, Item)>> {
        let mut out = vec![];
        for f in &n.files {
            let e = expr(&f.path)?;
            if let Some(it) = eval(nav, &e, &[Item::Obj(id)]).await?.into_iter().next() {
                out.push((Self::file_name(&f.name, attrs), it));
            }
        }
        Ok(out)
    }

    pub async fn aspect_children(&self, ctx: &Ctx, aspect: &str, node: &str, lang: &str) -> Result<Vec<NodeOut>> {
        let a = self.aspect_def(aspect)?;
        let nav = StoreNav::new(self, ctx);
        match parse_node(node)? {
            NodeRef::Root => self.group_level(ctx, &nav, a, &[], lang).await,
            NodeRef::Group(keys) => self.group_level(ctx, &nav, a, &keys, lang).await,
            NodeRef::Object(id) => {
                let (v, _, n) = self.node_def(&nav, a, id).await?;
                let Some(n) = n else { return Ok(vec![]) };
                let attrs = StoreNav::attrs(&v);
                let mut out = vec![];
                for (name, _) in self.files_of(&nav, id, n, &attrs).await? {
                    out.push(NodeOut { node: format!("o:{id}"), kind: "file".into(), display_name: name, display_description: String::new(), count: None, type_name: None, id: Some(id) });
                }
                for s in &n.sub_nodes {
                    let ids = self.objects_of(&nav, id, &s.path).await?;
                    if s.folder {
                        out.push(NodeOut {
                            node: format!("f:{id}:{}", s.label),
                            kind: "folder".into(),
                            display_name: self.i18n.label(&a.name, lang, &s.label),
                            display_description: String::new(),
                            count: Some(ids.len() as i64),
                            type_name: None,
                            id: None,
                        });
                    } else {
                        for x in ids {
                            out.extend(self.object_out(&nav, a, x, lang).await?);
                        }
                    }
                }
                Ok(out)
            }
            NodeRef::Folder(id, label) => {
                let (_, _, n) = self.node_def(&nav, a, id).await?;
                let s = n
                    .and_then(|n| n.sub_nodes.iter().find(|s| s.label == label))
                    .ok_or_else(|| BaseError::invalid("aspect.bad_node", "unknown folder").with("folder", label.clone()))?;
                let mut out = vec![];
                for x in self.objects_of(&nav, id, &s.path).await? {
                    out.extend(self.object_out(&nav, a, x, lang).await?);
                }
                Ok(out)
            }
        }
    }

    pub async fn aspect_node(&self, ctx: &Ctx, aspect: &str, node: &str, lang: &str) -> Result<NodeDetail> {
        let a = self.aspect_def(aspect)?;
        let nav = StoreNav::new(self, ctx);
        let id = match parse_node(node)? {
            NodeRef::Object(id) => id,
            NodeRef::Group(keys) => {
                let k = keys.last().cloned().unwrap_or_default();
                return Ok(NodeDetail {
                    node: NodeOut { node: node.to_string(), kind: "group".into(), display_name: k, display_description: String::new(), count: None, type_name: None, id: None },
                    attributes: vec![],
                    tabs: vec![],
                    files: vec![],
                    functions: vec![],
                });
            }
            _ => return Err(BaseError::invalid("aspect.bad_node", "details exist for objects and groups")),
        };
        let out = self.object_out(&nav, a, id, lang).await?.ok_or_else(|| BaseError::not_found("object.not_found", "object not found"))?;
        let (v, _, n) = self.node_def(&nav, a, id).await?;
        let n = n.cloned().unwrap_or_default();
        let focus = [Item::Obj(id)];
        let mut attributes = vec![];
        for x in &n.attributes {
            let vals = eval(&nav, &expr(&x.path)?, &focus).await?;
            attributes.push(LabeledValues { key: x.label.clone(), label: self.i18n.label(&a.name, lang, &x.label), values: vals.iter().map(|i| i.to_json()).collect() });
        }
        let mut tabs = vec![];
        for t in &n.tabs {
            let mut rows = vec![];
            for row in self.objects_of(&nav, id, &t.path).await? {
                let mut cells = vec![];
                for c in &t.columns {
                    let vals = eval(&nav, &expr(&c.path)?, &[Item::Obj(row)]).await?;
                    cells.push(Value::Array(vals.iter().map(|i| i.to_json()).collect()));
                }
                rows.push(serde_json::json!({"node": format!("o:{row}"), "cells": cells}));
            }
            tabs.push(TabOut {
                key: t.label.clone(),
                label: self.i18n.label(&a.name, lang, &t.label),
                columns: t.columns.iter().map(|c| self.i18n.label(&a.name, lang, &c.label)).collect(),
                rows,
            });
        }
        let attrs = StoreNav::attrs(&v);
        let files = self.files_of(&nav, id, &n, &attrs).await?.into_iter().map(|(n, _)| n).collect();
        let functions = n
            .functions
            .iter()
            .map(|f| serde_json::json!({"name": f.name, "label": self.i18n.label(&a.name, lang, &f.name), "url": f.url}))
            .collect();
        Ok(NodeDetail { node: out, attributes, tabs, files, functions })
    }

    /// Content of a file of an object node: (bytes, media type).
    pub async fn aspect_file(&self, ctx: &Ctx, aspect: &str, node: &str, name: &str) -> Result<(Vec<u8>, String)> {
        let a = self.aspect_def(aspect)?;
        let nav = StoreNav::new(self, ctx);
        let NodeRef::Object(id) = parse_node(node)? else {
            return Err(BaseError::invalid("aspect.bad_node", "files belong to object nodes"));
        };
        let (v, _, n) = self.node_def(&nav, a, id).await?;
        let n = n.ok_or_else(|| BaseError::not_found("aspect.no_file", "no such file"))?;
        let attrs = StoreNav::attrs(&v);
        let files = self.files_of(&nav, id, n, &attrs).await?;
        let (_, it) = files.into_iter().find(|(f, _)| f == name).ok_or_else(|| BaseError::not_found("aspect.no_file", "no such file").with("name", name))?;
        match it {
            Item::Content(x) => {
                let (data, media, _) = self.get_content(ctx, x).await?;
                Ok((data, media))
            }
            Item::Text(x, tt) => {
                let (content, media) = self.get_text(ctx, x, &tt).await?;
                Ok((content.into_bytes(), format!("{media}; charset=utf-8")))
            }
            _ => Err(BaseError::not_found("aspect.no_file", "no such file")),
        }
    }
}
