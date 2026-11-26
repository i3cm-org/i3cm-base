//! Rights evaluation (pure). The store supplies carriers, rights and the room path.
//!
//! Order of evaluation:
//! 1. A matching block right of any carrier of `me` on the room path denies (guardians excepted).
//! 2. Per carrier the room path is walked from the root down. On each level a matching
//!    minus right denies, otherwise a matching plus right allows, otherwise the value from
//!    above stays. The deepest room that says something wins.
//! 3. Union over the carriers: allowed if one carrier allows. Otherwise denied.
//!
//! A right covers the art itself; `write` also covers `read`. A right on type `*` covers
//! every type. Rights apply to the attributes the named type defines (not its subtypes'
//! attributes); callers check each type part separately.

use crate::catalog::{Art, Sign, ANY};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffRight {
    pub sign: Sign,
    pub arts: Vec<Art>,
    pub types: Vec<String>,
    pub room: Uuid,
    pub role: String,
}

impl EffRight {
    pub fn covers(&self, art: Art, type_name: &str) -> bool {
        let art_ok = self.arts.contains(&art) || (art == Art::Read && self.arts.contains(&Art::Write));
        let type_ok = self.types.iter().any(|t| t == ANY || t == type_name);
        art_ok && type_ok
    }
}

/// Everything known about `me` for one request.
#[derive(Debug, Clone, Default)]
pub struct Subject {
    pub account: Uuid,
    /// The account itself and every group it belongs to, directly or via subgroups.
    pub carriers: Vec<Uuid>,
    /// Rights per carrier, roles already expanded with their includes.
    pub rights: HashMap<Uuid, Vec<EffRight>>,
    pub guardian: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allowed { carrier: Uuid, role: String, room: Uuid },
    Blocked { carrier: Uuid, role: String, room: Uuid },
    Denied,
}

impl Decision {
    pub fn allowed(&self) -> bool {
        matches!(self, Decision::Allowed { .. })
    }
}

impl Subject {
    /// Decide whether `me` may perform `art` on the type part `type_name` of an object whose
    /// room path (root first) is `path`. Use `*` as type for place/remove/delete of whole objects.
    pub fn decide(&self, art: Art, type_name: &str, path: &[Uuid]) -> Decision {
        if self.guardian {
            return Decision::Allowed { carrier: self.account, role: "guardian".into(), room: path.first().copied().unwrap_or_default() };
        }
        for c in &self.carriers {
            for r in self.rights.get(c).into_iter().flatten() {
                if r.sign == Sign::Block && path.contains(&r.room) && r.covers(art, type_name) {
                    return Decision::Blocked { carrier: *c, role: r.role.clone(), room: r.room };
                }
            }
        }
        for c in &self.carriers {
            let rights = match self.rights.get(c) {
                Some(r) => r,
                None => continue,
            };
            let mut value: Option<(bool, &EffRight)> = None;
            for room in path {
                let here: Vec<&EffRight> = rights.iter().filter(|r| r.room == *room && r.sign != Sign::Block && r.covers(art, type_name)).collect();
                if let Some(minus) = here.iter().find(|r| r.sign == Sign::Minus) {
                    value = Some((false, minus));
                } else if let Some(plus) = here.iter().find(|r| r.sign == Sign::Plus) {
                    value = Some((true, plus));
                }
            }
            if let Some((true, r)) = value {
                return Decision::Allowed { carrier: *c, role: r.role.clone(), room: r.room };
            }
        }
        Decision::Denied
    }

    pub fn may(&self, art: Art, type_name: &str, path: &[Uuid]) -> bool {
        self.decide(art, type_name, path).allowed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(sign: Sign, arts: &[Art], types: &[&str], room: Uuid) -> EffRight {
        EffRight { sign, arts: arts.to_vec(), types: types.iter().map(|s| s.to_string()).collect(), room, role: "r".into() }
    }

    struct Fx {
        root: Uuid,
        sub: Uuid,
        me: Uuid,
        a: Uuid,
        b: Uuid,
    }
    fn fx() -> Fx {
        Fx { root: Uuid::new_v4(), sub: Uuid::new_v4(), me: Uuid::new_v4(), a: Uuid::new_v4(), b: Uuid::new_v4() }
    }

    #[test]
    fn inherited_plus_and_minus_per_carrier() {
        let f = fx();
        let mut s = Subject { account: f.me, carriers: vec![f.me, f.a], ..Default::default() };
        s.rights.insert(f.a, vec![r(Sign::Plus, &[Art::Write], &["x:T"], f.root)]);
        let path = [f.root, f.sub];
        assert!(s.may(Art::Write, "x:T", &path));
        assert!(s.may(Art::Read, "x:T", &path), "write covers read");
        assert!(!s.may(Art::Write, "x:U", &path));
        s.rights.get_mut(&f.a).unwrap().push(r(Sign::Minus, &[Art::Write], &["x:T"], f.sub));
        assert!(!s.may(Art::Write, "x:T", &path), "minus in deeper room");
        assert!(s.may(Art::Write, "x:T", &[f.root]), "root unaffected");
        // a plus via another carrier is not cancelled by A's minus
        s.carriers.push(f.b);
        s.rights.insert(f.b, vec![r(Sign::Plus, &[Art::Write], &["*"], f.sub)]);
        assert!(s.may(Art::Write, "x:T", &path));
    }

    #[test]
    fn minus_before_plus_on_same_level() {
        let f = fx();
        let mut s = Subject { account: f.me, carriers: vec![f.a], ..Default::default() };
        s.rights.insert(f.a, vec![r(Sign::Plus, &[Art::Read], &["*"], f.root), r(Sign::Minus, &[Art::Read], &["x:T"], f.root)]);
        assert!(!s.may(Art::Read, "x:T", &[f.root]));
        assert!(s.may(Art::Read, "x:U", &[f.root]));
    }

    #[test]
    fn block_wins_over_all_carriers() {
        let f = fx();
        let mut s = Subject { account: f.me, carriers: vec![f.a, f.b], ..Default::default() };
        s.rights.insert(f.a, vec![r(Sign::Plus, &[Art::Read], &["*"], f.root)]);
        s.rights.insert(f.b, vec![r(Sign::Block, &[Art::Read], &["x:Debtor"], f.root)]);
        assert!(matches!(s.decide(Art::Read, "x:Debtor", &[f.root, f.sub]), Decision::Blocked { .. }));
        assert!(s.may(Art::Read, "x:Person", &[f.root]));
        s.guardian = true;
        assert!(s.may(Art::Read, "x:Debtor", &[f.root]));
    }

    #[test]
    fn default_is_no() {
        let f = fx();
        let s = Subject { account: f.me, carriers: vec![f.me], ..Default::default() };
        assert_eq!(s.decide(Art::Read, "x:T", &[f.root]), Decision::Denied);
    }
}
