//! Reference paths for rights conditions (rights paths) and view filters.
//!
//! A step goes from an object over one named reference end to the object at the other end.
//! Names are resolved statically against the catalog; the result is a chain of steps that
//! the store translates into joins over the reference tables.

use crate::catalog::{Model, ANY};
use crate::error::{BaseError, Result};
use crate::fhirpath::{self, Expr};

/// Most steps a rights path may take.
pub const MAX_RIGHTS_STEPS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Name of the reference (catalog `RefDef::name`).
    pub reference: String,
    /// The current object is the holder of the reference (the next one is its target).
    pub from_holder: bool,
    /// Type at the other end.
    pub to: String,
}

fn bad(code: &str, msg: &str, src: &str) -> BaseError {
    BaseError::invalid(code, msg.to_string()).with("expr", src)
}

/// Resolve the reference end `name` seen from an object of type `cur`.
pub fn step(m: &Model, cur: &str, name: &str) -> Result<Step> {
    let mut all = vec![];
    for r in &m.catalog.references {
        if r.holder.end == name {
            all.push((Step { reference: r.name.clone(), from_holder: true, to: r.target.type_name.clone()}, r.holder.type_name.clone()));
        }
        if r.target.end == name {
            all.push((Step { reference: r.name.clone(), from_holder: false, to: r.holder.type_name.clone()}, r.target.type_name.clone()));
        }
    }
    let pick: Vec<&(Step, String)> = all.iter().filter(|(_, at)| can_share(m, cur, at)).collect();
    match pick.len() {
        1 => Ok(pick[0].0.clone()),
        0 => Err(BaseError::invalid("path.unknown_step", "no reference end with this name").with("type", cur).with("name", name)),
        _ => Err(BaseError::invalid("path.ambiguous", "several reference ends with this name").with("type", cur).with("name", name)),
    }
}

/// Can one object carry both types? Yes if one is a subtype of the other, or one requires,
/// brings or imprints on the other (or on one of its supertypes). Names are only resolved
/// against such types, never against unrelated ones.
fn can_share(m: &Model, a: &str, b: &str) -> bool {
    if a == ANY || b == ANY || m.is_a(a, b) || m.is_a(b, a) {
        return true;
    }
    let links = |x: &str, y: &str| {
        m.ancestors(x).iter().any(|d| d.requires.types.iter().chain(&d.brings).chain(&d.imprints_on).any(|t| m.is_a(y, t) || m.is_a(t, y)))
    };
    links(a, b) || links(b, a)
}

/// `a.b.c` as names, or `None` if the expression is anything else.
pub fn chain(e: &Expr) -> Option<Vec<String>> {
    match e {
        Expr::Member(None, n) if !n.starts_with('%') => Some(vec![n.clone()]),
        Expr::Member(Some(t), n) if !n.starts_with('%') => {
            let mut v = chain(t)?;
            v.push(n.clone());
            Some(v)
        }
        _ => None,
    }
}

pub fn is_me(e: &Expr) -> bool {
    matches!(e, Expr::Member(None, n) if n == "%me")
}

/// A checked rights path: objects of `type_name` for which following `steps` reaches `me`
/// (the account or one of its groups).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RightsPath {
    pub type_name: String,
    pub steps: Vec<Step>,
}

/// Parse and check a rights-path condition for objects of type `t`: `<path> = %me`, 1 to 3
/// steps over any references. The references a path uses become rights-relevant (see
/// `Model::is_rights_relevant`).
pub fn rights_path(m: &Model, t: &str, src: &str) -> Result<RightsPath> {
    let e = fhirpath::parse(src)?;
    let path = match &e {
        Expr::Bin("=", a, b) if is_me(b) => chain(a),
        Expr::Bin("=", a, b) if is_me(a) => chain(b),
        _ => None,
    }
    .ok_or_else(|| bad("path.rights_form", "a rights path has the form `<reference path> = %me`", src))?;
    if path.len() > MAX_RIGHTS_STEPS {
        return Err(bad("path.too_long", "a rights path takes at most 3 steps", src).with("steps", path.len().to_string()));
    }
    let mut cur = t.to_string();
    let mut steps = vec![];
    for n in &path {
        let s = step(m, &cur, n).map_err(|e| e.with("expr", src))?;
        cur = s.to.clone();
        steps.push(s);
    }
    Ok(RightsPath { type_name: t.to_string(), steps })
}

/* ------------------------------------------------------------- filter language */

/// Most terms (comparisons, tests) a filter may have, named filters expanded.
pub const MAX_FILTER_TERMS: usize = 32;
/// Most reference joins all paths of a filter may need together.
pub const MAX_FILTER_JOINS: usize = 6;
/// Most levels of named filters referring to named filters.
const MAX_FILTER_DEPTH: usize = 4;

/// An attribute reached from the entry object: over `steps` (empty: the entry object itself)
/// to an object carrying `owner`, which defines attribute `name`. `optional` marks an attached
/// type (`@requires` on the entry type): the value is absent when it is not attached.
#[derive(Debug, Clone, PartialEq)]
pub struct AttrRef {
    pub steps: Vec<Step>,
    pub owner: String,
    pub name: String,
}

/// Operand of a comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Attr(AttrRef),
    /// Field of the object's content: `mediaType`, `size` or `sha256`.
    Content(&'static str),
    Lit(serde_json::Value),
    Now,
    Today,
    /// A date/time operand shifted by a quantity: `today() + 7 days` (op `+` or `-`, amount, unit).
    Shifted(Box<Operand>, &'static str, f64, String),
}

/// A condition in the filter language (rights `when`, aspect `where`, named filters, queries);
/// only what translates into SQL.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    Cmp(Operand, &'static str, Operand),
    /// Membership in a list of literals; counts as one term.
    In(Operand, Vec<serde_json::Value>),
    /// The path over reference ends reaches `me` (the account or one of its groups).
    RefMe(Vec<Step>),
    /// Attribute is set (`exists()`) or not (`empty()`); over a path: some object has it set.
    AttrExists(AttrRef, bool),
    /// The path over reference ends reaches some object (`exists()`) or none (`empty()`).
    RefExists(Vec<Step>, bool),
    /// The object has a text of this (qualified) text type.
    Text(String, bool),
    /// The object has content.
    HasContent(bool),
    And(Box<Filter>, Box<Filter>),
    Or(Box<Filter>, Box<Filter>),
    Not(Box<Filter>),
}

impl Filter {
    /// Number of terms (leaves).
    pub fn terms(&self) -> usize {
        match self {
            Filter::And(a, b) | Filter::Or(a, b) => a.terms() + b.terms(),
            Filter::Not(a) => a.terms(),
            _ => 1,
        }
    }
    /// Number of reference joins over all paths.
    pub fn joins(&self) -> usize {
        let op = |o: &Operand| if let Operand::Attr(a) = o { a.steps.len() } else { 0 };
        match self {
            Filter::And(a, b) | Filter::Or(a, b) => a.joins() + b.joins(),
            Filter::Not(a) => a.joins(),
            Filter::Cmp(a, _, b) => op(a) + op(b),
            Filter::In(a, _) => op(a),
            Filter::RefMe(s) | Filter::RefExists(s, _) => s.len(),
            Filter::AttrExists(a, _) => a.steps.len(),
            _ => 0,
        }
    }
    /// All steps of all paths (rights relevance).
    pub fn steps(&self) -> Vec<&Step> {
        fn op(o: &Operand) -> Vec<&Step> {
            if let Operand::Attr(a) = o { a.steps.iter().collect() } else { vec![] }
        }
        match self {
            Filter::And(a, b) | Filter::Or(a, b) => a.steps().into_iter().chain(b.steps()).collect(),
            Filter::Not(a) => a.steps(),
            Filter::Cmp(a, _, b) => op(a).into_iter().chain(op(b)).collect(),
            Filter::In(a, _) => op(a),
            Filter::RefMe(s) | Filter::RefExists(s, _) => s.iter().collect(),
            Filter::AttrExists(a, _) => a.steps.iter().collect(),
            _ => vec![],
        }
    }
    /// Does the filter refer to `me`?
    pub fn uses_me(&self) -> bool {
        match self {
            Filter::And(a, b) | Filter::Or(a, b) => a.uses_me() || b.uses_me(),
            Filter::Not(a) => a.uses_me(),
            Filter::RefMe(_) => true,
            _ => false,
        }
    }
}

/// Where a filter comes from: catalog definitions may use paths freely; a free query over the
/// API may not run paths over objects the caller might not read (only `%me` on one step).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Catalog,
    Query,
}

/// Parse and check a filter of the catalog (aspect `where`, named filter) for entry type `t`.
pub fn view_filter(m: &Model, t: &str, src: &str) -> Result<Filter> {
    filter(m, t, src, Origin::Catalog)
}

/// Parse and check a filter for entry type `t`.
pub fn filter(m: &Model, t: &str, src: &str, origin: Origin) -> Result<Filter> {
    let e = fhirpath::parse(src)?;
    let f = Lower { m, src, origin, ns: Model::module_of(t).to_string(), depth: 0 }.lower(t, &e)?;
    if f.terms() > MAX_FILTER_TERMS || f.joins() > MAX_FILTER_JOINS {
        return Err(bad("filter.too_complex", "the filter has too many terms or path joins", src)
            .with("terms", f.terms().to_string())
            .with("joins", f.joins().to_string()));
    }
    Ok(f)
}

/// A rights condition: a pure path to `%me` (evaluated as an uncorrelated subquery from `me`)
/// or any other filter.
#[derive(Debug, Clone, PartialEq)]
pub enum RightsCond {
    Path(RightsPath),
    Filter(Filter),
}

impl RightsCond {
    pub fn steps(&self) -> Vec<&Step> {
        match self {
            RightsCond::Path(p) => p.steps.iter().collect(),
            RightsCond::Filter(f) => f.steps(),
        }
    }
}

/// Parse and check the condition `when` of a right on type `t`.
pub fn rights_cond(m: &Model, t: &str, src: &str) -> Result<RightsCond> {
    if let Ok(rp) = rights_path(m, t, src) {
        return Ok(RightsCond::Path(rp));
    }
    Ok(RightsCond::Filter(filter(m, t, src, Origin::Catalog)?))
}

struct Lower<'a> {
    m: &'a Model,
    src: &'a str,
    origin: Origin,
    /// Module whose named filters `@name` refers to without namespace.
    ns: String,
    depth: usize,
}

const CONTENT_FIELDS: &[&str] = &["mediaType", "size", "sha256"];

impl Lower<'_> {
    fn bad(&self, code: &str, msg: &str) -> BaseError {
        bad(code, msg, self.src)
    }

    /// Walk reference ends from `t`; returns the steps and the type reached.
    fn walk(&self, t: &str, names: &[String]) -> Result<(Vec<Step>, String)> {
        if names.len() > MAX_RIGHTS_STEPS {
            return Err(self.bad("path.too_long", "a path takes at most 3 steps").with("steps", names.len().to_string()));
        }
        let mut cur = t.to_string();
        let mut steps = vec![];
        for n in names {
            let s = step(self.m, &cur, n).map_err(|e| e.with("expr", self.src))?;
            cur = s.to.clone();
            steps.push(s);
        }
        if self.origin == Origin::Query && !steps.is_empty() {
            return Err(self.bad("query.path_not_allowed", "a query may not follow references; use a named filter of a module"));
        }
        Ok((steps, cur))
    }

    /// Attribute `name` readable at type `t`: its own, inherited, guaranteed host, or of an
    /// attachable type (absent when not attached).
    fn attr_owner(&self, t: &str, name: &str) -> Result<Option<String>> {
        let scalar = |d: &crate::catalog::TypeDef| d.attributes.iter().any(|a| a.name == name && !a.list && crate::values::Col::of(&a.vt) != crate::values::Col::Json);
        if let Some(d) = self.m.path_types(t).into_iter().find(|d| scalar(d)) {
            return Ok(Some(d.name.clone()));
        }
        let att: Vec<_> = self.m.attachable_types(t).into_iter().filter(|d| scalar(d)).collect();
        match att.len() {
            0 => Ok(None),
            1 => Ok(Some(att[0].name.clone())),
            _ => Err(self.bad("path.ambiguous", "several attachable types have this attribute").with("name", name)),
        }
    }

    /// An attribute operand from a member chain `a.b.c`.
    fn attr(&self, t: &str, names: &[String]) -> Result<Option<AttrRef>> {
        let (last, path) = names.split_last().expect("non-empty chain");
        let (steps, at) = self.walk(t, path)?;
        Ok(self.attr_owner(&at, last)?.map(|owner| AttrRef { steps, owner, name: last.clone() }))
    }

    fn operand(&self, t: &str, e: &Expr) -> Result<Operand> {
        match e {
            Expr::Lit(v) => Ok(Operand::Lit(v.clone())),
            Expr::Call(None, f, a) if f == "now" && a.is_empty() => Ok(Operand::Now),
            Expr::Call(None, f, a) if f == "today" && a.is_empty() => Ok(Operand::Today),
            Expr::Bin(op @ ("+" | "-"), a, b) => match b.as_ref() {
                Expr::Lit(q) if q.get("quantity").is_some() => {
                    let base = self.operand(t, a)?;
                    if matches!(base, Operand::Lit(_)) {
                        return Err(self.bad("path.bad_filter", "a quantity is added to now(), today() or a date attribute"));
                    }
                    let n = q["quantity"].as_f64().unwrap_or(0.0);
                    Ok(Operand::Shifted(Box::new(base), op, n, q["unit"].as_str().unwrap_or("day").to_string()))
                }
                _ => Err(self.bad("path.bad_filter", "only a quantity such as `7 days` can be added or subtracted")),
            },
            _ => {
                let names = chain(e).ok_or_else(|| self.bad("path.bad_filter", "a filter compares attributes with literals, now(), today() or %me"))?;
                if names[0] == "_content" {
                    return match names.get(1).map(String::as_str) {
                        Some(f) if names.len() == 2 && CONTENT_FIELDS.contains(&f) => Ok(Operand::Content(CONTENT_FIELDS.iter().find(|x| **x == f).unwrap())),
                        _ => Err(self.bad("path.bad_filter", "content fields are mediaType, size and sha256")),
                    };
                }
                self.attr(t, &names)?
                    .map(Operand::Attr)
                    .ok_or_else(|| self.bad("path.unknown_attribute", "no attribute with this name").with("type", t).with("name", names.last().unwrap().clone()))
            }
        }
    }

    fn named(&self, t: &str, name: &str) -> Result<Filter> {
        let n = name.trim_start_matches('@');
        let def = self.m.named_filter(n, &self.ns).ok_or_else(|| self.bad("filter.unknown", "no named filter with this name").with("filter", n))?;
        if !self.m.is_a(t, &def.type_name) {
            return Err(self.bad("filter.wrong_type", "the named filter is defined on another type").with("filter", def.name.clone()).with("type", t));
        }
        if self.depth >= MAX_FILTER_DEPTH {
            return Err(self.bad("filter.too_deep", "named filters refer to each other too deeply"));
        }
        let e = fhirpath::parse(&def.filter)?;
        // inside a free query the same restriction holds for the named filter: no paths over
        // objects the caller might not read (the result could reveal them)
        Lower { m: self.m, src: &def.filter, origin: self.origin, ns: Model::module_of(&def.name).to_string(), depth: self.depth + 1 }.lower(t, &e)
    }

    fn lower(&self, t: &str, e: &Expr) -> Result<Filter> {
        match e {
            Expr::Member(None, n) if n.starts_with('@') => self.named(t, n),
            Expr::Bin("and", a, b) => Ok(Filter::And(Box::new(self.lower(t, a)?), Box::new(self.lower(t, b)?))),
            Expr::Bin("or", a, b) => Ok(Filter::Or(Box::new(self.lower(t, a)?), Box::new(self.lower(t, b)?))),
            Expr::Call(Some(x), f, a) if f == "not" && a.is_empty() => Ok(Filter::Not(Box::new(self.lower(t, x)?))),
            Expr::Call(None, f, a) if f == "not" && a.len() == 1 => Ok(Filter::Not(Box::new(self.lower(t, &a[0])?))),
            Expr::Call(Some(x), f, a) if (f == "exists" || f == "empty") && a.is_empty() => {
                let yes = f == "exists";
                let names = chain(x).ok_or_else(|| self.bad("path.bad_filter", "exists() and empty() apply to an attribute, a reference end, a text or the content"))?;
                if names.len() == 1 && names[0] == "_content" {
                    return Ok(Filter::HasContent(yes));
                }
                if names.len() == 2 && names[0] == "_texts" {
                    let tt = self.text_type(t, &names[1])?;
                    return Ok(Filter::Text(tt, yes));
                }
                if let Some(a) = self.attr(t, &names)? {
                    return Ok(Filter::AttrExists(a, yes));
                }
                let (steps, _) = self.walk(t, &names)?;
                Ok(Filter::RefExists(steps, yes))
            }
            Expr::Bin(op, a, b) if is_me(b) || is_me(a) => {
                let other = if is_me(b) { a } else { b };
                if *op != "=" {
                    return Err(self.bad("path.bad_filter", "%me is compared with `=` to a reference path"));
                }
                let names = chain(other).ok_or_else(|| self.bad("path.bad_filter", "%me is compared with a reference path"))?;
                if names.len() > MAX_RIGHTS_STEPS {
                    return Err(self.bad("path.too_long", "a path takes at most 3 steps").with("steps", names.len().to_string()));
                }
                // one step to %me is allowed in queries: it only reaches the caller's own carriers
                let mut cur = t.to_string();
                let mut steps = vec![];
                for n in &names {
                    let s = step(self.m, &cur, n).map_err(|e| e.with("expr", self.src))?;
                    cur = s.to.clone();
                    steps.push(s);
                }
                if self.origin == Origin::Query && steps.len() > 1 {
                    return Err(self.bad("query.path_not_allowed", "a query may follow one reference to %me; use a named filter of a module for longer paths"));
                }
                Ok(Filter::RefMe(steps))
            }
            Expr::Bin("in", a, b) => {
                let items = match b.as_ref() {
                    Expr::Call(None, f, items) if f == "list" => items,
                    _ => return Err(self.bad("path.bad_filter", "`in` takes a list of literals")),
                };
                let mut vals = vec![];
                for x in items {
                    match x {
                        Expr::Lit(v) => vals.push(v.clone()),
                        _ => return Err(self.bad("path.bad_filter", "`in` takes a list of literals")),
                    }
                }
                if vals.is_empty() {
                    return Err(self.bad("path.bad_filter", "`in` needs at least one value"));
                }
                let o = self.operand(t, a)?;
                if !matches!(o, Operand::Attr(_) | Operand::Content(_)) {
                    return Err(self.bad("path.bad_filter", "`in` tests an attribute or a content field"));
                }
                Ok(Filter::In(o, vals))
            }
            Expr::Bin(op, a, b) if ["=", "!=", "<", "<=", ">", ">="].contains(op) => {
                let l = self.operand(t, a)?;
                let r = self.operand(t, b)?;
                let paths = [&l, &r].iter().filter(|o| matches!(o, Operand::Attr(x) if !x.steps.is_empty())).count();
                if paths > 1 {
                    return Err(self.bad("path.bad_filter", "only one side of a comparison may follow references"));
                }
                if !matches!(l, Operand::Attr(_) | Operand::Content(_)) && !matches!(r, Operand::Attr(_) | Operand::Content(_)) {
                    return Err(self.bad("path.bad_filter", "a comparison needs an attribute or a content field"));
                }
                Ok(Filter::Cmp(l, op, r))
            }
            _ => Err(self.bad("path.bad_filter", "not a filter that translates into SQL")),
        }
    }

    /// Qualified name of the text type `name` (local or qualified) owned by `t` or a type it shares objects with.
    fn text_type(&self, t: &str, name: &str) -> Result<String> {
        let owners: Vec<String> = self.m.path_types(t).iter().map(|d| d.name.clone()).chain(self.m.attachable_types(t).iter().map(|d| d.name.clone())).collect();
        self.m
            .catalog
            .text_types
            .iter()
            .find(|tt| owners.contains(&tt.owner) && (tt.name == name || tt.name.split_once(':').map(|x| x.1) == Some(name)))
            .map(|tt| tt.name.clone())
            .ok_or_else(|| self.bad("path.unknown_text", "the type has no text type with this name").with("type", t).with("text", name))
    }
}

#[cfg(test)]
mod host_type_tests {
    use crate::Model;
    use serde_json::json;

    fn model() -> Model {
        let t = |name: &str, attrs: serde_json::Value, req: &[&str]| {
            let (ns, local) = name.split_once(':').unwrap();
            json!({"name": name, "ns": ns, "local": local, "abstract": false, "extends": null, "attributes": attrs,
                "requires": {"mode": "any", "types": req}, "imprintsOn": [], "brings": [], "content": null, "textTypes": []})
        };
        let a = |n: &str, vt: serde_json::Value| json!({"name": n, "vt": vt, "required": false, "list": false, "indexed": false, "unique": false});
        let cat = json!({"format": 1, "modules": [{"name": "m", "requires": [], "rooms": [], "profiles": [], "i18n": {}}],
            "types": [t("m:Task", json!([a("due", json!({"kind": "datetime"})), a("status", json!({"kind": "string"}))]), &[]),
                      t("m:Deadline", json!([a("direction", json!({"kind": "string"}))]), &["m:Task"])],
            "references": [], "textTypes": []});
        Model::from_json(&cat.to_string()).unwrap()
    }

    #[test]
    fn a_type_with_requires_sees_the_names_of_its_host() {
        let m = model();
        assert!(super::view_filter(&m, "m:Deadline", "due <= today() + 7 days and status != 'done'").is_ok());
        assert!(super::view_filter(&m, "m:Task", "direction = 'x'").is_ok(), "the host sees the attributes of an attachable type (absent when not attached)");
        assert_eq!(m.path_defining_type("m:Deadline", "due").map(|d| d.name.as_str()), Some("m:Task"));
    }
}
