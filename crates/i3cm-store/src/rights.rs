//! Rights in SQL: rooms plus rights paths (DEC-045, SPEC-004 section 3).
//!
//! Unconditional rights become a room filter. A rights path becomes an uncorrelated
//! subquery that starts at `me` (its carriers) and walks the references back to the objects
//! (`x.object_id IN (…)`), which the planner drives from `me`. Paged lists combine the room
//! part and each path as separate branches of a `UNION` (no `OR`, see REP-018); aggregates
//! use [`Store::rights_clause`], where the subqueries are evaluated once as hashed sets.

use crate::context::{db, Ctx};
use crate::schema::ref_table;
use crate::sql::Param;
use crate::Store;
use i3cm_core::catalog::Art;
use i3cm_core::paths::{rights_path, RightsPath};
use i3cm_core::Result;
use std::collections::BTreeMap;
use tokio_postgres::GenericClient;
use uuid::Uuid;

/// What `me` may see of a type: rooms without condition, and per rights path the rooms where
/// it applies.
#[derive(Debug, Clone, Default)]
pub struct Access {
    pub rooms: Vec<Uuid>,
    pub paths: Vec<(RightsPath, Vec<Uuid>)>,
}

impl Access {
    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty() && self.paths.is_empty()
    }
}

/// `SELECT <object id> FROM …` for the objects a rights path reaches from `me`. `$1` is the
/// tenant, `$me` the parameter number of the carrier array.
pub fn path_ids_sql(rp: &RightsPath, m: &i3cm_core::Model, me: usize) -> String {
    let n = rp.steps.len();
    let tab = |i: usize| ref_table(m.ref_def(&rp.steps[i].reference).expect("checked reference"));
    let ends = |i: usize| -> (&'static str, &'static str) { if rp.steps[i].from_holder { ("a_id", "b_id") } else { ("b_id", "a_id") } };
    let cur = |i: usize| ends(i).0;
    let next = |i: usize| ends(i).1;
    // r0 is the first step from the object; the last step must reach one of me's carriers
    let mut sql = format!("SELECT r0.{} FROM {} r0", cur(0), tab(0));
    for i in 1..n {
        sql.push_str(&format!(
            " JOIN {t} r{i} ON r{i}.tenant_id=r0.tenant_id AND r{i}.{c}=r{p}.{nx} AND r{i}.valid_to IS NULL",
            t = tab(i),
            c = cur(i),
            p = i - 1,
            nx = next(i - 1)
        ));
    }
    sql.push_str(&format!(" WHERE r0.tenant_id=$1 AND r0.valid_to IS NULL AND r{}.{} = ANY(${me})", n - 1, next(n - 1)));
    sql
}

impl Store {
    /// Rooms and rights paths under which `me` may perform `art` on type `t`.
    pub fn access(&self, ctx: &Ctx, art: Art, t: &str) -> Result<Access> {
        let mut a = Access::default();
        let mut by_cond: BTreeMap<String, Vec<Uuid>> = BTreeMap::new();
        for r in ctx.rooms.rooms() {
            let path = match ctx.rooms.path(*r) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if ctx.subject.may(art, t, &path) {
                a.rooms.push(*r);
            } else {
                for c in ctx.subject.conds(art, t, &path) {
                    by_cond.entry(c).or_default().push(*r);
                }
            }
        }
        for (c, rooms) in by_cond {
            a.paths.push((rights_path(&self.model, t, &c)?, rooms));
        }
        Ok(a)
    }

    /// One boolean SQL condition on `s.room_id` (object state) and `x.object_id` for aggregate
    /// queries. Pushes its parameters; `$1` must be the tenant.
    pub fn rights_clause(&self, a: &Access, ctx: &Ctx, params: &mut Vec<Param>) -> String {
        params.push(Box::new(a.rooms.clone()));
        let mut parts = vec![format!("s.room_id = ANY(${})", params.len())];
        if !a.paths.is_empty() {
            params.push(Box::new(ctx.subject.carriers.clone()));
            let me = params.len();
            for (rp, rooms) in &a.paths {
                params.push(Box::new(rooms.clone()));
                parts.push(format!("(s.room_id = ANY(${}) AND x.object_id IN ({}))", params.len(), path_ids_sql(rp, &self.model, me)));
            }
        }
        if parts.len() == 1 { parts.pop().unwrap() } else { format!("({})", parts.join(" OR ")) }
    }

    /// Branch conditions for a paged `UNION`: one per part of the access, each a condition on
    /// `s` and `x` without `OR`. Pushes the parameters.
    pub fn rights_branches(&self, a: &Access, ctx: &Ctx, params: &mut Vec<Param>) -> Vec<String> {
        let mut out = vec![];
        if !a.rooms.is_empty() {
            params.push(Box::new(a.rooms.clone()));
            out.push(format!("s.room_id = ANY(${})", params.len()));
        }
        if !a.paths.is_empty() {
            params.push(Box::new(ctx.subject.carriers.clone()));
            let me = params.len();
            for (rp, rooms) in &a.paths {
                params.push(Box::new(rooms.clone()));
                out.push(format!("s.room_id = ANY(${}) AND x.object_id IN ({})", params.len(), path_ids_sql(rp, &self.model, me)));
            }
        }
        out
    }

    /// May `me` perform `art` on type part `t` of object `id` in a room with this path?
    /// Unconditional rights first, then the rights paths for this object.
    pub async fn may_obj<C: GenericClient>(&self, c: &C, ctx: &Ctx, art: Art, t: &str, path: &[Uuid], id: Uuid) -> Result<bool> {
        if ctx.subject.may(art, t, path) {
            return Ok(true);
        }
        for cond in ctx.subject.conds(art, t, path) {
            let rp = rights_path(&self.model, t, &cond)?;
            let sql = format!("SELECT EXISTS (SELECT 1 WHERE $2::uuid IN ({}))", path_ids_sql(&rp, &self.model, 3));
            let hit: bool = c.query_one(&sql, &[&ctx.tenant, &id, &ctx.subject.carriers]).await.map_err(db)?.get(0);
            if hit {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
