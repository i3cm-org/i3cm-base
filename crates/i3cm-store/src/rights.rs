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
use i3cm_core::paths::{rights_cond_in, RightsCond, RightsPath};
use i3cm_core::Result;
use std::collections::BTreeMap;
use tokio_postgres::GenericClient;
use uuid::Uuid;

/// What `me` may see of a type: rooms without condition, and per rights path the rooms where
/// it applies.
#[derive(Debug, Clone, Default)]
pub struct Access {
    /// The type part the access is for (the table `x` of the queries).
    pub t: String,
    pub rooms: Vec<Uuid>,
    pub paths: Vec<(RightsCond, Vec<Uuid>)>,
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
        let mut a = Access { t: t.to_string(), ..Access::default() };
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
            a.paths.push((rights_cond_in(&self.model, t, &c, i3cm_core::Model::module_of(t), &ctx.inactive)?, rooms));
        }
        Ok(a)
    }

    /// SQL for one rights condition on the entry object `x` of type `t`. The carrier array is
    /// pushed once, when the first path condition needs it (`me` keeps its parameter number).
    fn cond_sql(&self, rc: &RightsCond, t: &str, me: &mut Option<usize>, ctx: &Ctx, params: &mut Vec<Param>) -> Result<String> {
        Ok(match rc {
            RightsCond::Path(rp) => {
                let n = match *me {
                    Some(n) => n,
                    None => {
                        params.push(Box::new(ctx.subject.carriers.clone()));
                        *me = Some(params.len());
                        params.len()
                    }
                };
                format!("x.object_id IN ({})", path_ids_sql(rp, &self.model, n))
            }
            RightsCond::Filter(f) => self.filter_sql(f, t, ctx, params)?,
        })
    }

    /// One boolean SQL condition on `s.room_id` (object state) and `x.object_id` for aggregate
    /// queries. Pushes its parameters; `$1` must be the tenant.
    pub fn rights_clause(&self, a: &Access, ctx: &Ctx, params: &mut Vec<Param>) -> Result<String> {
        let mut parts = vec![];
        if !a.rooms.is_empty() {
            params.push(Box::new(a.rooms.clone()));
            parts.push(format!("s.room_id = ANY(${})", params.len()));
        }
        let mut me = None;
        for (rc, rooms) in &a.paths {
            params.push(Box::new(rooms.clone()));
            let r = params.len();
            parts.push(format!("(s.room_id = ANY(${r}) AND {})", self.cond_sql(rc, &a.t, &mut me, ctx, params)?));
        }
        Ok(match parts.len() {
            0 => "false".to_string(),
            1 => parts.pop().unwrap(),
            _ => format!("({})", parts.join(" OR ")),
        })
    }

    /// Branch conditions for a paged `UNION`: one per part of the access, each a condition on
    /// `s` and `x` without `OR`. Pushes the parameters.
    pub fn rights_branches(&self, a: &Access, ctx: &Ctx, params: &mut Vec<Param>) -> Result<Vec<String>> {
        let mut out = vec![];
        if !a.rooms.is_empty() {
            params.push(Box::new(a.rooms.clone()));
            out.push(format!("s.room_id = ANY(${})", params.len()));
        }
        let mut me = None;
        for (rc, rooms) in &a.paths {
            params.push(Box::new(rooms.clone()));
            let r = params.len();
            out.push(format!("s.room_id = ANY(${r}) AND {}", self.cond_sql(rc, &a.t, &mut me, ctx, params)?));
        }
        Ok(out)
    }

    /// May `me` perform `art` on type part `t` of object `id` in a room with this path?
    /// Unconditional rights first, then the rights paths for this object.
    pub async fn may_obj<C: GenericClient>(&self, c: &C, ctx: &Ctx, art: Art, t: &str, path: &[Uuid], id: Uuid) -> Result<bool> {
        if ctx.subject.may(art, t, path) {
            return Ok(true);
        }
        for cond in ctx.subject.conds(art, t, path) {
            let hit: bool = match rights_cond_in(&self.model, t, &cond, i3cm_core::Model::module_of(t), &ctx.inactive)? {
                RightsCond::Path(rp) => {
                    let sql = format!("SELECT EXISTS (SELECT 1 WHERE $2::uuid IN ({}))", path_ids_sql(&rp, &self.model, 3));
                    c.query_one(&sql, &[&ctx.tenant, &id, &ctx.subject.carriers]).await.map_err(db)?.get(0)
                }
                RightsCond::Filter(f) => {
                    let mut params: Vec<Param> = vec![Box::new(ctx.tenant), Box::new(id)];
                    let cond_sql = self.filter_sql(&f, t, ctx, &mut params)?;
                    let tb = crate::schema::type_table(self.model.require_type(t)?);
                    let sql = format!("SELECT EXISTS (SELECT 1 FROM {tb} x WHERE x.tenant_id=$1 AND x.object_id=$2 AND x.valid_to IS NULL AND x.deleted_at IS NULL AND {cond_sql})");
                    c.query_one(&sql, &crate::sql::refs(&params)).await.map_err(db)?.get(0)
                }
            };
            if hit {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/* ---------------------------------------------------------- filters in SQL */

use crate::schema::{col, type_table};
use crate::sql::param;
use i3cm_core::paths::{AttrRef, Filter, Operand, Step};
use i3cm_core::values::{to_val, Col};
use i3cm_core::BaseError;

/// State of one filter translation: entry type, request, whether other type parts are guarded
/// by read rights (free queries), and the guards built so far.
pub(crate) struct FilterSql<'a> {
    pub t: &'a str,
    pub ctx: &'a Ctx,
    pub guard: bool,
    pub readable: BTreeMap<String, String>,
}

impl Store {
    /// Joins over the reference tables of `steps`, starting at the entry object `x`. Returns
    /// the `FROM … WHERE …` part and the column of the object reached.
    fn path_sql(&self, steps: &[Step]) -> (String, String) {
        // the parser never yields an empty path; an empty one would match nothing
        if steps.is_empty() {
            return ("FROM (SELECT 1) r0 WHERE false".to_string(), "NULL".to_string());
        }
        let ends = |s: &Step| -> (&'static str, &'static str) { if s.from_holder { ("a_id", "b_id") } else { ("b_id", "a_id") } };
        let tab = |s: &Step| ref_table(self.model.ref_def(&s.reference).expect("checked reference"));
        let mut from = format!("FROM {} r0", tab(&steps[0]));
        for (i, s) in steps.iter().enumerate().skip(1) {
            from.push_str(&format!(
                " JOIN {t} r{i} ON r{i}.tenant_id=r0.tenant_id AND r{i}.{c}=r{p}.{nx} AND r{i}.valid_to IS NULL",
                t = tab(s),
                c = ends(s).0,
                p = i - 1,
                nx = ends(&steps[i - 1]).1
            ));
        }
        let last = format!("r{}.{}", steps.len() - 1, ends(&steps[steps.len() - 1]).1);
        (format!("{from} WHERE r0.tenant_id=x.tenant_id AND r0.{}=x.object_id AND r0.valid_to IS NULL", ends(&steps[0]).0), last)
    }

    fn attr_def_of(&self, a: &AttrRef) -> Result<i3cm_core::catalog::AttrDef> {
        let d = self.model.require_type(&a.owner)?;
        d.attributes.iter().find(|x| x.name == a.name).cloned().ok_or_else(|| BaseError::invalid("path.unknown_attribute", "no attribute with this name").with("name", a.name.clone()))
    }

    /// Wrap `cond` (on alias `o`, the object reached over `a.steps`) as an existence test.
    fn over_path(&self, a: &AttrRef, cond: &str) -> Result<String> {
        let (from, last) = self.path_sql(&a.steps);
        let ot = type_table(self.model.require_type(&a.owner)?);
        Ok(format!("EXISTS (SELECT 1 {from} AND EXISTS (SELECT 1 FROM {ot} o WHERE o.tenant_id=x.tenant_id AND o.object_id={last} AND o.valid_to IS NULL AND o.deleted_at IS NULL AND {cond}))"))
    }

    fn content_sql(field: &str) -> String {
        let c = match field {
            "mediaType" => "media_type",
            "size" => "size",
            _ => "sha256",
        };
        format!("(SELECT cn.{c} FROM content cn WHERE cn.tenant_id=x.tenant_id AND cn.object_id=x.object_id AND cn.valid_to IS NULL)")
    }

    /// A literal as a parameter typed like the operand it is compared with.
    fn lit_param(&self, other: Option<&Operand>, v: &serde_json::Value, params: &mut Vec<Param>) -> Result<String> {
        match other {
            // a date compared with a shifted operand: as a point in time
            Some(Operand::Now | Operand::Today | Operand::Shifted(..)) => {
                let s = v.as_str().ok_or_else(|| BaseError::invalid("path.bad_filter", "a point in time is compared with a date or date-time"))?;
                params.push(Box::new(s.to_string()));
                return Ok(format!("${}::timestamptz", params.len()));
            }
            Some(Operand::Attr(a)) => {
                let mut ad = self.attr_def_of(a)?;
                ad.required = false;
                params.push(param(&to_val(&ad, v)?, Col::of(&ad.vt), false));
            }
            Some(Operand::Content("size")) => {
                let n = v.as_i64().ok_or_else(|| BaseError::invalid("path.bad_filter", "the content size is compared with a whole number"))?;
                params.push(Box::new(n));
            }
            Some(Operand::Content(_)) => {
                let s = v.as_str().ok_or_else(|| BaseError::invalid("path.bad_filter", "this content field is compared with a text"))?;
                params.push(Box::new(s.to_string()));
            }
            _ => return Err(BaseError::invalid("path.bad_filter", "a comparison needs an attribute or a content field")),
        }
        Ok(format!("${}", params.len()))
    }

    /// SQL of an operand; attributes over a path appear as `o.<col>` (the caller wraps them).
    fn operand_sql(&self, q: &mut FilterSql, o: &Operand, other: Option<&Operand>, params: &mut Vec<Param>) -> Result<String> {
        Ok(match o {
            Operand::Attr(a) if a.steps.is_empty() => self.entry_attr(q, a, params)?,
            Operand::Attr(a) => format!("o.{}", col(&a.name)),
            Operand::Content(f) => Self::content_sql(f),
            Operand::Now => "now()".into(),
            Operand::Today => "current_date".into(),
            Operand::Shifted(base, op, n, unit) => {
                let b = match base.as_ref() {
                    Operand::Now => "now()".to_string(),
                    Operand::Today => "current_date".to_string(),
                    Operand::Attr(a) if a.steps.is_empty() => self.entry_attr(q, a, params)?,
                    Operand::Attr(a) => format!("o.{}", col(&a.name)),
                    _ => return Err(BaseError::invalid("path.bad_filter", "a quantity is added to now(), today() or a date attribute")),
                };
                // numbers and units come from the parser (closed list), not from input text
                format!("({b} {op} interval '{n} {unit}')")
            }
            Operand::Lit(v) => self.lit_param(other, v, params)?,
        })
    }

    /// SQL for a filter of the catalog on the entry object `x` of type `t`; `$1` is the tenant.
    pub fn filter_sql(&self, f: &Filter, t: &str, ctx: &Ctx, params: &mut Vec<Param>) -> Result<String> {
        self.fsql(&mut FilterSql { t, ctx, guard: false, readable: BTreeMap::new() }, f, params)
    }

    /// SQL for a free query's filter on `x` of type `t`: values of other type parts of the
    /// object (supertypes, hosts, attached types, their texts) count only where the caller may
    /// read that part; elsewhere they are absent, so the result reveals nothing about them.
    pub fn query_filter_sql(&self, f: &Filter, t: &str, ctx: &Ctx, params: &mut Vec<Param>) -> Result<String> {
        self.fsql(&mut FilterSql { t, ctx, guard: true, readable: BTreeMap::new() }, f, params)
    }

    /// Condition: the caller may read type part `owner` of the entry object `x` (built once per
    /// filter and owner).
    pub(crate) fn readable_part(&self, q: &mut FilterSql, owner: &str, params: &mut Vec<Param>) -> Result<String> {
        if let Some(c) = q.readable.get(owner) {
            return Ok(c.clone());
        }
        let a = self.access(q.ctx, Art::Read, owner)?;
        let c = if a.is_empty() {
            "false".to_string()
        } else {
            let clause = self.rights_clause(&a, q.ctx, params)?;
            let tb = type_table(self.model.require_type(owner)?);
            format!(
                "x.object_id IN (SELECT x.object_id FROM {tb} x JOIN object_state s ON s.tenant_id=x.tenant_id AND s.object_id=x.object_id AND s.valid_to IS NULL AND s.deleted_at IS NULL \
                 WHERE x.tenant_id=$1 AND x.valid_to IS NULL AND x.deleted_at IS NULL AND {clause})"
            )
        };
        q.readable.insert(owner.to_string(), c.clone());
        Ok(c)
    }

    /// An attribute of the entry object (own, inherited, host or attached), guarded if asked.
    fn entry_attr(&self, q: &mut FilterSql, a: &AttrRef, params: &mut Vec<Param>) -> Result<String> {
        let v = self.attr_col(q.t, &a.owner, &a.name);
        if !q.guard || a.owner == q.t {
            return Ok(v);
        }
        let r = self.readable_part(q, &a.owner, params)?;
        Ok(format!("(CASE WHEN {r} THEN {v} END)"))
    }

    fn fsql(&self, q: &mut FilterSql, f: &Filter, params: &mut Vec<Param>) -> Result<String> {
        let path_attr = |o: &Operand| -> Option<AttrRef> { o.path_attr().cloned() };
        Ok(match f {
            Filter::And(a, b) => format!("({} AND {})", self.fsql(q, a, params)?, self.fsql(q, b, params)?),
            Filter::Or(a, b) => format!("({} OR {})", self.fsql(q, a, params)?, self.fsql(q, b, params)?),
            // two-valued: an absent value makes a comparison false, so its negation true
            Filter::Not(a) => format!("(({}) IS NOT TRUE)", self.fsql(q, a, params)?),
            Filter::RefMe(steps) => {
                params.push(Box::new(q.ctx.subject.carriers.clone()));
                let me = params.len();
                let (from, last) = self.path_sql(steps);
                format!("EXISTS (SELECT 1 {from} AND {last} = ANY(${me}))")
            }
            Filter::RefExists(steps, yes) => {
                let (from, _) = self.path_sql(steps);
                format!("{}EXISTS (SELECT 1 {from})", if *yes { "" } else { "NOT " })
            }
            Filter::AttrExists(a, yes) => {
                if a.steps.is_empty() {
                    format!("{} IS {}NULL", self.entry_attr(q, a, params)?, if *yes { "NOT " } else { "" })
                } else {
                    let e = self.over_path(a, &format!("o.{} IS NOT NULL", col(&a.name)))?;
                    if *yes { e } else { format!("NOT {e}") }
                }
            }
            Filter::Text(tt, yes) => {
                let owner = self.model.catalog.text_types.iter().find(|x| &x.name == tt).map(|x| x.owner.clone()).unwrap_or_default();
                let guard = if q.guard && owner != q.t { format!("{} AND ", self.readable_part(q, &owner, params)?) } else { String::new() };
                params.push(Box::new(tt.clone()));
                format!(
                    "{}({guard}EXISTS (SELECT 1 FROM text_part tp WHERE tp.tenant_id=x.tenant_id AND tp.object_id=x.object_id AND tp.text_type=${} AND tp.valid_to IS NULL))",
                    if *yes { "" } else { "NOT " },
                    params.len()
                )
            }
            Filter::HasContent(yes) => format!(
                "{}EXISTS (SELECT 1 FROM content cn WHERE cn.tenant_id=x.tenant_id AND cn.object_id=x.object_id AND cn.valid_to IS NULL)",
                if *yes { "" } else { "NOT " }
            ),
            Filter::In(o, vals) => {
                let lhs = self.operand_sql(q, o, None, params)?;
                let mut items = vec![];
                for v in vals {
                    items.push(self.lit_param(Some(o), v, params)?);
                }
                let cond = format!("{lhs} IN ({})", items.join(", "));
                match path_attr(o) {
                    Some(a) => self.over_path(&a, &cond)?,
                    None => format!("({cond})"),
                }
            }
            Filter::Cmp(a, op, b) => {
                let l = self.operand_sql(q, a, Some(b), params)?;
                let r = self.operand_sql(q, b, Some(a), params)?;
                let cond = format!("({l} {} {r})", if *op == "!=" { "<>" } else { op });
                match path_attr(a).or_else(|| path_attr(b)) {
                    Some(p) => self.over_path(&p, &cond)?,
                    None => cond,
                }
            }
        })
    }

    /// Column of attribute `n` (defined on `owner`) for the entry object `x` of type `t`: the
    /// entry table itself, or a subquery on the owner's table for inherited, host or attached
    /// attributes (absent, so NULL, when the owner type is not attached).
    pub(crate) fn attr_col(&self, t: &str, owner: &str, n: &str) -> String {
        if owner == t {
            format!("x.{}", col(n))
        } else {
            let tb = type_table(self.model.require_type(owner).expect("checked type"));
            format!("(SELECT o.{} FROM {tb} o WHERE o.tenant_id=x.tenant_id AND o.object_id=x.object_id AND o.valid_to IS NULL AND o.deleted_at IS NULL)", col(n))
        }
    }
}
