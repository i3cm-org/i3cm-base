//! Reference paths for rights conditions (rights paths, DEC-045) and view filters (SPEC-004).
//!
//! A step goes from an object over one named reference end to the object at the other end.
//! Names are resolved statically against the catalog; the result is a chain of steps that
//! the store translates into joins over the reference tables.

use crate::catalog::{Model, ANY};
use crate::error::{BaseError, Result};
use crate::fhirpath::{self, Expr};

/// Most steps a rights path may take (DEC-045).
pub const MAX_RIGHTS_STEPS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Name of the reference (catalog `RefDef::name`).
    pub reference: String,
    /// The current object is the holder of the reference (the next one is its target).
    pub from_holder: bool,
    /// Type at the other end.
    pub to: String,
    pub rights_relevant: bool,
}

fn bad(code: &str, msg: &str, src: &str) -> BaseError {
    BaseError::invalid(code, msg.to_string()).with("expr", src)
}

/// Resolve the reference end `name` seen from an object of type `cur`.
pub fn step(m: &Model, cur: &str, name: &str) -> Result<Step> {
    let mut all = vec![];
    for r in &m.catalog.references {
        if r.holder.end == name {
            all.push((Step { reference: r.name.clone(), from_holder: true, to: r.target.type_name.clone(), rights_relevant: r.rights_relevant }, r.holder.type_name.clone()));
        }
        if r.target.end == name {
            all.push((Step { reference: r.name.clone(), from_holder: false, to: r.holder.type_name.clone(), rights_relevant: r.rights_relevant }, r.target.type_name.clone()));
        }
    }
    let related = |t: &str| t == ANY || cur == ANY || m.is_a(cur, t) || m.is_a(t, cur);
    let close: Vec<&(Step, String)> = all.iter().filter(|(_, at)| related(at)).collect();
    let pick: Vec<&(Step, String)> = if close.is_empty() { all.iter().collect() } else { close };
    match pick.len() {
        1 => Ok(pick[0].0.clone()),
        0 => Err(BaseError::invalid("path.unknown_step", "no reference end with this name").with("type", cur).with("name", name)),
        _ => Err(BaseError::invalid("path.ambiguous", "several reference ends with this name").with("type", cur).with("name", name)),
    }
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
/// steps, each over a reference marked `@rightsRelevant`.
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
        if !s.rights_relevant {
            return Err(bad("path.not_rights_relevant", "rights paths only follow references marked @rightsRelevant", src).with("name", n.clone()));
        }
        cur = s.to.clone();
        steps.push(s);
    }
    Ok(RightsPath { type_name: t.to_string(), steps })
}
