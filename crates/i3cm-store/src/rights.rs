//! Rights in SQL: rooms plus rights paths.
//!
//! Unconditional rights become a room filter. A rights path becomes an uncorrelated
//! subquery that starts at `me` (its carriers) and walks the references back to the objects
//! (`x.object_id IN (…)`), which the planner drives from `me`. Paged lists combine the room
//! part and each path as separate branches of a `UNION` (no `OR`); aggregates
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
        // types of modules not active in the tenant do not exist for it
        if ctx.inactive.contains(i3cm_core::Model::module_of(t)) {
            return Ok(a);
        }
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

/* ---------------------------------------------------------- view filters in SQL */

use crate::schema::col;
use crate::sql::param;
use i3cm_core::paths::{Filter, Operand};
use i3cm_core::values::{to_val, Col};
use i3cm_core::BaseError;

impl Store {
    /// SQL for a view filter on the entry object `x` of type `t`; `$1` is the tenant.
    pub fn filter_sql(&self, f: &Filter, t: &str, ctx: &Ctx, params: &mut Vec<Param>) -> Result<String> {
        let attr_def = |n: &str| self.model.path_types(t).into_iter().flat_map(|d| d.attributes.iter()).find(|a| a.name == n).cloned();
        Ok(match f {
            Filter::And(a, b) => format!("({} AND {})", self.filter_sql(a, t, ctx, params)?, self.filter_sql(b, t, ctx, params)?),
            Filter::Or(a, b) => format!("({} OR {})", self.filter_sql(a, t, ctx, params)?, self.filter_sql(b, t, ctx, params)?),
            Filter::Not(a) => format!("NOT ({})", self.filter_sql(a, t, ctx, params)?),
            Filter::RefMe(s) => {
                params.push(Box::new(ctx.subject.carriers.clone()));
                let (cur, next) = if s.from_holder { ("a_id", "b_id") } else { ("b_id", "a_id") };
                let tb = ref_table(self.model.ref_def(&s.reference).expect("checked reference"));
                format!("EXISTS (SELECT 1 FROM {tb} r WHERE r.tenant_id=x.tenant_id AND r.{cur}=x.object_id AND r.valid_to IS NULL AND r.{next} = ANY(${}))", params.len())
            }
            Filter::Exists(n, yes) => {
                if let Some(a) = attr_def(n) {
                    let owner = self.model.path_defining_type(t, &a.name).map(|d| d.name.clone()).unwrap_or_else(|| t.to_string());
                    let col_sql = self.attr_col(t, &owner, n);
                    format!("{col_sql} IS {}NULL", if *yes { "NOT " } else { "" })
                } else {
                    let s = i3cm_core::paths::step(&self.model, t, n)?;
                    let (cur, _) = if s.from_holder { ("a_id", "b_id") } else { ("b_id", "a_id") };
                    let tb = ref_table(self.model.ref_def(&s.reference).expect("checked reference"));
                    format!("{}EXISTS (SELECT 1 FROM {tb} r WHERE r.tenant_id=x.tenant_id AND r.{cur}=x.object_id AND r.valid_to IS NULL)", if *yes { "" } else { "NOT " })
                }
            }
            Filter::Cmp(a, op, b) => {
                // the attribute side gives the type for literal parameters
                let typed = [a, b].into_iter().find_map(|o| if let Operand::Attr(n) = o { attr_def(n) } else { None });
                let side = |o: &Operand, params: &mut Vec<Param>| -> Result<String> {
                    Ok(match o {
                        Operand::Attr(n) => {
                            let owner = self.model.path_defining_type(t, n).map(|d| d.name.clone()).unwrap_or_else(|| t.to_string());
                            self.attr_col(t, &owner, n)
                        }
                        Operand::Now => "now()".into(),
                        Operand::Today => "current_date".into(),
                        Operand::Shifted(base, op, n, unit) => {
                            let b = match base.as_ref() {
                                Operand::Now => "now()".to_string(),
                                Operand::Today => "current_date".to_string(),
                                Operand::Attr(n) => {
                                    let owner = self.model.path_defining_type(t, n).map(|d| d.name.clone()).unwrap_or_else(|| t.to_string());
                                    self.attr_col(t, &owner, n)
                                }
                                _ => return Err(BaseError::invalid("path.bad_filter", "a quantity is added to now(), today() or a date attribute")),
                            };
                            // numbers and units come from the parser (closed list), not from input text
                            format!("({b} {op} interval '{n} {unit}')")
                        }
                        Operand::Lit(v) => match &typed {
                            Some(ad) => {
                                let mut ad = ad.clone();
                                ad.required = false;
                                params.push(param(&to_val(&ad, v)?, Col::of(&ad.vt), false));
                                format!("${}", params.len())
                            }
                            None => return Err(BaseError::invalid("path.bad_filter", "a comparison needs an attribute of the entry type")),
                        },
                    })
                };
                let l = side(a, params)?;
                let r = side(b, params)?;
                format!("({l} {} {r})", if *op == "!=" { "<>" } else { op })
            }
        })
    }

    /// Column of attribute `n` (defined on `owner`) for the entry object `x` of type `t`: the
    /// entry table itself, or a subquery on the owner's table for inherited attributes.
    pub(crate) fn attr_col(&self, t: &str, owner: &str, n: &str) -> String {
        if owner == t {
            format!("x.{}", col(n))
        } else {
            let tb = crate::schema::type_table(self.model.require_type(owner).expect("checked type"));
            format!("(SELECT o.{} FROM {tb} o WHERE o.tenant_id=x.tenant_id AND o.object_id=x.object_id AND o.valid_to IS NULL)", col(n))
        }
    }
}
