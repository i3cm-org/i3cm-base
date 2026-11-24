//! Type checker: validates a catalog before the core uses it.

use crate::catalog::{Model, ANY};
use crate::error::{BaseError, Result};
use std::collections::{BTreeSet, HashMap};

pub const RESERVED: &[&str] = &["tenant_id", "object_id", "valid_from", "valid_to", "deleted_at", "id"];

fn err(id: &str, d: &str) -> BaseError {
    BaseError::invalid(id, d)
}

pub fn check(m: &Model) -> Result<()> {
    let c = &m.catalog;
    let known = |t: &str| t == ANY || m.type_def(t).is_some();

    for t in &c.types {
        if let Some(p) = &t.extends {
            if m.type_def(p).is_none() {
                return Err(err("type.unknown_supertype", "unknown supertype").with("type", t.name.clone()).with("supertype", p.clone()));
            }
        }
        for x in t.requires.types.iter().chain(&t.imprints_on).chain(&t.brings) {
            if !known(x) {
                return Err(err("type.unknown_rule_type", "query rule names an unknown type").with("type", t.name.clone()).with("other", x.clone()));
            }
        }
        let mut seen = BTreeSet::new();
        for a in &t.attributes {
            if RESERVED.contains(&a.name.as_str()) {
                return Err(err("type.reserved_attribute", "attribute name is reserved").with("type", t.name.clone()).with("attribute", a.name.clone()));
            }
            if !seen.insert(a.name.clone()) {
                return Err(err("type.duplicate_attribute", "attribute defined twice").with("type", t.name.clone()).with("attribute", a.name.clone()));
            }
        }
        // inherited attribute names must not be redefined
        for anc in m.ancestors(&t.name).iter().skip(1) {
            for a in &t.attributes {
                if anc.attributes.iter().any(|x| x.name == a.name) {
                    return Err(err("type.redefined_attribute", "attribute already defined by a supertype")
                        .with("type", t.name.clone()).with("attribute", a.name.clone()).with("supertype", anc.name.clone()));
                }
            }
        }
    }

    // inheritance cycles
    for t in &c.types {
        let mut cur = t.extends.clone();
        let mut steps = 0;
        while let Some(p) = cur {
            if p == t.name || steps > c.types.len() {
                return Err(err("type.inheritance_cycle", "inheritance cycle").with("type", t.name.clone()));
            }
            cur = m.type_def(&p).and_then(|x| x.extends.clone());
            steps += 1;
        }
    }

    // implication cycles: edge A -> B when every A is also a B
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for t in &c.types {
        for b in &t.brings {
            edges.entry(t.name.clone()).or_default().push(b.clone());
        }
        for on in &t.imprints_on {
            edges.entry(on.clone()).or_default().push(t.name.clone());
        }
    }
    fn dfs(n: &str, e: &HashMap<String, Vec<String>>, stack: &mut Vec<String>, done: &mut BTreeSet<String>) -> Option<Vec<String>> {
        if let Some(pos) = stack.iter().position(|x| x == n) {
            return Some(stack[pos..].to_vec());
        }
        if done.contains(n) {
            return None;
        }
        stack.push(n.to_string());
        for nx in e.get(n).into_iter().flatten() {
            if let Some(c) = dfs(nx, e, stack, done) {
                return Some(c);
            }
        }
        stack.pop();
        done.insert(n.to_string());
        None
    }
    let mut done = BTreeSet::new();
    for t in &c.types {
        if let Some(cycle) = dfs(&t.name, &edges, &mut Vec::new(), &mut done) {
            return Err(err("type.implication_cycle", "query rules form a cycle").with("cycle", cycle.join(" -> ")));
        }
    }

    // references
    let mut ends: HashMap<(String, String), String> = HashMap::new();
    for r in &c.references {
        if m.type_def(&r.holder.type_name).is_none() {
            return Err(err("reference.unknown_type", "reference holder is unknown").with("reference", r.name.clone()));
        }
        if !known(&r.target.type_name) {
            return Err(err("reference.unknown_type", "reference target is unknown").with("reference", r.name.clone()).with("target", r.target.type_name.clone()));
        }
        for (t, e) in [(&r.holder.type_name, &r.holder.end), (&r.target.type_name, &r.target.end)] {
            if let Some(prev) = ends.insert((t.clone(), e.clone()), r.name.clone()) {
                return Err(err("reference.duplicate_end", "end name used twice on a type")
                    .with("type", t.clone()).with("end", e.clone()).with("references", format!("{prev}, {}", r.name)));
            }
            if t != ANY && m.ancestors(t).iter().any(|a| a.attributes.iter().any(|x| &x.name == e)) {
                return Err(err("reference.end_clashes_attribute", "end name equals an attribute name").with("type", t.clone()).with("end", e.clone()));
            }
        }
        if let Some(max) = r.target.max {
            if max < r.target.min {
                return Err(err("reference.bad_cardinality", "max below min").with("reference", r.name.clone()));
            }
        }
    }

    // roles
    let mut role_names = BTreeSet::new();
    let mut rooms: BTreeSet<String> = ["root".to_string()].into();
    for module in &c.modules {
        rooms.extend(module.rooms.iter().cloned());
    }
    for module in &c.modules {
        for r in &module.roles {
            if !role_names.insert(r.name.clone()) {
                return Err(err("role.duplicate", "role defined twice").with("role", r.name.clone()));
            }
            for right in &r.rights {
                for t in &right.types {
                    if !known(t) {
                        return Err(err("role.unknown_type", "role right names an unknown type").with("role", r.name.clone()).with("type", t.clone()));
                    }
                }
                if !rooms.contains(&right.room) {
                    return Err(err("role.unknown_room", "role right names an unknown room").with("role", r.name.clone()).with("room", right.room.clone()));
                }
            }
        }
        for lang in module.i18n.keys() {
            crate::i18n::parse_check(&module.name, lang, &module.i18n[lang])?;
        }
    }
    for r in m.roles() {
        for inc in &r.includes {
            if !role_names.contains(inc) {
                return Err(err("role.unknown_include", "included role is unknown").with("role", r.name.clone()).with("include", inc.clone()));
            }
        }
    }
    // include cycles
    let incl: HashMap<String, Vec<String>> = m.roles().map(|r| (r.name.clone(), r.includes.clone())).collect();
    let mut done = BTreeSet::new();
    for r in m.roles() {
        if let Some(cycle) = dfs(&r.name, &incl, &mut Vec::new(), &mut done) {
            return Err(err("role.include_cycle", "role includes form a cycle").with("cycle", cycle.join(" -> ")));
        }
    }
    Ok(())
}
