//! Reading: objects (current or at a version), versions, reference ends, lists per type with
//! an attribute filter, date grouping, groups and members, rooms, content and texts.

use crate::context::{db, Ctx};
use crate::order::GROUP_MEMBERSHIP;
use crate::schema::{col, ref_table, type_table};
use crate::sql::{param, refs, Param};
use crate::Store;
use i3cm_core::catalog::{Art, Side, ValueType, ANY};
use i3cm_core::values::{to_val, Col};
use i3cm_core::{BaseError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use tokio_postgres::GenericClient;
use uuid::Uuid;

/// Validity condition for a row at a version (or the current row).
fn at_cond(alias: &str, at: Option<i64>, p: usize) -> String {
    match at {
        None => format!("{alias}valid_to IS NULL"),
        Some(_) => format!("{alias}valid_from <= ${p} AND ({alias}valid_to IS NULL OR {alias}valid_to > ${p})"),
    }
}

/// (room, deleted_at) of an object.
pub(crate) async fn state_of<C: GenericClient>(c: &C, tenant: Uuid, id: Uuid, at: Option<i64>) -> Result<Option<(Uuid, Option<chrono::DateTime<chrono::Utc>>)>> {
    let sql = format!("SELECT room_id, deleted_at FROM object_state WHERE tenant_id=$1 AND object_id=$2 AND {}", at_cond("", at, 3));
    let row = match at {
        None => c.query_opt(&sql, &[&tenant, &id]).await,
        Some(v) => c.query_opt(&sql, &[&tenant, &id, &v]).await,
    }
    .map_err(db)?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

/// Attached types (type -> implied) of an object.
pub(crate) async fn types_of<C: GenericClient>(c: &C, tenant: Uuid, id: Uuid, at: Option<i64>) -> Result<BTreeMap<String, bool>> {
    let sql = format!("SELECT type_name, implied FROM object_type WHERE tenant_id=$1 AND object_id=$2 AND deleted_at IS NULL AND {}", at_cond("", at, 3));
    let rows = match at {
        None => c.query(&sql, &[&tenant, &id]).await,
        Some(v) => c.query(&sql, &[&tenant, &id, &v]).await,
    }
    .map_err(db)?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Part {
    #[serde(rename = "type")]
    pub type_name: String,
    pub attributes: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shown {
    #[serde(rename = "type")]
    pub type_name: String,
    pub implied: bool,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "displayDescription")]
    pub display_description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectView {
    pub id: Uuid,
    pub room: Uuid,
    pub deleted: bool,
    pub created_by: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Display names of the attached types the caller can read.
    pub types: Vec<Shown>,
    /// Attribute values per type part the caller can read (attached types and supertypes).
    pub parts: Vec<Part>,
    pub content: Option<Value>,
    pub texts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub id: Uuid,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub types: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListQuery {
    pub attr: Option<String>,
    pub eq: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    #[serde(default)]
    pub desc: bool,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Keyset paging: continue after this object (the last one of the previous page).
    pub after: Option<Uuid>,
}

impl Store {
    /// Unset attributes render as empty text in display templates.
    fn fill_missing(&self, t: &str, attrs: &mut Map<String, Value>) {
        for a in self.model.ancestors(t) {
            for x in &a.attributes {
                attrs.entry(x.name.clone()).or_insert_with(|| Value::String(String::new()));
            }
        }
    }

    /// Current attributes of type part `t` for several objects in one query.
    async fn rows_json<C: GenericClient>(&self, c: &C, tenant: Uuid, ids: &[Uuid], t: &str) -> Result<std::collections::HashMap<Uuid, Map<String, Value>>> {
        let def = self.model.require_type(t)?;
        if ids.is_empty() {
            return Ok(Default::default());
        }
        let cols: Vec<String> = def.attributes.iter().map(|a| match Col::of(&a.vt) {
            Col::Num if !a.list => format!("{}::text AS {}", col(&a.name), col(&a.name)),
            _ => col(&a.name),
        }).collect();
        let sel = if cols.is_empty() { "1 AS _".to_string() } else { cols.join(",") };
        let sql = format!(
            "SELECT object_id, to_jsonb(x) - 'object_id' FROM (SELECT object_id, {sel} FROM {} WHERE tenant_id=$1 AND object_id = ANY($2) AND deleted_at IS NULL AND valid_to IS NULL) x",
            type_table(def)
        );
        let rows = c.query(&sql, &[&tenant, &ids]).await.map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let mut v: Value = r.get(1);
                let mut m = v.as_object_mut().map(std::mem::take).unwrap_or_default();
                m.remove("_");
                m.retain(|_, v| !v.is_null());
                (r.get(0), m)
            })
            .collect())
    }

    async fn row_json<C: GenericClient>(&self, c: &C, tenant: Uuid, id: Uuid, t: &str, at: Option<i64>) -> Result<Option<Map<String, Value>>> {
        let def = self.model.require_type(t)?;
        let cols: Vec<String> = def.attributes.iter().map(|a| match Col::of(&a.vt) {
            Col::Num if !a.list => format!("{}::text AS {}", col(&a.name), col(&a.name)),
            _ => col(&a.name),
        }).collect();
        let sel = if cols.is_empty() { "1 AS _".to_string() } else { cols.join(",") };
        let sql = format!(
            "SELECT to_jsonb(x) FROM (SELECT {sel} FROM {} WHERE tenant_id=$1 AND object_id=$2 AND deleted_at IS NULL AND {}) x",
            type_table(def),
            at_cond("", at, 3)
        );
        let row = match at {
            None => c.query_opt(&sql, &[&tenant, &id]).await,
            Some(v) => c.query_opt(&sql, &[&tenant, &id, &v]).await,
        }
        .map_err(db)?;
        Ok(row.map(|r| {
            let mut v: Value = r.get(0);
            let mut m = v.as_object_mut().map(std::mem::take).unwrap_or_default();
            m.remove("_");
            m.retain(|_, v| !v.is_null());
            m
        }))
    }

    async fn view<C: GenericClient>(&self, c: &C, ctx: &Ctx, id: Uuid, at: Option<i64>, lang: &str) -> Result<ObjectView> {
        let nf = || BaseError::not_found("object.not_found", "object not found").with("id", id.to_string());
        let (room, deleted) = state_of(c, ctx.tenant, id, at).await?.ok_or_else(nf)?;
        if deleted.is_some() && at.is_none() {
            return Err(nf());
        }
        // `me` may always read its own login parts (me can always be queried after login)
        let own = id == ctx.me();
        let path = ctx.rooms.path(room).unwrap_or_else(|_| vec![room]);
        let set = types_of(c, ctx.tenant, id, at).await?;
        let tables = self.model.table_types(&set.keys().cloned().collect());
        let mut parts = vec![];
        let mut readable: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
        for t in &tables {
            if !((own && self.model.is_a(t, "base:Carrier")) || self.may_obj(c, ctx, Art::Read, t, &path, id).await?) {
                continue;
            }
            if let Some(attrs) = self.row_json(c, ctx.tenant, id, t, at).await? {
                readable.insert(t.clone(), attrs.clone());
                parts.push(Part { type_name: t.clone(), attributes: attrs });
            }
        }
        if parts.is_empty() {
            return Err(nf());
        }
        let mut types = vec![];
        for (t, implied) in &set {
            if !readable.contains_key(t) {
                continue;
            }
            let mut merged = Map::new();
            for a in self.model.ancestors(t) {
                if let Some(x) = readable.get(&a.name) {
                    merged.extend(x.clone());
                }
            }
            self.fill_missing(t, &mut merged);
            let (n, d) = self.i18n.names(lang, t, &merged);
            types.push(Shown { type_name: t.clone(), implied: *implied, display_name: n, display_description: d });
        }
        let obj = c.query_one("SELECT created_by, created_at FROM object WHERE tenant_id=$1 AND id=$2", &[&ctx.tenant, &id]).await.map_err(db)?;
        let content_type = tables.iter().find(|t| self.model.type_def(t).map(|d| d.content.is_some()).unwrap_or(false) && readable.contains_key(*t));
        let content = if content_type.is_some() {
            let sql = format!(
                "SELECT jsonb_build_object('sha256', sha256, 'mediaType', media_type, 'size', size, 'filename', filename) FROM content WHERE tenant_id=$1 AND object_id=$2 AND {}",
                at_cond("", at, 3)
            );
            match at {
                None => c.query_opt(&sql, &[&ctx.tenant, &id]).await,
                Some(v) => c.query_opt(&sql, &[&ctx.tenant, &id, &v]).await,
            }
            .map_err(db)?
            .map(|r| r.get(0))
        } else {
            None
        };
        let mut texts = vec![];
        let sql = format!("SELECT text_type FROM text_part WHERE tenant_id=$1 AND object_id=$2 AND {} ORDER BY 1", at_cond("", at, 3));
        let rows = match at {
            None => c.query(&sql, &[&ctx.tenant, &id]).await,
            Some(v) => c.query(&sql, &[&ctx.tenant, &id, &v]).await,
        }
        .map_err(db)?;
        for r in rows {
            let tt: String = r.get(0);
            if let Some(def) = self.model.text_type(&tt) {
                if readable.contains_key(&def.owner) {
                    texts.push(tt);
                }
            }
        }
        Ok(ObjectView { id, room, deleted: deleted.is_some(), created_by: obj.get(0), created_at: obj.get(1), types, parts, content, texts })
    }

    pub async fn get_object(&self, ctx: &Ctx, id: Uuid, at: Option<i64>, lang: &str) -> Result<ObjectView> {
        let c = self.client().await?;
        self.view(&**c, ctx, id, at, lang).await
    }

    async fn summary<C: GenericClient>(&self, c: &C, ctx: &Ctx, id: Uuid, lang: &str) -> Result<Option<Summary>> {
        match self.view(c, ctx, id, None, lang).await {
            Ok(v) if !v.deleted => Ok(Some(Summary {
                id,
                display_name: v.types.iter().find(|t| !t.implied).or(v.types.first()).map(|t| t.display_name.clone()).unwrap_or_default(),
                types: v.types.iter().map(|t| t.type_name.clone()).collect(),
                link: None,
                value: None,
            })),
            Ok(_) => Ok(None),
            Err(e) if e.kind == i3cm_core::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Versions (transaction marks) that changed the object, with time and account.
    pub async fn versions(&self, ctx: &Ctx, id: Uuid) -> Result<Vec<Value>> {
        let c = self.client().await?;
        let v = self.view(&**c, ctx, id, None, "en").await?;
        let mut unions = vec!["SELECT valid_from FROM object_state WHERE tenant_id=$1 AND object_id=$2".to_string(),
            "SELECT valid_from FROM object_type WHERE tenant_id=$1 AND object_id=$2".to_string(),
            "SELECT valid_from FROM content WHERE tenant_id=$1 AND object_id=$2".to_string(),
            "SELECT valid_from FROM text_part WHERE tenant_id=$1 AND object_id=$2".to_string()];
        for p in &v.parts {
            unions.push(format!("SELECT valid_from FROM {} WHERE tenant_id=$1 AND object_id=$2", type_table(self.model.require_type(&p.type_name)?)));
        }
        let sql = format!(
            "SELECT t.id, t.at, t.account FROM txn t WHERE t.tenant_id=$1 AND t.id IN ({}) ORDER BY t.id",
            unions.join(" UNION ")
        );
        Ok(c.query(&sql, &[&ctx.tenant, &id])
            .await
            .map_err(db)?
            .iter()
            .map(|r| serde_json::json!({"version": r.get::<_, i64>(0), "at": r.get::<_, chrono::DateTime<chrono::Utc>>(1), "account": r.get::<_, Option<Uuid>>(2)}))
            .collect())
    }

    /// Objects at the named end of an object.
    pub async fn ends(&self, ctx: &Ctx, id: Uuid, end: &str, limit: i64, offset: i64, lang: &str) -> Result<Vec<Summary>> {
        let c = self.client().await?;
        self.view(&**c, ctx, id, None, lang).await?;
        let set: BTreeSet<String> = types_of(&**c, ctx.tenant, id, None).await?.into_keys().collect();
        let (def, side) = self
            .model
            .end_by_name(&set, end)
            .ok_or_else(|| BaseError::invalid("reference.unknown_end", "the object has no such end").with("end", end))?;
        let (this, other) = match side {
            Side::Holder => ("a_id", "b_id"),
            Side::Target => ("b_id", "a_id"),
        };
        let order = if def.ordered { "x.rank NULLS LAST, x.seq" } else { "x.seq" };
        let sql = format!(
            "SELECT x.{other}, x.id FROM {} x JOIN object_state s ON s.tenant_id=x.tenant_id AND s.object_id=x.{other} AND s.valid_to IS NULL AND s.deleted_at IS NULL \
             WHERE x.tenant_id=$1 AND x.{this}=$2 AND x.valid_to IS NULL ORDER BY {order} LIMIT $3 OFFSET $4",
            ref_table(def)
        );
        let rows = c.query(&sql, &[&ctx.tenant, &id, &limit, &offset]).await.map_err(db)?;
        let mut out = vec![];
        for r in rows {
            if let Some(mut s) = self.summary(&**c, ctx, r.get(0), lang).await? {
                s.link = Some(r.get(1));
                out.push(s);
            }
        }
        Ok(out)
    }

    fn filter_param(&self, t: &str, attr: &str, raw: &str) -> Result<Param> {
        let def = self.model.require_type(t)?;
        let a = def
            .attributes
            .iter()
            .find(|a| a.name == attr)
            .ok_or_else(|| BaseError::invalid("attribute.unknown", "the type itself does not define this attribute").with("type", t).with("attribute", attr))?;
        if a.list || Col::of(&a.vt) == Col::Json {
            return Err(BaseError::invalid("list.unsupported_attribute", "lists and structured values cannot be filtered").with("attribute", attr));
        }
        let json: Value = match a.vt {
            ValueType::Int => raw.parse::<i64>().map(Value::from).map_err(|_| BaseError::invalid("value.invalid", "integer expected"))?,
            ValueType::Boolean => Value::Bool(raw == "true"),
            _ => Value::String(raw.to_string()),
        };
        let mut a2 = a.clone();
        a2.required = true;
        let v = to_val(&a2, &json)?;
        Ok(param(&v, Col::of(&a.vt), false))
    }

    /// Objects of a type (with subtypes' objects included through the shared table), filtered by
    /// one attribute of the type itself, ordered by that attribute.
    pub async fn list(&self, ctx: &Ctx, t: &str, q: &ListQuery, lang: &str) -> Result<Vec<Summary>> {
        let def = self.model.require_type(t)?;
        let access = self.access(ctx, Art::Read, t)?;
        if access.is_empty() {
            return Ok(vec![]);
        }
        let tb = type_table(def);
        let mut params: Vec<Param> = vec![Box::new(ctx.tenant)];
        let mut conds = vec![];
        let order_col = match &q.attr {
            Some(a) => {
                let c = col(a);
                if !def.attributes.iter().any(|x| &x.name == a) {
                    return Err(BaseError::invalid("attribute.unknown", "the type itself does not define this attribute").with("attribute", a.clone()));
                }
                if let Some(eq) = &q.eq {
                    params.push(self.filter_param(t, a, eq)?);
                    conds.push(format!("x.{c} = ${}", params.len()));
                }
                if let Some(f) = &q.from {
                    params.push(self.filter_param(t, a, f)?);
                    conds.push(format!("x.{c} >= ${}", params.len()));
                }
                if let Some(to) = &q.to {
                    params.push(self.filter_param(t, a, to)?);
                    conds.push(format!("x.{c} <= ${}", params.len()));
                }
                c
            }
            // default order: the object id (stable, and the room index serves keyset paging)
            None => "object_id".to_string(),
        };
        let (dir, cmp) = if q.desc { ("DESC", "<") } else { ("ASC", ">") };
        if let Some(after) = q.after {
            params.push(Box::new(after));
            if order_col == "object_id" {
                conds.push(format!("x.object_id {cmp} ${}", params.len()));
            } else {
                conds.push(format!(
                    "(x.{order_col}, x.object_id) {cmp} (SELECT y.{order_col}, y.object_id FROM {tb} y WHERE y.tenant_id=$1 AND y.object_id=${} AND y.valid_to IS NULL)",
                    params.len()
                ));
            }
        }
        let limit = q.limit.unwrap_or(100).clamp(1, 1000);
        let offset = if q.after.is_some() { 0 } else { q.offset.unwrap_or(0).max(0) };
        let where_extra: String = conds.iter().map(|c| format!(" AND {c}")).collect();
        let value_sel = match &q.attr {
            Some(a) => format!("to_jsonb(x.{})", col(a)),
            None => "NULL::jsonb".to_string(),
        };
        // one branch per part of the access (rooms; each rights path), merged by UNION: no OR
        // in a single query, so every branch is driven by its own index (REP-018)
        let branches = self.rights_branches(&access, ctx, &mut params);
        let branch = |cond: &str, lim: i64| {
            format!(
                "SELECT x.object_id, {value_sel} AS v, x.{order_col} AS k FROM {tb} x JOIN object_state s ON s.tenant_id=x.tenant_id AND s.object_id=x.object_id AND s.valid_to IS NULL AND s.deleted_at IS NULL \
                 WHERE x.tenant_id=$1 AND x.valid_to IS NULL AND x.deleted_at IS NULL{where_extra} AND {cond} ORDER BY x.{order_col} {dir}, x.object_id {dir} LIMIT {lim}"
            )
        };
        let sql = if branches.len() == 1 {
            format!("SELECT object_id, v FROM ({}) u", branch(&branches[0], limit + offset)) + &format!(" ORDER BY k {dir}, object_id {dir} LIMIT {limit} OFFSET {offset}")
        } else {
            let parts: Vec<String> = branches.iter().map(|b| format!("({})", branch(b, limit + offset))).collect();
            format!("SELECT object_id, v FROM ({}) u ORDER BY k {dir}, object_id {dir} LIMIT {limit} OFFSET {offset}", parts.join(" UNION "))
        };
        let c = self.client().await?;
        let rows = c.query(&sql, &refs(&params)).await.map_err(db)?;
        let ids: Vec<Uuid> = rows.iter().map(|r| r.get(0)).collect();
        // the attributes of the whole page in one query (display names)
        let mut attrs_of = self.rows_json(&**c, ctx.tenant, &ids, t).await?;
        let mut out = vec![];
        for r in rows {
            let id: Uuid = r.get(0);
            let value: Option<Value> = r.get(1);
            let mut attrs = attrs_of.remove(&id).unwrap_or_default();
            self.fill_missing(t, &mut attrs);
            let (n, _) = self.i18n.names(lang, t, &attrs);
            out.push(Summary { id, display_name: n, types: vec![t.to_string()], link: None, value });
        }
        Ok(out)
    }

    /// Counts per year, month or day of a date or date-time attribute (date navigation).
    pub async fn group_counts(&self, ctx: &Ctx, t: &str, attr: &str, unit: &str, q: &ListQuery) -> Result<Vec<Value>> {
        let def = self.model.require_type(t)?;
        let a = def
            .attributes
            .iter()
            .find(|x| x.name == attr)
            .ok_or_else(|| BaseError::invalid("attribute.unknown", "the type itself does not define this attribute").with("attribute", attr))?;
        if !matches!(a.vt, ValueType::Date | ValueType::Datetime) || a.list {
            return Err(BaseError::invalid("group.unsupported_attribute", "grouping needs a date or date-time attribute").with("attribute", attr));
        }
        let unit = match unit {
            "year" | "month" | "day" => unit,
            _ => return Err(BaseError::invalid("group.unit", "unit must be year, month or day").with("unit", unit)),
        };
        let access = self.access(ctx, Art::Read, t)?;
        let mut params: Vec<Param> = vec![Box::new(ctx.tenant)];
        let rights = self.rights_clause(&access, ctx, &mut params);
        let mut conds = vec![rights];
        if let Some(f) = &q.from {
            params.push(self.filter_param(t, attr, f)?);
            conds.push(format!("x.{} >= ${}", col(attr), params.len()));
        }
        if let Some(to) = &q.to {
            params.push(self.filter_param(t, attr, to)?);
            conds.push(format!("x.{} <= ${}", col(attr), params.len()));
        }
        let where_extra = if conds.is_empty() { String::new() } else { format!(" AND {}", conds.join(" AND ")) };
        let sql = format!(
            "SELECT to_char(date_trunc('{unit}', x.{c}), '{fmt}') AS k, count(*) FROM {tb} x \
             JOIN object_state s ON s.tenant_id=x.tenant_id AND s.object_id=x.object_id AND s.valid_to IS NULL AND s.deleted_at IS NULL \
             WHERE x.tenant_id=$1 AND x.valid_to IS NULL AND x.deleted_at IS NULL AND x.{c} IS NOT NULL{where_extra} GROUP BY 1 ORDER BY 1",
            c = col(attr),
            tb = type_table(def),
            fmt = match unit {
                "year" => "YYYY",
                "month" => "YYYY-MM",
                _ => "YYYY-MM-DD",
            }
        );
        let c = self.client().await?;
        Ok(c.query(&sql, &refs(&params))
            .await
            .map_err(db)?
            .iter()
            .map(|r| serde_json::json!({"key": r.get::<_, String>(0), "count": r.get::<_, i64>(1)}))
            .collect())
    }

    /// Groups of `me`, resolved through subgroups.
    pub async fn my_groups(&self, ctx: &Ctx, lang: &str) -> Result<Vec<Summary>> {
        let c = self.client().await?;
        let mut out = vec![];
        for g in ctx.subject.carriers.iter().filter(|x| **x != ctx.me()) {
            if let Some(s) = self.summary(&**c, ctx, *g, lang).await? {
                out.push(s);
            } else {
                out.push(Summary { id: *g, display_name: String::new(), types: vec!["base:Group".into()], link: None, value: None });
            }
        }
        Ok(out)
    }

    /// Members of a group; with `transitive` also members of subgroups.
    pub async fn members(&self, ctx: &Ctx, group: Uuid, transitive: bool, lang: &str) -> Result<Vec<Summary>> {
        let c = self.client().await?;
        self.view(&**c, ctx, group, None, lang).await?;
        let gt = ref_table(self.model.ref_def("base:GroupMembership.group").unwrap());
        let mt = ref_table(self.model.ref_def("base:GroupMembership.member").unwrap());
        let live = "JOIN object_state s ON s.tenant_id=g.tenant_id AND s.object_id=g.a_id AND s.valid_to IS NULL AND s.deleted_at IS NULL";
        let sql = if transitive {
            format!(
                "WITH RECURSIVE c(id) AS (SELECT m.b_id FROM {gt} g {live} JOIN {mt} m ON m.tenant_id=g.tenant_id AND m.a_id=g.a_id AND m.valid_to IS NULL WHERE g.tenant_id=$1 AND g.b_id=$2 AND g.valid_to IS NULL \
                 UNION SELECT m.b_id FROM c JOIN {gt} g ON g.tenant_id=$1 AND g.b_id=c.id AND g.valid_to IS NULL {live} JOIN {mt} m ON m.tenant_id=g.tenant_id AND m.a_id=g.a_id AND m.valid_to IS NULL) SELECT id FROM c"
            )
        } else {
            format!("SELECT m.b_id FROM {gt} g {live} JOIN {mt} m ON m.tenant_id=g.tenant_id AND m.a_id=g.a_id AND m.valid_to IS NULL WHERE g.tenant_id=$1 AND g.b_id=$2 AND g.valid_to IS NULL")
        };
        let mut out = vec![];
        for r in c.query(&sql, &[&ctx.tenant, &group]).await.map_err(db)? {
            if let Some(s) = self.summary(&**c, ctx, r.get(0), lang).await? {
                out.push(s);
            }
        }
        Ok(out)
    }

    pub async fn room_children(&self, ctx: &Ctx, room: Uuid, lang: &str) -> Result<Vec<Summary>> {
        let c = self.client().await?;
        let mut out = vec![];
        let mut kids: Vec<Uuid> = ctx.rooms.parent.iter().filter(|(_, p)| **p == Some(room)).map(|(k, _)| *k).collect();
        kids.sort();
        for k in kids {
            if let Some(s) = self.summary(&**c, ctx, k, lang).await? {
                out.push(s);
            }
        }
        Ok(out)
    }

    pub async fn room_objects(&self, ctx: &Ctx, room: Uuid, limit: i64, offset: i64, lang: &str) -> Result<Vec<Summary>> {
        let c = self.client().await?;
        let rows = c
            .query(
                "SELECT object_id FROM object_state WHERE tenant_id=$1 AND room_id=$2 AND valid_to IS NULL AND deleted_at IS NULL AND object_id<>$2 ORDER BY valid_from, object_id LIMIT $3 OFFSET $4",
                &[&ctx.tenant, &room, &limit, &offset],
            )
            .await
            .map_err(db)?;
        let mut out = vec![];
        for r in rows {
            if let Some(s) = self.summary(&**c, ctx, r.get(0), lang).await? {
                out.push(s);
            }
        }
        Ok(out)
    }

    pub async fn room_path(&self, ctx: &Ctx, room: Uuid, lang: &str) -> Result<Vec<Summary>> {
        let c = self.client().await?;
        let mut out = vec![];
        for r in ctx.rooms.path(room)? {
            match self.summary(&**c, ctx, r, lang).await? {
                Some(s) => out.push(s),
                None => out.push(Summary { id: r, display_name: String::new(), types: vec![], link: None, value: None }),
            }
        }
        Ok(out)
    }

    pub async fn module_rooms(&self, ctx: &Ctx) -> Result<BTreeMap<String, Uuid>> {
        let c = self.client().await?;
        let mut out: BTreeMap<String, Uuid> = c
            .query("SELECT name, room_id FROM module_room WHERE tenant_id=$1", &[&ctx.tenant])
            .await
            .map_err(db)?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        out.insert("root".into(), ctx.rooms.root);
        Ok(out)
    }

    async fn type_with<'a, C: GenericClient>(&self, c: &C, ctx: &Ctx, id: Uuid, pick: impl Fn(&str) -> bool) -> Result<(String, Vec<Uuid>, i64)> {
        let (room, deleted) = state_of(c, ctx.tenant, id, None).await?.ok_or_else(|| BaseError::not_found("object.not_found", "object not found"))?;
        if deleted.is_some() {
            return Err(BaseError::not_found("object.not_found", "object not found"));
        }
        let set = types_of(c, ctx.tenant, id, None).await?;
        let tables = self.model.table_types(&set.keys().cloned().collect());
        let t = tables.into_iter().find(|t| pick(t)).ok_or_else(|| BaseError::invalid("type.no_such_part", "the object has no type allowing this"))?;
        Ok((t, ctx.rooms.path(room)?, 0))
    }

    /// Store binary content (new version).
    pub async fn put_content(&self, ctx: &Ctx, id: Uuid, data: &[u8], media_type: &str, filename: Option<&str>) -> Result<i64> {
        let mut c = self.client().await?;
        let (t, path, _) = self.type_with(&**c, ctx, id, |t| self.model.type_def(t).map(|d| d.content.is_some()).unwrap_or(false)).await?;
        if !self.may_obj(&**c, ctx, Art::Write, &t, &path, id).await? {
            return Err(BaseError::not_found("rights.denied", "not allowed or not found").with("art", "write").with("type", t));
        }
        let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(data));
        let tx = c.transaction().await.map_err(db)?;
        let txn: i64 = tx.query_one("SELECT nextval('txn_seq')", &[]).await.map_err(db)?.get(0);
        tx.execute("INSERT INTO txn (tenant_id, id, account) VALUES ($1,$2,$3)", &[&ctx.tenant, &txn, &ctx.me()]).await.map_err(db)?;
        tx.execute("INSERT INTO blob (sha256, data) VALUES ($1,$2) ON CONFLICT DO NOTHING", &[&sha, &data]).await.map_err(db)?;
        tx.execute("UPDATE content SET valid_to=$3 WHERE tenant_id=$1 AND object_id=$2 AND valid_to IS NULL", &[&ctx.tenant, &id, &txn]).await.map_err(db)?;
        let size = data.len() as i64;
        tx.execute(
            "INSERT INTO content (tenant_id, object_id, sha256, media_type, size, filename, valid_from) VALUES ($1,$2,$3,$4,$5,$6,$7)",
            &[&ctx.tenant, &id, &sha, &media_type, &size, &filename, &txn],
        )
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(txn)
    }

    pub async fn get_content(&self, ctx: &Ctx, id: Uuid) -> Result<(Vec<u8>, String, Option<String>)> {
        let c = self.client().await?;
        let (t, path, _) = self.type_with(&**c, ctx, id, |t| self.model.type_def(t).map(|d| d.content.is_some()).unwrap_or(false)).await?;
        if !self.may_obj(&**c, ctx, Art::Read, &t, &path, id).await? {
            return Err(BaseError::not_found("rights.denied", "not allowed or not found"));
        }
        let r = c
            .query_opt(
                "SELECT b.data, c.media_type, c.filename FROM content c JOIN blob b ON b.sha256=c.sha256 WHERE c.tenant_id=$1 AND c.object_id=$2 AND c.valid_to IS NULL",
                &[&ctx.tenant, &id],
            )
            .await
            .map_err(db)?
            .ok_or_else(|| BaseError::not_found("content.none", "the object has no content"))?;
        Ok((r.get(0), r.get(1), r.get(2)))
    }

    pub async fn put_text(&self, ctx: &Ctx, id: Uuid, text_type: &str, content: &str) -> Result<i64> {
        let def = self.model.text_type(text_type).ok_or_else(|| BaseError::invalid("text.unknown_type", "unknown text type").with("textType", text_type))?.clone();
        let mut c = self.client().await?;
        let (t, path, _) = self.type_with(&**c, ctx, id, |t| t == def.owner).await?;
        if !self.may_obj(&**c, ctx, Art::Write, &t, &path, id).await? {
            return Err(BaseError::not_found("rights.denied", "not allowed or not found").with("art", "write").with("type", t));
        }
        let tx = c.transaction().await.map_err(db)?;
        let txn: i64 = tx.query_one("SELECT nextval('txn_seq')", &[]).await.map_err(db)?.get(0);
        tx.execute("INSERT INTO txn (tenant_id, id, account) VALUES ($1,$2,$3)", &[&ctx.tenant, &txn, &ctx.me()]).await.map_err(db)?;
        tx.execute("UPDATE text_part SET valid_to=$4 WHERE tenant_id=$1 AND object_id=$2 AND text_type=$3 AND valid_to IS NULL", &[&ctx.tenant, &id, &text_type, &txn])
            .await
            .map_err(db)?;
        tx.execute(
            "INSERT INTO text_part (tenant_id, object_id, text_type, content, media_type, valid_from) VALUES ($1,$2,$3,$4,$5,$6)",
            &[&ctx.tenant, &id, &text_type, &content, &def.media_type, &txn],
        )
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(txn)
    }

    pub async fn get_text(&self, ctx: &Ctx, id: Uuid, text_type: &str) -> Result<(String, String)> {
        let def = self.model.text_type(text_type).ok_or_else(|| BaseError::invalid("text.unknown_type", "unknown text type"))?.clone();
        let c = self.client().await?;
        let (t, path, _) = self.type_with(&**c, ctx, id, |t| t == def.owner).await?;
        if !self.may_obj(&**c, ctx, Art::Read, &t, &path, id).await? {
            return Err(BaseError::not_found("rights.denied", "not allowed or not found"));
        }
        let r = c
            .query_opt("SELECT content, media_type FROM text_part WHERE tenant_id=$1 AND object_id=$2 AND text_type=$3 AND valid_to IS NULL", &[&ctx.tenant, &id, &text_type])
            .await
            .map_err(db)?
            .ok_or_else(|| BaseError::not_found("text.none", "the object has no such text"))?;
        Ok((r.get(0), r.get(1)))
    }

    /// Assign a profile to a carrier (needs the right to grant rights).
    pub async fn assign(&self, ctx: &Ctx, profile: &str, carrier: Uuid) -> Result<Uuid> {
        if !ctx.subject.may(Art::Grant, ANY, &[ctx.rooms.root]) && !ctx.subject.may(Art::Grant, GROUP_MEMBERSHIP, &[ctx.rooms.root]) {
            return Err(BaseError::not_found("rights.denied", "not allowed or not found").with("art", "grant"));
        }
        let mut c = self.client().await?;
        let exists = c.query_opt("SELECT 1 FROM profile WHERE tenant_id=$1 AND name=$2", &[&ctx.tenant, &profile]).await.map_err(db)?.is_some();
        if !exists {
            return Err(BaseError::invalid("profile.unknown", "unknown profile").with("profile", profile));
        }
        let set = types_of(&**c, ctx.tenant, carrier, None).await?;
        if !self.model.set_is_a(set.keys(), "base:Carrier") {
            return Err(BaseError::invalid("assignment.not_a_carrier", "profiles can only be assigned to groups and accounts"));
        }
        let tx = c.transaction().await.map_err(db)?;
        let txn: i64 = tx.query_one("SELECT nextval('txn_seq')", &[]).await.map_err(db)?.get(0);
        tx.execute("INSERT INTO txn (tenant_id, id, account) VALUES ($1,$2,$3)", &[&ctx.tenant, &txn, &ctx.me()]).await.map_err(db)?;
        let id = Uuid::new_v4();
        tx.execute("INSERT INTO assignment (tenant_id, id, profile, carrier, valid_from) VALUES ($1,$2,$3,$4,$5)", &[&ctx.tenant, &id, &profile, &carrier, &txn])
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(id)
    }

    pub async fn profiles(&self, ctx: &Ctx) -> Result<Vec<Value>> {
        let c = self.client().await?;
        Ok(c.query(
            "SELECT r.name, r.guardian, coalesce((SELECT array_agg(includes) FROM profile_include i WHERE i.tenant_id=r.tenant_id AND i.profile=r.name), '{}') FROM profile r WHERE tenant_id=$1 ORDER BY 1",
            &[&ctx.tenant],
        )
        .await
        .map_err(db)?
        .iter()
        .map(|r| serde_json::json!({"name": r.get::<_, String>(0), "guardian": r.get::<_, bool>(1), "includes": r.get::<_, Vec<String>>(2)}))
        .collect())
    }
}
