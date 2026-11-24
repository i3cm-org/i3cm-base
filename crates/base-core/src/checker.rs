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
            if RESERVED.contains(&a.name.as_str()) || a.name.starts_with('_') {
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
            if e.starts_with('_') {
                return Err(err("reference.reserved_end", "end names starting with _ are reserved").with("reference", r.name.clone()).with("end", e.clone()));
            }
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
    for a in m.aspects() {
        check_aspect(m, a)?;
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

/// Names that every object has in paths (DEC: reserved names with underscore).
pub const PATH_RESERVED: &[&str] = &["_content", "_texts", "_id", "_room", "_createdBy", "_createdAt"];

fn check_path(m: &Model, aspect: &str, focus: Option<&str>, path: &str) -> Result<crate::fhirpath::Expr> {
    use crate::fhirpath::{check_profile, first_name, parse, VIEW_FUNCTIONS};
    let e = parse(path).map_err(|e| e.with("aspect", aspect))?;
    check_profile(&e, VIEW_FUNCTIONS, path).map_err(|e| e.with("aspect", aspect))?;
    if let (Some(t), Some(n)) = (focus, first_name(&e)) {
        let set: BTreeSet<String> = [t.to_string()].into();
        let known = PATH_RESERVED.contains(&n)
            || m.defining_type(t, n).is_some()
            || m.end_by_name(&set, n).is_some()
            || m.subtypes_closure(t).iter().any(|s| m.defining_type(s, n).is_some() || m.end_by_name(&[s.clone()].into(), n).is_some());
        if !known {
            return Err(err("aspect.unknown_name", "path starts with a name the type does not have")
                .with("aspect", aspect).with("type", t).with("name", n).with("path", path));
        }
    }
    Ok(e)
}

fn check_aspect(m: &Model, a: &crate::catalog::AspectDef) -> Result<()> {
    let entry = &a.entry.type_name;
    if m.type_def(entry).is_none() {
        return Err(err("aspect.unknown_type", "aspect names an unknown type").with("aspect", a.name.clone()).with("type", entry.clone()));
    }
    for g in &a.entry.group_by {
        check_path(m, &a.name, Some(entry), g)?;
    }
    for (t, n) in &a.nodes {
        if m.type_def(t).is_none() {
            return Err(err("aspect.unknown_type", "aspect names an unknown type").with("aspect", a.name.clone()).with("type", t.clone()));
        }
        for x in &n.attributes {
            check_path(m, &a.name, Some(t), &x.path)?;
        }
        for x in &n.sub_nodes {
            check_path(m, &a.name, Some(t), &x.path)?;
        }
        for x in &n.tabs {
            check_path(m, &a.name, Some(t), &x.path)?;
            for c in &x.columns {
                check_path(m, &a.name, None, &c.path)?;
            }
        }
        for f in &n.files {
            let ok = f.path == "_content" && m.ancestors(t).iter().any(|x| x.content.is_some())
                || f.path.strip_prefix("_texts.").map(|tt| m.text_type_for(&[t.clone()].into(), tt.trim_matches('`')).is_some()).unwrap_or(false);
            if !ok {
                return Err(err("aspect.bad_file", "a file must be _content or _texts.<text type> of the node type")
                    .with("aspect", a.name.clone()).with("type", t.clone()).with("file", f.name.clone()));
            }
        }
    }
    Ok(())
}
