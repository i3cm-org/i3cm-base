//! Orders: one or more operations executed in one transaction. Rights are checked per
//! operation before it is applied (the start state; for new objects the end state), integrity
//! (preconditions, cardinalities) after all operations on the end state. A dry run performs
//! everything and rolls back.

use crate::context::{db, Ctx};
use crate::read::{state_of, types_of};
use crate::schema::{col, ref_table, type_table};
use crate::sql::{param, refs, Param};
use crate::Store;
use i3cm_core::catalog::{Art, Model, RefDef, RequireMode, Side, ANY};
use i3cm_core::values::{to_val, Col};
use i3cm_core::{BaseError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tokio_postgres::Client;
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Order {
    #[serde(default)]
    pub dry_run: bool,
    pub ops: Vec<Op>,
}

/// Object ids in operations are UUIDs or the local name (`ref`) of an object created earlier
/// in the same order.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Create {
        #[serde(rename = "ref", default)]
        local: Option<String>,
        #[serde(default)]
        room: Option<String>,
        types: BTreeMap<String, Map<String, Value>>,
    },
    Update { id: String, types: BTreeMap<String, Map<String, Value>> },
    Attach {
        id: String,
        #[serde(rename = "type")]
        type_name: String,
        #[serde(default)]
        values: Map<String, Value>,
    },
    Detach {
        id: String,
        #[serde(rename = "type")]
        type_name: String,
    },
    Delete { id: String },
    Move { id: String, room: String },
    Link {
        reference: String,
        from: String,
        to: String,
        #[serde(default)]
        rank: Option<i32>,
    },
    Unlink { reference: String, id: Uuid },
    /// New content version of an object (base64), in the same order as structure changes.
    Content {
        id: String,
        #[serde(rename = "mediaType")]
        media_type: String,
        #[serde(default)]
        filename: Option<String>,
        base64: String,
    },
    /// New version of a text of an object.
    Text {
        id: String,
        #[serde(rename = "textType")]
        text_type: String,
        content: String,
    },
    /// Internal (writing files through views, `sed -i`): new content version from a stored blob.
    #[serde(skip_deserializing)]
    SetContent { id: String, sha256: String, media_type: String, size: i64, filename: Option<String> },
    /// Internal: new version of a text.
    #[serde(skip_deserializing)]
    SetText { id: String, text_type: String, content: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResult {
    pub txn: i64,
    pub dry_run: bool,
    pub created: BTreeMap<String, Uuid>,
    pub links: Vec<Uuid>,
}

pub(crate) const GROUP_MEMBERSHIP: &str = "base:GroupMembership";

/// Art needed to create objects of a type (technical memberships grant rights).
fn create_art(t: &str) -> Art {
    if t == GROUP_MEMBERSHIP { Art::Grant } else { Art::Create }
}

pub(crate) struct Exec<'a> {
    pub(crate) tx: &'a Client,
    pub(crate) m: &'a Model,
    pub(crate) ctx: &'a mut Ctx,
    pub(crate) txn: i64,
    pub(crate) local: HashMap<String, Uuid>,
    pub(crate) new_objects: BTreeSet<Uuid>,
    pub(crate) touched: BTreeSet<Uuid>,
    pub(crate) detached: BTreeSet<Uuid>,
    pub(crate) links: Vec<Uuid>,
}

fn denied(art: Art, t: &str) -> BaseError {
    BaseError::not_found("rights.denied", "not allowed or not found").with("art", art.as_str()).with("type", t)
}

impl Store {
    pub async fn execute(&self, ctx: &mut Ctx, order: &Order) -> Result<OrderResult> {
        if order.ops.is_empty() {
            return Err(BaseError::invalid("order.empty", "order without operations"));
        }
        let rooms_before = ctx.rooms.clone();
        let mut sess = self.begin(ctx, &crate::session::Audit::default()).await?;
        for (i, op) in order.ops.iter().enumerate() {
            if let Err(e) = sess.apply(self, ctx, op).await {
                sess.abort().await;
                ctx.rooms = rooms_before;
                return Err(e.with("op_index", i));
            }
        }
        let res = sess.finish(self, ctx, !order.dry_run).await;
        if res.is_err() || order.dry_run {
            ctx.rooms = rooms_before;
        }
        res
    }
}

impl Exec<'_> {
    fn tenant(&self) -> Uuid {
        self.ctx.tenant
    }

    /// Types of modules not active in the tenant do not exist for it (DEC-035).
    fn active(&self, t: &str) -> Result<()> {
        if self.ctx.inactive.contains(Model::module_of(t)) {
            return Err(BaseError::not_found("module.inactive", "the module of this type is not active in the tenant").with("type", t));
        }
        Ok(())
    }

    fn resolve(&self, s: &str) -> Result<Uuid> {
        if let Some(id) = self.local.get(s) {
            return Ok(*id);
        }
        Uuid::parse_str(s).map_err(|_| BaseError::invalid("order.unknown_ref", "neither a UUID nor a local reference").with("id", s))
    }

    fn check(&self, art: Art, t: &str, path: &[Uuid]) -> Result<()> {
        if self.ctx.subject.may(art, t, path) { Ok(()) } else { Err(denied(art, t)) }
    }

    /// Like `check`, for an existing object: rights paths count too (read, write, delete).
    async fn allowed_on(&self, art: Art, t: &str, path: &[Uuid], id: Uuid) -> Result<bool> {
        if self.ctx.subject.may(art, t, path) {
            return Ok(true);
        }
        for cond in self.ctx.subject.conds(art, t, path) {
            let rp = i3cm_core::paths::rights_path(self.m, t, &cond)?;
            let sql = format!("SELECT EXISTS (SELECT 1 WHERE $2::uuid IN ({}))", crate::rights::path_ids_sql(&rp, self.m, 3));
            let hit: bool = self.tx.query_one(&sql, &[&self.ctx.tenant, &id, &self.ctx.subject.carriers]).await.map_err(db)?.get(0);
            if hit {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn check_on(&self, art: Art, t: &str, path: &[Uuid], id: Uuid) -> Result<()> {
        if self.allowed_on(art, t, path, id).await? { Ok(()) } else { Err(denied(art, t)) }
    }

    /// Current room path of an existing, not deleted object.
    async fn live(&self, id: Uuid) -> Result<(Uuid, Vec<Uuid>)> {
        match state_of(self.tx, self.tenant(), id, None).await? {
            Some((room, None)) => Ok((room, self.ctx.rooms.path(room)?)),
            _ => Err(BaseError::not_found("object.not_found", "object not found").with("id", id.to_string())),
        }
    }

    /// Split values given per type to the types that define each attribute.
    fn distribute(&self, given: &BTreeMap<String, Map<String, Value>>, allowed: &BTreeSet<String>) -> Result<BTreeMap<String, Map<String, Value>>> {
        let mut out: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
        for (t, attrs) in given {
            self.m.require_type(t)?;
            for (k, v) in attrs {
                let def = self
                    .m
                    .defining_type(t, k)
                    .ok_or_else(|| BaseError::invalid("attribute.unknown", "unknown attribute").with("type", t.clone()).with("attribute", k.clone()))?;
                if !allowed.contains(&def.name) {
                    return Err(BaseError::invalid("type.not_attached", "type is not attached").with("type", def.name.clone()));
                }
                out.entry(def.name.clone()).or_default().insert(k.clone(), v.clone());
            }
        }
        Ok(out)
    }

    async fn insert_type_row(&self, id: Uuid, t: &str, values: &Map<String, Value>) -> Result<()> {
        let def = self.m.require_type(t)?;
        if let Some(k) = values.keys().find(|k| !def.attributes.iter().any(|a| &a.name == *k)) {
            return Err(BaseError::invalid("attribute.unknown", "unknown attribute").with("type", t).with("attribute", k.clone()));
        }
        let mut cols = vec!["tenant_id".to_string(), "object_id".into(), "valid_from".into()];
        let mut params: Vec<Param> = vec![Box::new(self.tenant()), Box::new(id), Box::new(self.txn)];
        for a in &def.attributes {
            let v = to_val(a, values.get(&a.name).unwrap_or(&Value::Null)).map_err(|e| e.with("type", t))?;
            cols.push(col(&a.name));
            params.push(param(&v, Col::of(&a.vt), a.list));
        }
        let ph: Vec<String> = (1..=cols.len()).map(|i| format!("${i}")).collect();
        let sql = format!("INSERT INTO {} ({}) VALUES ({})", type_table(def), cols.join(","), ph.join(","));
        self.tx.execute(&sql, &refs(&params)).await.map_err(db)?;
        Ok(())
    }

    /// New version of a type row with changed attributes and/or deletion mark.
    async fn version_type_row(&self, id: Uuid, t: &str, changes: &Map<String, Value>, deleted: Option<bool>) -> Result<()> {
        let def = self.m.require_type(t)?;
        let tb = type_table(def);
        let cur: Option<i64> = self
            .tx
            .query_opt(&format!("SELECT valid_from FROM {tb} WHERE tenant_id=$1 AND object_id=$2 AND valid_to IS NULL"), &[&self.tenant(), &id])
            .await
            .map_err(db)?
            .map(|r| r.get(0));
        let Some(cur) = cur else {
            return Err(BaseError::invalid("type.not_attached", "type is not attached").with("type", t));
        };
        let mut params: Vec<Param> = vec![Box::new(self.tenant()), Box::new(id), Box::new(self.txn)];
        let mut sets: Vec<(String, String)> = vec![];
        for (k, v) in changes {
            let a = def
                .attributes
                .iter()
                .find(|a| &a.name == k)
                .ok_or_else(|| BaseError::invalid("attribute.unknown", "unknown attribute").with("type", t).with("attribute", k.clone()))?;
            let val = to_val(a, v).map_err(|e| e.with("type", t))?;
            params.push(param(&val, Col::of(&a.vt), a.list));
            sets.push((col(k), format!("${}", params.len())));
        }
        let del_expr = match deleted {
            Some(true) => "now()".to_string(),
            Some(false) => "NULL".to_string(),
            None => "deleted_at".to_string(),
        };
        if cur == self.txn {
            let mut assigns: Vec<String> = sets.iter().map(|(c, p)| format!("{c}={p}")).collect();
            assigns.push(format!("deleted_at={del_expr}"));
            let sql = format!("UPDATE {tb} SET {} WHERE tenant_id=$1 AND object_id=$2 AND valid_from=$3", assigns.join(","));
            self.tx.execute(&sql, &refs(&params)).await.map_err(db)?;
            return Ok(());
        }
        let mut cols = vec!["tenant_id".to_string(), "object_id".into(), "valid_from".into(), "deleted_at".into()];
        let mut exprs = vec!["tenant_id".to_string(), "object_id".into(), "$3".into(), del_expr];
        for a in &def.attributes {
            let c = col(&a.name);
            let e = sets.iter().find(|(x, _)| *x == c).map(|(_, p)| p.clone()).unwrap_or_else(|| c.clone());
            cols.push(c);
            exprs.push(e);
        }
        let sql = format!(
            "WITH old AS (UPDATE {tb} SET valid_to=$3 WHERE tenant_id=$1 AND object_id=$2 AND valid_to IS NULL RETURNING *) \
             INSERT INTO {tb} ({}) SELECT {} FROM old",
            cols.join(","),
            exprs.join(",")
        );
        self.tx.execute(&sql, &refs(&params)).await.map_err(db)?;
        Ok(())
    }

    async fn version_state(&self, id: Uuid, room: Option<Uuid>, delete: bool) -> Result<()> {
        let t = self.tenant();
        let cur: i64 = self
            .tx
            .query_one("SELECT valid_from FROM object_state WHERE tenant_id=$1 AND object_id=$2 AND valid_to IS NULL", &[&t, &id])
            .await
            .map_err(db)?
            .get(0);
        let del = if delete { "now()" } else { "deleted_at" };
        if cur == self.txn {
            self.tx
                .execute(
                    &format!("UPDATE object_state SET room_id=COALESCE($3, room_id), deleted_at={del} WHERE tenant_id=$1 AND object_id=$2 AND valid_to IS NULL"),
                    &[&t, &id, &room],
                )
                .await
                .map_err(db)?;
        } else {
            self.tx
                .execute(
                    &format!(
                        "WITH old AS (UPDATE object_state SET valid_to=$4 WHERE tenant_id=$1 AND object_id=$2 AND valid_to IS NULL RETURNING *) \
                         INSERT INTO object_state (tenant_id, object_id, room_id, deleted_at, valid_from) SELECT tenant_id, object_id, COALESCE($3, room_id), {del}, $4 FROM old"
                    ),
                    &[&t, &id, &room, &self.txn],
                )
                .await
                .map_err(db)?;
        }
        Ok(())
    }

    async fn set_type_entry(&self, id: Uuid, t: &str, implied: bool, deleted: bool) -> Result<()> {
        let tenant = self.tenant();
        self.tx
            .execute("UPDATE object_type SET valid_to=$4 WHERE tenant_id=$1 AND object_id=$2 AND type_name=$3 AND valid_to IS NULL AND valid_from<$4", &[&tenant, &id, &t, &self.txn])
            .await
            .map_err(db)?;
        self.tx
            .execute(
                "INSERT INTO object_type (tenant_id, object_id, type_name, implied, deleted_at, valid_from) VALUES ($1,$2,$3,$4, CASE WHEN $5 THEN now() END, $6) \
                 ON CONFLICT (tenant_id, object_id, type_name, valid_from) DO UPDATE SET implied=EXCLUDED.implied, deleted_at=EXCLUDED.deleted_at",
                &[&tenant, &id, &t, &implied, &deleted, &self.txn],
            )
            .await
            .map_err(db)?;
        Ok(())
    }

    /// Objects that are parts ("part of") of the given whole.
    async fn parts_of(&self, whole: Uuid) -> Result<Vec<Uuid>> {
        let mut out = vec![];
        for r in self.m.catalog.references.iter().filter(|r| r.part_of) {
            let sql = format!(
                "SELECT x.a_id FROM {} x JOIN object_state s ON s.tenant_id=x.tenant_id AND s.object_id=x.a_id AND s.valid_to IS NULL AND s.deleted_at IS NULL \
                 WHERE x.tenant_id=$1 AND x.b_id=$2 AND x.valid_to IS NULL",
                ref_table(r)
            );
            for row in self.tx.query(&sql, &[&self.tenant(), &whole]).await.map_err(db)? {
                out.push(row.get(0));
            }
        }
        Ok(out)
    }

    /// All objects on the other side of active references of `id`.
    async fn neighbours(&self, id: Uuid) -> Result<Vec<Uuid>> {
        let mut out = vec![];
        for r in &self.m.catalog.references {
            let sql = format!(
                "SELECT CASE WHEN a_id=$2 THEN b_id ELSE a_id END FROM {} WHERE tenant_id=$1 AND (a_id=$2 OR b_id=$2) AND valid_to IS NULL",
                ref_table(r)
            );
            for row in self.tx.query(&sql, &[&self.tenant(), &id]).await.map_err(db)? {
                out.push(row.get(0));
            }
        }
        Ok(out)
    }

    /// New content version of an object from a stored blob.
    async fn set_content(&mut self, id: &str, sha256: &String, media_type: &String, size: i64, filename: &Option<String>) -> Result<()> {
        let tenant = self.ctx.tenant;
        let id = self.resolve(id)?;
        let (_, path) = self.live(id).await?;
        let set = types_of(self.tx, tenant, id, None).await?;
        let tables = self.m.table_types(&set.keys().cloned().collect());
        let t = tables.into_iter().find(|t| self.m.type_def(t).map(|d| d.content.is_some()).unwrap_or(false))
            .ok_or_else(|| BaseError::invalid("type.no_such_part", "the object has no type with content"))?;
        if !self.new_objects.contains(&id) {
            self.check_on(Art::Write, &t, &path, id).await?;
        }
        self.tx.execute("UPDATE content SET valid_to=$3 WHERE tenant_id=$1 AND object_id=$2 AND valid_to IS NULL", &[&tenant, &id, &self.txn]).await.map_err(db)?;
        self.tx
            .execute(
                "INSERT INTO content (tenant_id, object_id, sha256, media_type, size, filename, valid_from) VALUES ($1,$2,$3,$4,$5,$6,$7)",
                &[&tenant, &id, sha256, media_type, &size, filename, &self.txn],
            )
            .await
            .map_err(db)?;
        self.touched.insert(id);
        Ok(())
    }

    pub(crate) async fn apply(&mut self, op: &Op) -> Result<()> {
        let tenant = self.tenant();
        match op {
            Op::Create { local, room, types } => {
                let id = Uuid::new_v4();
                if let Some(l) = local {
                    if self.local.insert(l.clone(), id).is_some() {
                        return Err(BaseError::invalid("order.duplicate_ref", "local reference used twice").with("ref", l.clone()));
                    }
                }
                let room = match room {
                    Some(r) => self.resolve(r)?,
                    None => self.ctx.rooms.root,
                };
                let path = self.ctx.rooms.path(room)?;
                if types.is_empty() {
                    return Err(BaseError::invalid("order.no_types", "an object needs at least one type"));
                }
                let explicit: BTreeSet<String> = types.keys().cloned().collect();
                for t in &explicit {
                    if self.m.require_type(t)?.is_abstract {
                        return Err(BaseError::invalid("type.abstract", "abstract types cannot be attached").with("type", t.clone()));
                    }
                    self.active(t)?;
                }
                let closure = self.m.implied_closure_in(&explicit, &self.ctx.inactive);
                let tables = self.m.table_types(&closure);
                let values = self.distribute(types, &tables)?;
                let mut need: BTreeSet<&String> = explicit.iter().collect();
                need.extend(values.keys());
                for t in need {
                    self.check(create_art(t), t, &path)?;
                }
                self.tx
                    .execute("INSERT INTO object (tenant_id, id, created_by, created_txn) VALUES ($1,$2,$3,$4)", &[&tenant, &id, &self.ctx.me(), &self.txn])
                    .await
                    .map_err(db)?;
                self.tx
                    .execute("INSERT INTO object_state (tenant_id, object_id, room_id, valid_from) VALUES ($1,$2,$3,$4)", &[&tenant, &id, &room, &self.txn])
                    .await
                    .map_err(db)?;
                for t in &closure {
                    self.set_type_entry(id, t, !explicit.contains(t), false).await?;
                }
                let empty = Map::new();
                for t in &tables {
                    self.insert_type_row(id, t, values.get(t).unwrap_or(&empty)).await?;
                }
                if closure.contains("base:Room") {
                    self.ctx.rooms.parent.insert(id, None);
                }
                self.new_objects.insert(id);
                self.touched.insert(id);
            }
            Op::Update { id, types } => {
                let id = self.resolve(id)?;
                let (_, path) = self.live(id).await?;
                let set = types_of(self.tx, tenant, id, None).await?;
                let tables = self.m.table_types(&set.keys().cloned().collect());
                let values = self.distribute(types, &tables)?;
                for (t, changes) in &values {
                    if !self.new_objects.contains(&id) {
                        self.check_on(Art::Write, t, &path, id).await?;
                    }
                    self.version_type_row(id, t, changes, None).await?;
                }
                self.touched.insert(id);
            }
            Op::Attach { id, type_name, values } => {
                self.active(type_name)?;
                let id = self.resolve(id)?;
                let (_, path) = self.live(id).await?;
                let def = self.m.require_type(type_name)?;
                if def.is_abstract {
                    return Err(BaseError::invalid("type.abstract", "abstract types cannot be attached").with("type", type_name.clone()));
                }
                let cur = types_of(self.tx, tenant, id, None).await?;
                if cur.get(type_name) == Some(&false) {
                    return Err(BaseError::conflict("type.already_attached", "type already attached").with("type", type_name.clone()));
                }
                let explicit: BTreeSet<String> = cur.iter().filter(|(_, imp)| !**imp).map(|(t, _)| t.clone()).chain([type_name.clone()]).collect();
                let closure = self.m.implied_closure_in(&explicit, &self.ctx.inactive);
                let old_tables = self.m.table_types(&cur.keys().cloned().collect());
                let new_tables = self.m.table_types(&closure);
                let given: BTreeMap<String, Map<String, Value>> = [(type_name.clone(), values.clone())].into();
                let vals = self.distribute(&given, &new_tables)?;
                self.check(create_art(type_name), type_name, &path)?;
                for t in vals.keys() {
                    self.check(create_art(t), t, &path)?;
                }
                for t in &closure {
                    let implied = !explicit.contains(t);
                    if cur.get(t) != Some(&implied) {
                        self.set_type_entry(id, t, implied, false).await?;
                    }
                }
                let empty = Map::new();
                for t in &new_tables {
                    let v = vals.get(t).unwrap_or(&empty);
                    if old_tables.contains(t) {
                        if !v.is_empty() {
                            self.version_type_row(id, t, v, None).await?;
                        }
                        continue;
                    }
                    let exists = self
                        .tx
                        .query_opt(&format!("SELECT 1 FROM {} WHERE tenant_id=$1 AND object_id=$2 AND valid_to IS NULL", type_table(self.m.require_type(t)?)), &[&tenant, &id])
                        .await
                        .map_err(db)?
                        .is_some();
                    if exists {
                        // re-attach after an earlier detach: values given now must be complete
                        self.version_type_row(id, t, v, Some(false)).await?;
                    } else {
                        self.insert_type_row(id, t, v).await?;
                    }
                }
                self.touched.insert(id);
            }
            Op::Detach { id, type_name } => {
                let id = self.resolve(id)?;
                let (_, path) = self.live(id).await?;
                let cur = types_of(self.tx, tenant, id, None).await?;
                match cur.get(type_name) {
                    Some(false) => {}
                    Some(true) => return Err(BaseError::invalid("type.implied", "implied types are detached with the type that implies them").with("type", type_name.clone())),
                    None => return Err(BaseError::invalid("type.not_attached", "type is not attached").with("type", type_name.clone())),
                }
                self.check_on(Art::Delete, type_name, &path, id).await?;
                let explicit: BTreeSet<String> = cur.iter().filter(|(t, imp)| !**imp && *t != type_name).map(|(t, _)| t.clone()).collect();
                if explicit.is_empty() {
                    return Err(BaseError::invalid("type.last", "the last type cannot be detached; delete the object instead"));
                }
                let closure = self.m.implied_closure_in(&explicit, &self.ctx.inactive);
                let old_tables = self.m.table_types(&cur.keys().cloned().collect());
                let new_tables = self.m.table_types(&closure);
                for t in cur.keys().filter(|t| !closure.contains(*t)) {
                    self.set_type_entry(id, t, cur[t], true).await?;
                }
                for t in old_tables.difference(&new_tables) {
                    self.version_type_row(id, t, &Map::new(), Some(true)).await?;
                }
                self.touched.insert(id);
                self.detached.insert(id);
                for n in self.neighbours(id).await? {
                    self.touched.insert(n);
                }
            }
            Op::Delete { id } => {
                let id = self.resolve(id)?;
                let (_, path) = self.live(id).await?;
                self.check(Art::Delete, ANY, &path)?;
                let mut todo = vec![id];
                while let Some(x) = todo.pop() {
                    self.version_state(x, None, true).await?;
                    self.touched.insert(x);
                    for n in self.neighbours(x).await? {
                        self.touched.insert(n);
                    }
                    todo.extend(self.parts_of(x).await?);
                }
            }
            Op::Move { id, room } => {
                let id = self.resolve(id)?;
                let target = self.resolve(room)?;
                let (_, from) = self.live(id).await?;
                let to = self.ctx.rooms.path(target)?;
                self.check(Art::Remove, ANY, &from)?;
                self.check(Art::Place, ANY, &to)?;
                let mut todo = vec![id];
                while let Some(x) = todo.pop() {
                    self.version_state(x, Some(target), false).await?;
                    todo.extend(self.parts_of(x).await?);
                }
                self.touched.insert(id);
            }
            Op::Link { reference, from, to, rank } => {
                let def = self.m.ref_def(reference).ok_or_else(|| BaseError::invalid("reference.unknown", "unknown reference").with("reference", reference.clone()))?.clone();
                let a = self.resolve(from)?;
                let b = self.resolve(to)?;
                let (_, pa) = self.live(a).await?;
                let (room_b, pb) = self.live(b).await?;
                let ta = types_of(self.tx, tenant, a, None).await?;
                let tb = types_of(self.tx, tenant, b, None).await?;
                if !self.m.set_is_a(ta.keys(), &def.holder.type_name) {
                    return Err(BaseError::invalid("reference.wrong_type", "holder lacks the required type").with("reference", reference.clone()).with("type", def.holder.type_name.clone()));
                }
                if !self.m.set_is_a(tb.keys(), &def.target.type_name) {
                    return Err(BaseError::invalid("reference.wrong_type", "target lacks the required type").with("reference", reference.clone()).with("type", def.target.type_name.clone()));
                }
                self.check_link_rights(&def, a, &pa, b, &tb, &pb).await?;
                let rid = Uuid::new_v4();
                self.tx
                    .execute(
                        &format!("INSERT INTO {} (tenant_id, id, a_id, b_id, rank, valid_from) VALUES ($1,$2,$3,$4,$5,$6)", ref_table(&def)),
                        &[&tenant, &rid, &a, &b, rank, &self.txn],
                    )
                    .await
                    .map_err(db)?;
                if def.name == "base:Room.parent" {
                    self.ctx.rooms.parent.insert(a, Some(b));
                    if self.ctx.rooms.path(a).map(|p| p.len() > 255).unwrap_or(true) {
                        return Err(BaseError::invalid("room.cycle", "rooms must form a tree"));
                    }
                }
                if def.part_of {
                    // parts live in the room of their whole
                    let (room_a, _) = self.live(a).await?;
                    if room_a != room_b {
                        self.version_state(a, Some(room_b), false).await?;
                    }
                }
                self.links.push(rid);
                self.touched.insert(a);
                self.touched.insert(b);
            }
            Op::Content { id, media_type, filename, base64 } => {
                use base64::Engine as _;
                let data = base64::engine::general_purpose::STANDARD
                    .decode(base64.as_bytes())
                    .map_err(|_| BaseError::invalid("content.bad_base64", "content is not valid base64"))?;
                let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&data));
                self.tx.execute("INSERT INTO blob (sha256, data) VALUES ($1,$2) ON CONFLICT DO NOTHING", &[&sha, &data]).await.map_err(db)?;
                let size = data.len() as i64;
                self.set_content(id, &sha, media_type, size, filename).await?;
            }
            Op::SetContent { id, sha256, media_type, size, filename } => {
                self.set_content(id, sha256, media_type, *size, filename).await?;
            }
            Op::Text { id, text_type, content } | Op::SetText { id, text_type, content } => {
                let id = self.resolve(id)?;
                let def = self.m.text_type(text_type).ok_or_else(|| BaseError::invalid("text.unknown_type", "unknown text type").with("textType", text_type.clone()))?.clone();
                let (_, path) = self.live(id).await?;
                let set = types_of(self.tx, tenant, id, None).await?;
                if !self.m.table_types(&set.keys().cloned().collect()).contains(&def.owner) {
                    return Err(BaseError::invalid("type.no_such_part", "the object has no type with this text").with("textType", text_type.clone()));
                }
                if !self.new_objects.contains(&id) {
                    self.check_on(Art::Write, &def.owner, &path, id).await?;
                }
                self.tx
                    .execute("UPDATE text_part SET valid_to=$4 WHERE tenant_id=$1 AND object_id=$2 AND text_type=$3 AND valid_to IS NULL", &[&tenant, &id, text_type, &self.txn])
                    .await
                    .map_err(db)?;
                self.tx
                    .execute(
                        "INSERT INTO text_part (tenant_id, object_id, text_type, content, media_type, valid_from) VALUES ($1,$2,$3,$4,$5,$6)",
                        &[&tenant, &id, text_type, content, &def.media_type, &self.txn],
                    )
                    .await
                    .map_err(db)?;
                self.touched.insert(id);
            }
            Op::Unlink { reference, id } => {
                let def = self.m.ref_def(reference).ok_or_else(|| BaseError::invalid("reference.unknown", "unknown reference").with("reference", reference.clone()))?.clone();
                let tb = ref_table(&def);
                let row = self
                    .tx
                    .query_opt(&format!("SELECT a_id, b_id FROM {tb} WHERE tenant_id=$1 AND id=$2 AND valid_to IS NULL"), &[&tenant, id])
                    .await
                    .map_err(db)?
                    .ok_or_else(|| BaseError::not_found("reference.not_found", "reference not found").with("id", id.to_string()))?;
                let (a, b): (Uuid, Uuid) = (row.get(0), row.get(1));
                let (_, pa) = self.live(a).await?;
                let art = if def.holder.type_name == GROUP_MEMBERSHIP { Art::Grant } else { Art::Write };
                self.check_on(art, &def.holder.type_name, &pa, a).await?;
                self.tx
                    .execute(&format!("UPDATE {tb} SET valid_to=$3 WHERE tenant_id=$1 AND id=$2 AND valid_to IS NULL"), &[&tenant, id, &self.txn])
                    .await
                    .map_err(db)?;
                if def.name == "base:Room.parent" {
                    self.ctx.rooms.parent.insert(a, None);
                }
                self.touched.insert(a);
                self.touched.insert(b);
            }
        }
        Ok(())
    }

    async fn check_link_rights(&self, def: &RefDef, a: Uuid, pa: &[Uuid], b: Uuid, tb: &BTreeMap<String, bool>, pb: &[Uuid]) -> Result<()> {
        // writing the reference: write right on the holding type part (create for new objects);
        // technical group memberships need the right to grant rights
        let holder = &def.holder.type_name;
        let art = if holder == GROUP_MEMBERSHIP {
            Art::Grant
        } else if self.new_objects.contains(&a) {
            Art::Create
        } else {
            Art::Write
        };
        let holder_ok = match art {
            Art::Write => self.allowed_on(Art::Write, holder, pa, a).await?,
            Art::Create => self.ctx.subject.may(art, holder, pa) || self.ctx.subject.may(Art::Write, holder, pa),
            _ => self.ctx.subject.may(art, holder, pa),
        };
        if !holder_ok {
            return Err(denied(art, holder));
        }
        // referencing needs read access to the target (rights paths count)
        let mut readable = false;
        for t in self.m.table_types(&tb.keys().cloned().collect()) {
            if (def.target.type_name == ANY || tb.contains_key(&t) || self.m.is_a(&t, &def.target.type_name)) && self.allowed_on(Art::Read, &t, pb, b).await? {
                readable = true;
                break;
            }
        }
        if !readable {
            return Err(denied(Art::Read, &def.target.type_name));
        }
        Ok(())
    }

    async fn count_end(&self, r: &RefDef, side: Side, id: Uuid) -> Result<i64> {
        let (this, other) = match side {
            Side::Holder => ("a_id", "b_id"),
            Side::Target => ("b_id", "a_id"),
        };
        let sql = format!(
            "SELECT count(*) FROM {} x JOIN object_state s ON s.tenant_id=x.tenant_id AND s.object_id=x.{other} AND s.valid_to IS NULL AND s.deleted_at IS NULL \
             WHERE x.tenant_id=$1 AND x.{this}=$2 AND x.valid_to IS NULL",
            ref_table(r)
        );
        Ok(self.tx.query_one(&sql, &[&self.tenant(), &id]).await.map_err(db)?.get(0))
    }

    pub(crate) async fn check_end_state(&mut self) -> Result<()> {
        let tenant = self.tenant();
        let mut violations: Vec<Value> = vec![];
        for id in self.touched.clone() {
            match state_of(self.tx, tenant, id, None).await? {
                Some((_, None)) => {}
                _ => continue, // deleted objects only matter for their neighbours
            }
            let set: BTreeSet<String> = types_of(self.tx, tenant, id, None).await?.into_keys().collect();
            for t in &set {
                let def = self.m.require_type(t)?;
                let req = &def.requires;
                if req.types.is_empty() {
                    continue;
                }
                let ok = match req.mode {
                    RequireMode::Any => req.types.iter().any(|x| self.m.set_is_a(&set, x)),
                    RequireMode::All => req.types.iter().all(|x| self.m.set_is_a(&set, x)),
                };
                if !ok {
                    violations.push(json!({"error_id": "type.precondition", "object": id, "type": t, "requires": req.types}));
                }
            }
            for (r, side) in self.m.ends_for(&set) {
                let end = match side {
                    Side::Holder => &r.holder,
                    Side::Target => &r.target,
                };
                if end.min == 0 && end.max.is_none() {
                    continue;
                }
                let n = self.count_end(r, side, id).await?;
                if n < end.min as i64 || end.max.map(|mx| n > mx as i64).unwrap_or(false) {
                    violations.push(json!({"error_id": "cardinality.violated", "object": id, "end": end.end, "reference": r.name, "count": n, "min": end.min, "max": end.max}));
                }
            }
            if self.detached.contains(&id) {
                // references that need a type the object no longer has
                for r in &self.m.catalog.references {
                    for (side, col_this, need) in [(Side::Holder, "a_id", &r.holder.type_name), (Side::Target, "b_id", &r.target.type_name)] {
                        if need == ANY || self.m.set_is_a(&set, need) {
                            continue;
                        }
                        let sql = format!("SELECT count(*) FROM {} WHERE tenant_id=$1 AND {col_this}=$2 AND valid_to IS NULL", ref_table(r));
                        let n: i64 = self.tx.query_one(&sql, &[&tenant, &id]).await.map_err(db)?.get(0);
                        if n > 0 {
                            let end = if side == Side::Holder { &r.holder.end } else { &r.target.end };
                            violations.push(json!({"error_id": "reference.type_lost", "object": id, "reference": r.name, "end": end, "count": n, "type": need}));
                        }
                    }
                }
            }
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(BaseError::conflict("integrity.violated", "the end state violates integrity rules").with("violations", Value::Array(violations)))
        }
    }
}
