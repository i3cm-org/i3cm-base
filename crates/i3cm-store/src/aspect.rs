//! Views: evaluation of paths and business aspects (tree of groups, objects, folders and files).

use crate::context::{db, Ctx};
use crate::read::ObjectView;
use crate::schema::{col, type_table};
use crate::sql::{param, refs, Param};
use crate::Store;
use i3cm_core::catalog::{AspectDef, NodeDef};
use i3cm_core::fhirpath::{check_profile, eval, group_key, parse, BoxFut, Expr, GroupKey, Item, Nav, VIEW_FUNCTIONS};
use i3cm_core::values::{display, Col, Val};
use chrono::{DateTime, Utc};
use i3cm_core::{BaseError, ErrorKind, Result};
use i3cm_tools::{fs_name, ino, project, Named, Slot};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const MAX_LIST: i64 = 1000;

/// Built-in view: every object by its id (the stable address; the root lists nothing).
pub const VIEW_ID: &str = "_id";
/// Built-in view: objects by type (type, then objects; sub-nodes are files and named ends).
pub const VIEW_TYPE: &str = "_type";

fn is_builtin(a: &str) -> bool {
    a == VIEW_ID || a == VIEW_TYPE
}

/// Paging of a children listing (used by built-in views; module aspects group instead).
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub offset: i64,
    pub limit: Option<i64>,
    /// Keyset paging: continue after this node id (the last one of the previous page).
    pub after: Option<String>,
}

/// Last change of an object: time and account.
type Change = (DateTime<Utc>, Option<Uuid>);

/// Navigator over the store for one request; sees only what the caller may read.
pub struct StoreNav<'a> {
    store: &'a Store,
    ctx: &'a Ctx,
    cache: Mutex<HashMap<Uuid, Option<Arc<ObjectView>>>>,
    changed: Mutex<HashMap<Uuid, Option<Change>>>,
}

impl<'a> StoreNav<'a> {
    pub fn new(store: &'a Store, ctx: &'a Ctx) -> Self {
        StoreNav { store, ctx, cache: Mutex::new(HashMap::new()), changed: Mutex::new(HashMap::new()) }
    }

    pub(crate) async fn view(&self, id: Uuid) -> Result<Option<Arc<ObjectView>>> {
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

    /// Last change of any readable part: time and account (from the history).
    async fn changed(&self, id: Uuid) -> Result<Option<(DateTime<Utc>, Option<Uuid>)>> {
        if let Some(c) = self.changed.lock().unwrap().get(&id) {
            return Ok(*c);
        }
        let last = self.store.versions(self.ctx, id).await?.pop();
        let c = last.map(|v| {
            let at = v["at"].as_str().and_then(|x| DateTime::parse_from_rfc3339(x).ok()).map(|x| x.with_timezone(&Utc)).unwrap_or_default();
            let by = v["account"].as_str().and_then(|x| Uuid::parse_str(x).ok());
            (at, by)
        });
        self.changed.lock().unwrap().insert(id, c);
        Ok(c)
    }

    fn attrs(v: &ObjectView) -> Map<String, Value> {
        let mut m = Map::new();
        for p in &v.parts {
            m.extend(p.attributes.clone());
        }
        // name patterns may use the stored upload file name: `{_content.filename}`
        if let Some(f) = v.content.as_ref().and_then(|c| c.get("filename")).filter(|f| !f.is_null()) {
            m.insert("_content.filename".into(), f.clone());
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
                        "_changedBy" => out.extend(self.changed(*id).await?.and_then(|(_, by)| by).map(Item::Obj)),
                        "_changedAt" => out.extend(self.changed(*id).await?.map(|(at, _)| Item::Val(Value::String(at.to_rfc3339())))),
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
                // `_content.filename`, `_content.mediaType`, `_content.size`
                Item::Content(id) => {
                    let Some(v) = self.view(*id).await? else { return Ok(vec![]) };
                    let mut out = vec![];
                    if let Some(x) = v.content.as_ref().and_then(|c| c.get(name)) {
                        push(&mut out, x);
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeOut {
    /// Opaque node id; clients pass it back unchanged. For a file: the id of its node.
    pub node: String,
    /// A file (bytes) instead of a node (index and children).
    pub file: bool,
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
    /// Name in paths: the display name without `/` (FUSE, tools).
    #[serde(default)]
    pub name: String,
    /// Name in paths by the fallback rule of the aspect (Fluent attribute `.fallback` of the name),
    /// used instead of `name` when names in a directory collide.
    #[serde(rename = "fallbackName", skip_serializing_if = "Option::is_none")]
    pub fallback_name: Option<String>,
    /// Display name by the fallback rule (not serialized; source of `fallback_name`).
    #[serde(skip)]
    pub fallback_display: Option<String>,
    /// Bytes of the file; for a node: of its index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(rename = "changedAt", skip_serializing_if = "Option::is_none")]
    pub changed_at: Option<DateTime<Utc>>,
    /// Inode of the file; for a node: of its index. Equal content gives an equal number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ino: Option<u64>,
    /// Where the bytes of a file come from (content or a text).
    #[serde(skip)]
    pub src: Option<Item>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledValues {
    pub key: String,
    pub label: String,
    pub values: Vec<Value>,
    /// Display texts of enum values (`<ns>-<Type>-<attr>-<value>`), parallel to `values`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub display: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabOut {
    pub key: String,
    pub label: String,
    pub columns: Vec<String>,
    pub rows: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabRow {
    pub node: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    pub cells: Vec<Value>,
}

/// A page of one tab; `total` counts all rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabPage {
    pub key: String,
    pub label: String,
    pub columns: Vec<String>,
    pub total: usize,
    pub rows: Vec<TabRow>,
}

/// A cell as text: values joined with ", " (export and sorting).
pub fn cell_text(v: &Value) -> String {
    fn one(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            Value::Object(o) if o.contains_key("amount") && o.contains_key("currency") => format!("{} {}", one(&o["amount"]), one(&o["currency"])),
            Value::Object(o) if o.contains_key("content") => one(&o["content"]),
            other => other.to_string(),
        }
    }
    match v {
        Value::Array(xs) => xs.iter().map(one).collect::<Vec<_>>().join(", "),
        x => one(x),
    }
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

    /// Display name of an aspect (built-in or module).
    fn aspect_display(&self, a: &str, lang: &str) -> String {
        if is_builtin(a) { self.i18n.label("base", lang, a) } else { self.i18n.label(a, lang, "_name").replace("_name", a) }
    }

    /// Aspects; `size` and `ino` describe the index of the entry node (FUSE: `<name>.md`).
    pub fn aspect_list(&self, lang: &str) -> Vec<Value> {
        let names = [VIEW_ID.to_string(), VIEW_TYPE.to_string()].into_iter().chain(self.model.aspects().map(|a| a.name.clone()));
        names
            .map(|a| {
                let d = self.aspect_display(&a, lang);
                let size = Self::group_index(&d).len();
                let mut v = serde_json::json!({"name": a, "displayName": d, "size": size, "ino": ino(&["i", &a, ""])});
                match self.model.aspect(&a) {
                    Some(def) => v["entry"] = Value::String(def.entry.type_name.clone()),
                    None => v["builtin"] = Value::Bool(true),
                }
                v
            })
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
        let fallback_display = self.i18n.fallback_in(Some(&a.name), lang, &t, &attrs);
        Ok(Some(NodeOut { node: format!("o:{id}"), file: false, display_name: n, display_description: d, count: None, type_name: Some(t), id: Some(id), fallback_display, ..Default::default() }))
    }

    /// SQL text of a grouping key applied to the value expression `v`.
    /// SQL element type of a list column, for an empty-list placeholder.
    fn elem_sql(c: Col) -> &'static str {
        match c {
            Col::Text => "text",
            Col::Int => "bigint",
            Col::Num => "numeric",
            Col::Bool => "boolean",
            Col::Date => "date",
            Col::Ts => "timestamptz",
            Col::Time => "time",
            _ => "text",
        }
    }

    /// Display texts of enum values for a plain attribute path: `<ns>-<Type>-<attr>-<value>`,
    /// falling back to the value itself; empty if the path is not an enum attribute.
    fn enum_display(&self, types: &[String], path: &str, values: &[Value], lang: &str) -> Vec<String> {
        if !path.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return vec![];
        }
        for t in types {
            if let Some(owner) = self.model.path_defining_type(t, path) {
                let Some(ad) = owner.attributes.iter().find(|x| x.name == path) else { continue };
                if !matches!(ad.vt, i3cm_core::catalog::ValueType::Enum { .. }) {
                    return vec![];
                }
                return values.iter().map(|v| {
                    let raw = v.as_str().unwrap_or_default();
                    self.i18n.enum_label(lang, &owner.name, path, raw).unwrap_or_else(|| raw.to_string())
                }).collect();
            }
        }
        vec![]
    }

    /// Label of the group of objects without a value: `<aspect>--label-_none`, default "—".
    fn none_label(&self, aspect: &str, lang: &str) -> String {
        let l = self.i18n.label(aspect, lang, "_none");
        if l == "_none" { "—".to_string() } else { l }
    }

    fn key_sql(v: &str, c: Col, k: &GroupKey) -> String {
        let mut t = match c {
            Col::Ts => format!("to_char({v} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"+00:00\"')"),
            Col::Text => v.to_string(),
            _ => format!("{v}::text"),
        };
        if let Some(n) = k.len {
            t = format!("substring({t}, 1, {n})");
        }
        if let Some(f) = k.case {
            t = format!("{f}({t})");
        }
        // objects without a value form one group with the empty key
        format!("coalesce({t}, '')")
    }

    /// The chain of nodes from the top of a view down to `node` (for opening a tree at a node).
    /// An object reached only through a folder of another object has no known chain above it.
    pub async fn aspect_path(&self, ctx: &Ctx, aspect: &str, node: &str, lang: &str) -> Result<Vec<NodeOut>> {
        let nav = StoreNav::new(self, ctx);
        let grp = |keys: &[String]| -> Vec<NodeOut> {
            (1..=keys.len()).map(|i| Self::group_out(format!("g:{}", keys[..i].iter().map(|k| esc(k)).collect::<Vec<_>>().join("/")), keys[i - 1].clone(), None)).collect()
        };
        let obj_chain = |id: Uuid| async move {
            let mut out: Vec<NodeOut> = vec![];
            if aspect == VIEW_TYPE {
                if let Some(o) = self.plain_object_out(&nav, id, lang).await? {
                    if let Some(t) = o.type_name.clone() {
                        out.push(Self::group_out(format!("g:{}", esc(&t)), t, None));
                    }
                    out.push(o);
                }
                return Ok::<_, BaseError>(out);
            }
            if is_builtin(aspect) {
                out.extend(self.plain_object_out(&nav, id, lang).await?);
                return Ok(out);
            }
            let a = self.aspect_def(aspect)?;
            let set = nav.view(id).await?.map(|v| v.types.iter().map(|t| t.type_name.clone()).collect::<BTreeSet<String>>()).unwrap_or_default();
            if self.model.set_is_a(set.iter(), &a.entry.type_name) && !a.entry.group_by.is_empty() {
                let def = self.model.require_type(&a.entry.type_name)?;
                let mut keys = vec![];
                let c = self.client().await?;
                for g in &a.entry.group_by {
                    let Some(k) = group_key(&expr(g)?) else { break };
                    let Some(ad) = def.attributes.iter().find(|x| x.name == k.attr) else { break };
                    let ct = Col::of(&ad.vt);
                    let (from, v) = if ad.list { (format!("CROSS JOIN LATERAL unnest(x.{}) u", col(&k.attr)), "u".to_string()) } else { (String::new(), format!("x.{}", col(&k.attr))) };
                    let sql = format!("SELECT {} FROM {} x {from} WHERE x.tenant_id=$1 AND x.object_id=$2 AND x.valid_to IS NULL ORDER BY 1 LIMIT 1", Self::key_sql(&v, ct, &k), type_table(def));
                    match c.query_opt(&sql, &[&ctx.tenant, &id]).await.map_err(db)?.and_then(|r| r.get::<_, Option<String>>(0)) {
                        Some(key) => keys.push(key),
                        None => break,
                    }
                }
                out.extend(grp(&keys));
            }
            out.extend(self.object_out(&nav, a, id, lang).await?);
            Ok(out)
        };
        Ok(match parse_node(node)? {
            NodeRef::Root => vec![],
            NodeRef::Group(keys) => grp(&keys),
            NodeRef::Object(id) => obj_chain(id).await?,
            NodeRef::Folder(id, label) => {
                let mut v = obj_chain(id).await?;
                v.push(NodeOut { node: node.to_string(), file: false, display_name: label, ..Default::default() });
                v
            }
        })
    }

    /// Group keys of level `prefix.len()` below `prefix` (with counts), or the objects at full
    /// depth. Always in SQL; a list attribute places an object under each of its values.
    async fn group_level(&self, ctx: &Ctx, nav: &StoreNav<'_>, a: &AspectDef, prefix: &[String], lang: &str, page: Page) -> Result<Vec<NodeOut>> {
        let def = self.model.require_type(&a.entry.type_name)?;
        let mut cols = vec![];
        for g in &a.entry.group_by {
            let bad = || BaseError::invalid("aspect.bad_group", "grouping rule cannot run in SQL").with("rule", g.clone());
            let k = group_key(&expr(g)?).ok_or_else(bad)?;
            let owner = self.model.path_defining_type(&a.entry.type_name, &k.attr).ok_or_else(bad)?;
            let ad = owner.attributes.iter().find(|x| x.name == k.attr).ok_or_else(bad)?;
            let c = Col::of(&ad.vt);
            if c == Col::Json {
                return Err(bad());
            }
            // own column, or the column of a supertype or required host type (subquery)
            cols.push((self.attr_col(&a.entry.type_name, &owner.name, &k.attr), c, ad.list, k));
        }
        let level = prefix.len();
        if level > cols.len() {
            return Err(BaseError::invalid("aspect.bad_node", "too many group levels"));
        }
        let access = self.access(ctx, i3cm_core::Art::Read, &a.entry.type_name)?;
        if access.is_empty() {
            return Ok(vec![]);
        }
        let limit = page.limit.unwrap_or(MAX_LIST).clamp(1, MAX_LIST);
        let mut params: Vec<Param> = vec![Box::new(ctx.tenant)];
        let mut conds = String::new();
        for (i, k) in prefix.iter().enumerate() {
            let (c, ct, list, gk) = &cols[i];
            params.push(param(&Val::Text(k.clone()), Col::Text, false));
            let n = params.len();
            if *list {
                conds.push_str(&format!(" AND EXISTS (SELECT 1 FROM unnest(coalesce(nullif({c}, '{{}}'), ARRAY[NULL]::{}[])) u WHERE {} = ${n})", Self::elem_sql(*ct), Self::key_sql("u", *ct, gk)));
            } else {
                conds.push_str(&format!(" AND {} = ${n}", Self::key_sql(c, *ct, gk)));
            }
        }
        if let Some(w) = &a.entry.filter {
            let f = i3cm_core::paths::view_filter(&self.model, &a.entry.type_name, w)?;
            conds.push_str(&format!(" AND {}", self.filter_sql(&f, &a.entry.type_name, ctx, &mut params)?));
        }
        let from = format!(
            "FROM {} x JOIN object_state s ON s.tenant_id=x.tenant_id AND s.object_id=x.object_id AND s.valid_to IS NULL AND s.deleted_at IS NULL",
            type_table(def)
        );
        let after = page.after.as_deref().map(parse_node).transpose()?;
        let offset = if after.is_some() { 0 } else { page.offset.max(0) };
        let c = self.client().await?;
        if level < cols.len() {
            // group keys: an aggregate over all readable objects, rights as one clause
            let rights = self.rights_clause(&access, ctx, &mut params);
            let wh = format!("WHERE x.tenant_id=$1 AND x.valid_to IS NULL AND x.deleted_at IS NULL{conds} AND {rights}");
            let mut outer = String::from("k IS NOT NULL");
            if let Some(NodeRef::Group(keys)) = &after {
                params.push(Box::new(keys.last().cloned().unwrap_or_default()));
                outer.push_str(&format!(" AND k > ${}", params.len()));
            }
            let (cn, ct, list, gk) = &cols[level];
            let inner = if *list {
                format!("SELECT DISTINCT x.object_id, {} AS k {from} CROSS JOIN LATERAL unnest(coalesce(nullif({cn}, '{{}}'), ARRAY[NULL]::{}[])) u {wh}", Self::key_sql("u", *ct, gk), Self::elem_sql(*ct))
            } else {
                format!("SELECT {} AS k {from} {wh}", Self::key_sql(cn, *ct, gk))
            };
            let sql = format!("SELECT k, count(*) FROM ({inner}) q WHERE {outer} GROUP BY k ORDER BY k LIMIT {limit} OFFSET {offset}");
            let rows = c.query(&sql, &refs(&params)).await.map_err(db)?;
            Ok(rows
                .iter()
                .map(|r| {
                    let k: String = r.get(0);
                    let path: Vec<String> = prefix.iter().cloned().chain([k.clone()]).map(|x| esc(&x)).collect();
                    let shown = if k.is_empty() { self.none_label(&a.name, lang) } else { k };
                    NodeOut { node: format!("g:{}", path.join("/")), file: false, display_name: shown, display_description: String::new(), count: Some(r.get(1)), type_name: None, id: None, ..Default::default() }
                })
                .collect())
        } else {
            // objects: one branch per part of the access, merged by UNION (no OR, REP-018)
            let mut after_cond = String::new();
            if let Some(NodeRef::Object(id)) = &after {
                params.push(Box::new(*id));
                after_cond = format!(" AND x.object_id > ${}", params.len());
            }
            let branches = self.rights_branches(&access, ctx, &mut params);
            let wh = format!("WHERE x.tenant_id=$1 AND x.valid_to IS NULL AND x.deleted_at IS NULL{conds}{after_cond}");
            let parts: Vec<String> = branches.iter().map(|b| format!("(SELECT x.object_id {from} {wh} AND {b} ORDER BY x.object_id LIMIT {})", limit + offset)).collect();
            let sql = format!("SELECT object_id FROM ({}) u ORDER BY object_id LIMIT {limit} OFFSET {offset}", parts.join(" UNION "));
            let ids: Vec<Uuid> = c.query(&sql, &refs(&params)).await.map_err(db)?.iter().map(|r| r.get(0)).collect();
            drop(c);
            let mut out = vec![];
            for id in ids {
                out.extend(self.object_out(nav, a, id, lang).await?);
            }
            Ok(out)
        }
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
        self.aspect_children_paged(ctx, aspect, node, lang, Page::default()).await
    }

    /// Children of a node, one page; every entry carries name, size, change time and inode.
    pub async fn aspect_children_paged(&self, ctx: &Ctx, aspect: &str, node: &str, lang: &str, page: Page) -> Result<Vec<NodeOut>> {
        let nav = StoreNav::new(self, ctx);
        let (offset, limit, after) = (page.offset, page.limit, page.after.clone());
        let (mut out, paged) = if is_builtin(aspect) { self.builtin_children(ctx, &nav, aspect, node, lang, page).await? } else { self.children_raw(ctx, &nav, aspect, node, lang, page).await? };
        if !paged {
            // short listings: keyset by position of the given node, else offset
            let skip = match after.as_deref().and_then(|a| out.iter().position(|n| n.node == a)) {
                Some(i) => i + 1,
                None => offset.max(0) as usize,
            };
            out = out.into_iter().skip(skip).take(limit.unwrap_or(MAX_LIST).clamp(1, MAX_LIST) as usize).collect();
        }
        for n in out.iter_mut() {
            n.name = fs_name(&n.display_name);
            n.fallback_name = n.fallback_display.as_deref().map(fs_name);
        }
        self.decorate(ctx, &nav, aspect, lang, &mut out).await?;
        Ok(out)
    }

    /// Children of a module aspect node, and whether `page` was applied already.
    async fn children_raw(&self, ctx: &Ctx, nav: &StoreNav<'_>, aspect: &str, node: &str, lang: &str, page: Page) -> Result<(Vec<NodeOut>, bool)> {
        let a = self.aspect_def(aspect)?;
        Ok(match parse_node(node)? {
            NodeRef::Root => (self.group_level(ctx, nav, a, &[], lang, page).await?, true),
            NodeRef::Group(keys) => (self.group_level(ctx, nav, a, &keys, lang, page).await?, true),
            NodeRef::Object(id) => {
                let (v, _, n) = self.node_def(nav, a, id).await?;
                let Some(n) = n else { return Ok((vec![], false)) };
                let attrs = StoreNav::attrs(&v);
                let mut files = vec![];
                for (name, it) in self.files_of(nav, id, n, &attrs).await? {
                    files.push(NodeOut { node: format!("o:{id}"), file: true, display_name: name, display_description: String::new(), count: None, type_name: None, id: Some(id), src: Some(it), ..Default::default() });
                }
                let mut out = vec![];
                for s in &n.sub_nodes {
                    let ids = self.objects_of(nav, id, &s.path).await?;
                    if s.folder {
                        out.push(NodeOut {
                            node: format!("f:{id}:{}", s.label),
                            file: false,
                            display_name: self.i18n.label(&a.name, lang, &s.label),
                            display_description: String::new(),
                            count: Some(ids.len() as i64),
                            type_name: None,
                            id: None, ..Default::default() });
                    } else {
                        for x in ids {
                            out.extend(self.object_out(nav, a, x, lang).await?);
                        }
                    }
                }
                // entries in the order of the definition (files block and sub-node block)
                if n.files_first {
                    files.extend(out);
                    (files, false)
                } else {
                    out.extend(files);
                    (out, false)
                }
            }
            NodeRef::Folder(id, label) => {
                let (_, _, n) = self.node_def(nav, a, id).await?;
                let s = n
                    .and_then(|n| n.sub_nodes.iter().find(|s| s.label == label))
                    .ok_or_else(|| BaseError::invalid("aspect.bad_node", "unknown folder").with("folder", label.clone()))?;
                let mut out = vec![];
                for x in self.objects_of(nav, id, &s.path).await? {
                    out.extend(self.object_out(nav, a, x, lang).await?);
                }
                (out, false)
            }
        })
    }

    pub async fn aspect_node(&self, ctx: &Ctx, aspect: &str, node: &str, lang: &str) -> Result<NodeDetail> {
        if is_builtin(aspect) {
            return self.builtin_node(ctx, aspect, node, lang).await;
        }
        let a = self.aspect_def(aspect)?;
        let nav = StoreNav::new(self, ctx);
        let id = match parse_node(node)? {
            NodeRef::Object(id) => id,
            NodeRef::Group(keys) => {
                let k = keys.last().cloned().unwrap_or_default();
                return Ok(NodeDetail {
                    node: NodeOut { node: node.to_string(), file: false, display_name: k, display_description: String::new(), count: None, type_name: None, id: None, ..Default::default() },
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
            let values: Vec<Value> = vals.iter().map(|i| i.to_json()).collect();
            let display = self.enum_display(&v.types.iter().map(|t| t.type_name.clone()).collect::<Vec<_>>(), &x.path, &values, lang);
            attributes.push(LabeledValues { key: x.label.clone(), label: self.i18n.label(&a.name, lang, &x.label), values, display });
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

    /// One tab of an object node, sorted and paged by the server. `sort` is a column index
    /// (values compared as text), `limit: None` returns all rows (export). Every row carries
    /// the object's display name and type, so a tab without columns can be shown as a list.
    pub async fn aspect_tab(&self, ctx: &Ctx, aspect: &str, node: &str, tab: &str, sort: Option<(usize, bool)>, page: Page, lang: &str) -> Result<TabPage> {
        let a = self.aspect_def(aspect)?;
        let nav = StoreNav::new(self, ctx);
        let NodeRef::Object(id) = parse_node(node)? else {
            return Err(BaseError::not_found("aspect.no_tab", "no such tab").with("tab", tab));
        };
        let (_, _, n) = self.node_def(&nav, a, id).await?;
        let t = n.and_then(|n| n.tabs.iter().find(|t| t.label == tab)).ok_or_else(|| BaseError::not_found("aspect.no_tab", "no such tab").with("tab", tab))?.clone();
        let mut rows = vec![];
        for row in self.objects_of(&nav, id, &t.path).await? {
            let Some(o) = self.plain_object_out(&nav, row, lang).await? else { continue };
            let mut cells = vec![];
            for c in &t.columns {
                let vals = eval(&nav, &expr(&c.path)?, &[Item::Obj(row)]).await?;
                cells.push(Value::Array(vals.iter().map(|i| i.to_json()).collect()));
            }
            rows.push(TabRow { node: o.node, display_name: o.display_name, type_name: o.type_name, cells });
        }
        if let Some((col, desc)) = sort {
            if col >= t.columns.len() {
                return Err(BaseError::invalid("aspect.bad_sort", "sort names a column index of the tab").with("sort", col.to_string()));
            }
            rows.sort_by_cached_key(|r| cell_text(&r.cells[col]).to_lowercase());
            if desc {
                rows.reverse();
            }
        }
        let total = rows.len();
        let rows = match page.limit {
            Some(l) => rows.into_iter().skip(page.offset.max(0) as usize).take(l.clamp(1, 1000) as usize).collect(),
            None => rows,
        };
        Ok(TabPage {
            key: t.label.clone(),
            label: self.i18n.label(&a.name, lang, &t.label),
            columns: t.columns.iter().map(|c| self.i18n.label(&a.name, lang, &c.label)).collect(),
            total,
            rows,
        })
    }

    /// Content of a file of an object node: (bytes, media type).
    /// The files of an object node with what stands behind them (content or text).
    pub async fn files_of_node(&self, ctx: &Ctx, aspect: &str, node: &str) -> Result<Vec<(String, Item)>> {
        let nav = StoreNav::new(self, ctx);
        let NodeRef::Object(id) = parse_node(node)? else {
            return Err(BaseError::invalid("aspect.bad_node", "files belong to object nodes"));
        };
        if is_builtin(aspect) {
            let v = nav.view(id).await?.ok_or_else(|| BaseError::not_found("object.not_found", "object not found"))?;
            return Ok(Self::plain_files(&v));
        }
        let a = self.aspect_def(aspect)?;
        let (v, _, n) = self.node_def(&nav, a, id).await?;
        let Some(n) = n else { return Ok(vec![]) };
        let attrs = StoreNav::attrs(&v);
        self.files_of(&nav, id, n, &attrs).await
    }

    pub async fn aspect_file(&self, ctx: &Ctx, aspect: &str, node: &str, name: &str) -> Result<(Vec<u8>, String)> {
        if is_builtin(aspect) {
            return self.builtin_file(ctx, node, name).await;
        }
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

    // ---- built-in views (`_id`, `_type`): derived from the catalog, no definition needed ----

    fn group_out(node: String, name: String, count: Option<i64>) -> NodeOut {
        NodeOut { node, file: false, display_name: name, display_description: String::new(), count, type_name: None, id: None, ..Default::default() }
    }

    /// Object node with the global display texts.
    async fn plain_object_out(&self, nav: &StoreNav<'_>, id: Uuid, lang: &str) -> Result<Option<NodeOut>> {
        let Some(v) = nav.view(id).await? else { return Ok(None) };
        let t = v.types.iter().find(|x| !x.implied).or(v.types.first()).map(|x| x.type_name.clone()).unwrap_or_default();
        let mut attrs = StoreNav::attrs(&v);
        for anc in self.model.ancestors(&t) {
            for x in &anc.attributes {
                attrs.entry(x.name.clone()).or_insert_with(|| Value::String(String::new()));
            }
        }
        let (n, d) = self.i18n.names_in(None, lang, &t, &attrs);
        Ok(Some(NodeOut { node: format!("o:{id}"), file: false, display_name: n, display_description: d, count: None, type_name: Some(t), id: Some(id), ..Default::default() }))
    }

    /// Files of an object in the built-in views: its content and its texts.
    fn plain_files(v: &ObjectView) -> Vec<(String, Item)> {
        let mut out = vec![];
        if let Some(c) = &v.content {
            let name = c.get("filename").and_then(|x| x.as_str()).filter(|x| !x.is_empty()).unwrap_or("content").replace('/', "_");
            out.push((name, Item::Content(v.id)));
        }
        for t in &v.texts {
            let local = t.split_once(':').map(|x| x.1).unwrap_or(t);
            out.push((local.replace('/', "_"), Item::Text(v.id, t.clone())));
        }
        out
    }

    /// Named ends of the object that currently hold at least one readable object.
    async fn filled_ends(&self, ctx: &Ctx, v: &ObjectView) -> Result<Vec<String>> {
        let set: BTreeSet<String> = v.types.iter().map(|t| t.type_name.clone()).collect();
        let mut names: Vec<String> = self
            .model
            .ends_for(&set)
            .into_iter()
            .map(|(r, side)| match side {
                i3cm_core::catalog::Side::Holder => r.holder.end.clone(),
                i3cm_core::catalog::Side::Target => r.target.end.clone(),
            })
            .filter(|n| !n.is_empty())
            .collect();
        names.sort();
        names.dedup();
        let mut out = vec![];
        for n in names {
            if !self.ends(ctx, v.id, &n, 1, 0, "en").await?.is_empty() {
                out.push(n);
            }
        }
        Ok(out)
    }

    async fn count_of_type(&self, ctx: &Ctx, t: &str) -> Result<i64> {
        let def = self.model.require_type(t)?;
        let access = self.access(ctx, i3cm_core::Art::Read, t)?;
        if access.is_empty() {
            return Ok(0);
        }
        let mut params: Vec<Param> = vec![Box::new(ctx.tenant)];
        let rights = self.rights_clause(&access, ctx, &mut params);
        let sql = format!(
            "SELECT count(*) FROM {} x JOIN object_state s ON s.tenant_id=x.tenant_id AND s.object_id=x.object_id AND s.valid_to IS NULL AND s.deleted_at IS NULL \
             WHERE x.tenant_id=$1 AND x.valid_to IS NULL AND x.deleted_at IS NULL AND {rights}",
            type_table(def)
        );
        let c = self.client().await?;
        Ok(c.query_one(&sql, &refs(&params)).await.map_err(db)?.get(0))
    }

    async fn builtin_children(&self, ctx: &Ctx, nav: &StoreNav<'_>, aspect: &str, node: &str, lang: &str, page: Page) -> Result<(Vec<NodeOut>, bool)> {
        let limit = page.limit.unwrap_or(MAX_LIST).clamp(1, MAX_LIST);
        Ok(match (aspect, parse_node(node)?) {
            (VIEW_ID, NodeRef::Root) => (vec![], true),
            (VIEW_TYPE, NodeRef::Root) => {
                let mut out = vec![];
                for t in &self.model.catalog.types {
                    let n = self.count_of_type(ctx, &t.name).await?;
                    if n > 0 {
                        out.push(Self::group_out(format!("g:{}", esc(&t.name)), t.name.clone(), Some(n)));
                    }
                }
                (out, false)
            }
            (VIEW_TYPE, NodeRef::Group(keys)) if keys.len() == 1 => {
                let q = crate::read::ListQuery { limit: Some(limit), offset: Some(page.offset), ..Default::default() };
                let mut out = vec![];
                for s in self.list(ctx, &keys[0], &q, lang).await? {
                    out.extend(self.plain_object_out(nav, s.id, lang).await?);
                }
                (out, true)
            }
            (_, NodeRef::Object(id)) => {
                let v = nav.view(id).await?.ok_or_else(|| BaseError::not_found("object.not_found", "object not found"))?;
                let mut out: Vec<NodeOut> = Self::plain_files(&v)
                    .into_iter()
                    .map(|(name, it)| NodeOut { node: format!("o:{id}"), file: true, display_name: name, display_description: String::new(), count: None, type_name: None, id: Some(id), src: Some(it), ..Default::default() })
                    .collect();
                for e in self.filled_ends(ctx, &v).await? {
                    out.push(NodeOut { node: format!("f:{id}:{e}"), file: false, display_name: e, display_description: String::new(), count: None, type_name: None, id: None, ..Default::default() });
                }
                (out, false)
            }
            (_, NodeRef::Folder(id, end)) => {
                let mut out = vec![];
                for s in self.ends(ctx, id, &end, limit, page.offset, lang).await? {
                    out.extend(self.plain_object_out(nav, s.id, lang).await?);
                }
                (out, true)
            }
            _ => return Err(BaseError::invalid("aspect.bad_node", "unknown node id").with("node", node)),
        })
    }

    async fn builtin_node(&self, ctx: &Ctx, aspect: &str, node: &str, lang: &str) -> Result<NodeDetail> {
        let nav = StoreNav::new(self, ctx);
        let empty = |n: NodeOut| NodeDetail { node: n, attributes: vec![], tabs: vec![], files: vec![], functions: vec![] };
        match (aspect, parse_node(node)?) {
            (VIEW_TYPE, NodeRef::Group(keys)) if keys.len() == 1 => {
                let (n, _) = self.i18n.names(lang, &keys[0], &Map::new());
                let mut g = Self::group_out(node.to_string(), keys[0].clone(), Some(self.count_of_type(ctx, &keys[0]).await?));
                g.display_description = n;
                Ok(empty(g))
            }
            (_, NodeRef::Object(id)) => {
                let out = self.plain_object_out(&nav, id, lang).await?.ok_or_else(|| BaseError::not_found("object.not_found", "object not found"))?;
                let v = nav.view(id).await?.ok_or_else(|| BaseError::not_found("object.not_found", "object not found"))?;
                let mut attributes = vec![
                    LabeledValues { key: "_id".into(), label: "_id".into(), values: vec![Value::String(id.to_string())], display: vec![] },
                    LabeledValues { key: "_room".into(), label: "_room".into(), values: vec![Value::String(v.room.to_string())], display: vec![] },
                    LabeledValues { key: "_type".into(), label: "_type".into(), values: v.types.iter().map(|t| Value::String(t.type_name.clone())).collect(), display: vec![] },
                ];
                for name in ["_createdBy", "_createdAt", "_changedBy", "_changedAt"] {
                    let values = nav.member(&Item::Obj(id), name).await?.iter().map(|i| i.to_json()).collect();
                    attributes.push(LabeledValues { key: name.into(), label: name.into(), values, display: vec![] });
                }
                for p in &v.parts {
                    for (k, x) in &p.attributes {
                        let values = match x {
                            Value::Null => vec![],
                            Value::Array(a) => a.clone(),
                            o => vec![o.clone()],
                        };
                        attributes.push(LabeledValues { key: k.clone(), label: k.clone(), values, display: vec![] });
                    }
                }
                let files = Self::plain_files(&v).into_iter().map(|(n, _)| n).collect();
                Ok(NodeDetail { node: out, attributes, tabs: vec![], files, functions: vec![] })
            }
            _ => Err(BaseError::invalid("aspect.bad_node", "details exist for objects and types")),
        }
    }

    async fn builtin_file(&self, ctx: &Ctx, node: &str, name: &str) -> Result<(Vec<u8>, String)> {
        let nav = StoreNav::new(self, ctx);
        let NodeRef::Object(id) = parse_node(node)? else {
            return Err(BaseError::invalid("aspect.bad_node", "files belong to object nodes"));
        };
        let v = nav.view(id).await?.ok_or_else(|| BaseError::not_found("object.not_found", "object not found"))?;
        let (_, it) = Self::plain_files(&v).into_iter().find(|(f, _)| f == name).ok_or_else(|| BaseError::not_found("aspect.no_file", "no such file").with("name", name))?;
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

    // ---- index (object file), entry metadata, batch reading ----

    fn group_index(display: &str) -> String {
        format!("# {display}\n")
    }

    async fn text_size(&self, ctx: &Ctx, id: Uuid, tt: &str) -> Result<u64> {
        let c = self.client().await?;
        let r = c
            .query_opt("SELECT octet_length(content) FROM text_part WHERE tenant_id=$1 AND object_id=$2 AND text_type=$3 AND valid_to IS NULL", &[&ctx.tenant, &id, &tt])
            .await
            .map_err(db)?;
        Ok(r.map(|r| r.get::<_, i32>(0) as u64).unwrap_or(0))
    }

    /// Set size, change time and inode of entries (`name` must be set: it is the directory name
    /// that relative links in an index start from).
    async fn decorate(&self, ctx: &Ctx, nav: &StoreNav<'_>, aspect: &str, lang: &str, out: &mut [NodeOut]) -> Result<()> {
        for n in out.iter_mut() {
            if let Some(id) = n.id {
                n.changed_at = nav.changed(id).await?.map(|c| c.0);
            }
            if n.file {
                match &n.src {
                    Some(Item::Content(id)) => {
                        let v = nav.view(*id).await?;
                        n.size = v.and_then(|v| v.content.as_ref().and_then(|c| c["size"].as_u64()));
                        n.ino = Some(ino(&["c", &id.to_string()]));
                    }
                    Some(Item::Text(id, tt)) => {
                        n.size = Some(self.text_size(ctx, *id, tt).await?);
                        n.ino = Some(ino(&["t", &id.to_string(), tt]));
                    }
                    _ => {}
                }
            } else {
                let name = n.name.clone();
                n.size = Some(self.index_for(ctx, nav, aspect, n, &name, lang).await?.len() as u64);
                n.ino = Some(match (n.id, n.node.starts_with("o:")) {
                    (Some(id), true) => ino(&["i", aspect, &id.to_string()]),
                    _ => ino(&["i", aspect, &n.node]),
                });
            }
        }
        Ok(())
    }

    /// Directory names of the node entries below `node`, as FUSE and the tools show them (complete
    /// listing, collisions resolved by [`project`]); key: node id.
    async fn dir_names_below(&self, ctx: &Ctx, nav: &StoreNav<'_>, aspect: &str, node: &str, lang: &str) -> Result<HashMap<String, String>> {
        let mut all: Vec<NodeOut> = vec![];
        let mut offset = 0;
        loop {
            let page = Page { offset, limit: Some(MAX_LIST), after: all.last().map(|c| c.node.clone()) };
            let (mut out, paged) = if is_builtin(aspect) { self.builtin_children(ctx, nav, aspect, node, lang, page).await? } else { self.children_raw(ctx, nav, aspect, node, lang, page).await? };
            let n = out.len() as i64;
            all.append(&mut out);
            if !paged || n < MAX_LIST {
                break;
            }
            offset += n;
        }
        for c in all.iter_mut() {
            c.name = fs_name(&c.display_name);
            c.fallback_name = c.fallback_display.as_deref().map(fs_name);
        }
        let named: Vec<Named> = all.iter().map(|c| Named { name: &c.name, file: c.file, fallback: c.fallback_name.as_deref(), key: &c.node }).collect();
        let entries = project(&named);
        Ok(entries.iter().filter_map(|(name, slot)| match slot { Slot::Dir(i) => Some((all[*i].node.clone(), name.clone())), _ => None }).collect())
    }

    /// The index of a node as Markdown. Objects: front matter with the reserved fields and the
    /// readable attributes, then the display name and references as `[[links]]` (relative below
    /// `dir`, otherwise `/_id/<id>`). Groups and folders: their name.
    async fn index_for(&self, ctx: &Ctx, nav: &StoreNav<'_>, aspect: &str, n: &NodeOut, dir: &str, lang: &str) -> Result<String> {
        let id = match parse_node(&n.node)? {
            NodeRef::Object(id) => id,
            NodeRef::Root => return Ok(Self::group_index(&self.aspect_display(aspect, lang))),
            _ => return Ok(Self::group_index(&n.display_name)),
        };
        let v = nav.view(id).await?.ok_or_else(|| BaseError::not_found("object.not_found", "object not found"))?;
        let j = |x: &Value| serde_json::to_string(x).unwrap_or_default();
        let mut s = String::from("---\n");
        s.push_str(&format!("_id: {}\n", j(&Value::String(id.to_string()))));
        s.push_str(&format!("_type: {}\n", j(&Value::Array(v.types.iter().map(|t| Value::String(t.type_name.clone())).collect()))));
        s.push_str(&format!("_room: {}\n", j(&Value::String(v.room.to_string()))));
        for (k, x) in StoreNav::attrs(&v) {
            if !x.is_null() {
                s.push_str(&format!("{k}: {}\n", j(&x)));
            }
        }
        s.push_str(&format!("---\n# {}\n", n.display_name));
        if !n.display_description.is_empty() {
            s.push_str(&format!("\n{}\n", n.display_description));
        }
        let link = |target: String, label: &str| format!("- [[{target}|{}]]\n", label.replace('|', "-"));
        let ends = self.filled_ends(ctx, &v).await?;
        // links name exactly the entry a listing shows (DEC: unique names by fallback and key)
        let here = self.dir_names_below(ctx, nav, aspect, &n.node, lang).await?;
        let name_in = |m: &HashMap<String, String>, node: &str, display: &str| m.get(node).cloned().unwrap_or_else(|| fs_name(display));
        if is_builtin(aspect) {
            for e in ends {
                s.push_str(&format!("\n## {e}\n\n"));
                let folder = format!("f:{id}:{e}");
                let below = self.dir_names_below(ctx, nav, aspect, &folder, lang).await?;
                let fname = name_in(&here, &folder, &e);
                for x in self.ends(ctx, id, &e, MAX_LIST, 0, lang).await? {
                    if let Some(c) = self.plain_object_out(nav, x.id, lang).await? {
                        s.push_str(&link(format!("{dir}/{fname}/{}", name_in(&below, &c.node, &c.display_name)), &c.display_name));
                    }
                }
            }
            return Ok(s);
        }
        let a = self.aspect_def(aspect)?;
        let (_, _, def) = self.node_def(nav, a, id).await?;
        let mut covered = BTreeSet::new();
        if let Some(def) = def {
            for sn in &def.sub_nodes {
                covered.insert(sn.path.clone());
                let ids = self.objects_of(nav, id, &sn.path).await?;
                if ids.is_empty() {
                    continue;
                }
                let label = if sn.folder { self.i18n.label(&a.name, lang, &sn.label) } else { sn.label.clone() };
                s.push_str(&format!("\n## {label}\n\n"));
                let folder = format!("f:{id}:{}", sn.label);
                let below = if sn.folder { self.dir_names_below(ctx, nav, aspect, &folder, lang).await? } else { HashMap::new() };
                for x in ids {
                    if let Some(c) = self.object_out(nav, a, x, lang).await? {
                        let target = if sn.folder {
                            format!("{dir}/{}/{}", name_in(&here, &folder, &label), name_in(&below, &c.node, &c.display_name))
                        } else {
                            format!("{dir}/{}", name_in(&here, &c.node, &c.display_name))
                        };
                        s.push_str(&link(target, &c.display_name));
                    }
                }
            }
        }
        for e in ends.into_iter().filter(|e| !covered.contains(e)) {
            s.push_str(&format!("\n## {e}\n\n"));
            for x in self.ends(ctx, id, &e, MAX_LIST, 0, lang).await? {
                if let Some(c) = self.plain_object_out(nav, x.id, lang).await? {
                    s.push_str(&link(format!("/_id/{}", x.id), &c.display_name));
                }
            }
        }
        Ok(s)
    }

    /// The node itself (not its children), undecorated.
    async fn node_out_for(&self, nav: &StoreNav<'_>, aspect: &str, node: &str, lang: &str) -> Result<NodeOut> {
        let nf = || BaseError::not_found("object.not_found", "object not found");
        let builtin = is_builtin(aspect);
        let a = if builtin { None } else { Some(self.aspect_def(aspect)?) };
        Ok(match parse_node(node)? {
            NodeRef::Root => Self::group_out(String::new(), self.aspect_display(aspect, lang), None),
            NodeRef::Object(id) => match a {
                Some(a) => self.object_out(nav, a, id, lang).await?.ok_or_else(nf)?,
                None => self.plain_object_out(nav, id, lang).await?.ok_or_else(nf)?,
            },
            NodeRef::Group(keys) => Self::group_out(node.to_string(), keys.last().cloned().unwrap_or_default(), None),
            NodeRef::Folder(id, label) => {
                nav.view(id).await?.ok_or_else(nf)?;
                let d = match a {
                    Some(a) => self.i18n.label(&a.name, lang, &label),
                    None => label,
                };
                Self::group_out(node.to_string(), d, None)
            }
        })
    }

    /// One entry with metadata as in a listing; `name` overrides the directory name.
    pub async fn aspect_entry(&self, ctx: &Ctx, aspect: &str, node: &str, name: Option<&str>, lang: &str) -> Result<NodeOut> {
        let nav = StoreNav::new(self, ctx);
        let mut n = self.node_out_for(&nav, aspect, node, lang).await?;
        n.name = name.map(String::from).unwrap_or_else(|| fs_name(&n.display_name));
        self.decorate(ctx, &nav, aspect, lang, std::slice::from_mut(&mut n)).await?;
        Ok(n)
    }

    /// The index of a node (FUSE: `<name>.md` beside the directory `<name>/`).
    pub async fn aspect_index(&self, ctx: &Ctx, aspect: &str, node: &str, name: Option<&str>, lang: &str) -> Result<String> {
        let nav = StoreNav::new(self, ctx);
        let n = self.node_out_for(&nav, aspect, node, lang).await?;
        let dir = name.map(String::from).unwrap_or_else(|| fs_name(&n.display_name));
        self.index_for(ctx, &nav, aspect, &n, &dir, lang).await
    }

    /// Keys (`t:<object>:<text type>`) of current texts containing `literal`: the prefilter of
    /// `grep` (a trigram index makes it fast; without one it scans). Rights are not checked:
    /// the result only decides what is skipped.
    pub async fn text_candidates(&self, ctx: &Ctx, literal: &str, icase: bool) -> Result<std::collections::HashSet<String>> {
        let esc = literal.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let op = if icase { "ILIKE" } else { "LIKE" };
        let c = self.client().await?;
        let rows = c
            .query(&format!("SELECT object_id, text_type FROM text_part WHERE tenant_id=$1 AND valid_to IS NULL AND content {op} $2"), &[&ctx.tenant, &format!("%{esc}%")])
            .await
            .map_err(db)?;
        Ok(rows.iter().map(|r| format!("t:{}:{}", r.get::<_, Uuid>(0), r.get::<_, String>(1))).collect())
    }
}
