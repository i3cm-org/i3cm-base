//! Per-request context: who is `me` (carriers, rights) and the room tree.

use crate::schema::{ref_table, type_table};
use crate::Store;
use i3cm_core::catalog::{Art, Sign};
use i3cm_core::rights::{EffRight, Subject};
use i3cm_core::{BaseError, Result};
use std::collections::{BTreeSet, HashMap};
use tokio_postgres::GenericClient;
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct RoomTree {
    pub root: Uuid,
    pub parent: HashMap<Uuid, Option<Uuid>>,
}

impl RoomTree {
    /// Path from the root down to `room`; error if the room is unknown.
    pub fn path(&self, room: Uuid) -> Result<Vec<Uuid>> {
        let mut out = vec![room];
        let mut cur = room;
        if !self.parent.contains_key(&room) {
            return Err(BaseError::invalid("room.unknown", "unknown room").with("room", room.to_string()));
        }
        while let Some(Some(p)) = self.parent.get(&cur) {
            if out.contains(p) || out.len() > 256 {
                break;
            }
            out.push(*p);
            cur = *p;
        }
        out.reverse();
        Ok(out)
    }
    pub fn rooms(&self) -> impl Iterator<Item = &Uuid> {
        self.parent.keys()
    }
}

#[derive(Debug, Clone)]
pub struct Ctx {
    pub tenant: Uuid,
    pub subject: Subject,
    pub rooms: RoomTree,
    /// Installed modules not active in this tenant: their types are invisible.
    pub inactive: std::collections::BTreeSet<String>,
    /// Development tenant (studio): set at creation, allowed by the operator.
    pub development: bool,
}

impl Ctx {
    pub fn me(&self) -> Uuid {
        self.subject.account
    }
}

pub(crate) fn db(e: tokio_postgres::Error) -> BaseError {
    if let Some(dbe) = e.as_db_error() {
        if dbe.code().code() == "23505" {
            return BaseError::conflict("value.not_unique", "value must be unique").with("constraint", dbe.constraint().unwrap_or("").to_string());
        }
        return BaseError::internal(format!("database: {}", dbe.message()));
    }
    BaseError::internal(format!("database: {e}"))
}

impl Store {
    pub(crate) async fn load_ctx<C: GenericClient>(&self, c: &C, tenant: Uuid, account: Uuid) -> Result<Ctx> {
        let m = &self.model;
        let member = ref_table(m.ref_def("base:GroupMembership.member").ok_or_else(|| BaseError::internal("catalog lacks base:GroupMembership"))?);
        let group = ref_table(m.ref_def("base:GroupMembership.group").unwrap());
        let sql = format!(
            "WITH RECURSIVE c(id) AS (SELECT $2::uuid UNION \
             SELECT g.b_id FROM c JOIN {member} mm ON mm.tenant_id=$1 AND mm.b_id=c.id AND mm.valid_to IS NULL \
             JOIN {group} g ON g.tenant_id=$1 AND g.a_id=mm.a_id AND g.valid_to IS NULL \
             JOIN object_state s ON s.tenant_id=$1 AND s.object_id=mm.a_id AND s.valid_to IS NULL AND s.deleted_at IS NULL) \
             SELECT id FROM c"
        );
        let carriers: Vec<Uuid> = c.query(&sql, &[&tenant, &account]).await.map_err(db)?.iter().map(|r| r.get(0)).collect();

        // profiles assigned to any carrier, expanded with includes
        let rows = c
            .query("SELECT carrier, profile FROM assignment WHERE tenant_id=$1 AND valid_to IS NULL AND carrier = ANY($2)", &[&tenant, &carriers])
            .await
            .map_err(db)?;
        let includes: Vec<(String, String)> = c
            .query("SELECT profile, includes FROM profile_include WHERE tenant_id=$1", &[&tenant])
            .await
            .map_err(db)?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        let mut profile_rights: HashMap<String, Vec<EffRight>> = HashMap::new();
        let mut guardians = BTreeSet::new();
        for r in c.query("SELECT name, guardian FROM profile WHERE tenant_id=$1", &[&tenant]).await.map_err(db)? {
            if r.get::<_, bool>(1) {
                guardians.insert(r.get::<_, String>(0));
            }
        }
        for r in c.query("SELECT profile, sign, arts, types, room_id, cond FROM profile_right WHERE tenant_id=$1 ORDER BY profile, idx", &[&tenant]).await.map_err(db)? {
            let profile: String = r.get(0);
            let sign = match r.get::<_, String>(1).as_str() {
                "+" => Sign::Plus,
                "-" => Sign::Minus,
                _ => Sign::Block,
            };
            let arts: Vec<Art> = r.get::<_, Vec<String>>(2).iter().filter_map(|a| Art::parse(a)).collect();
            // a right on a type applies to that type's part of every object carrying it, also
            // of objects of a subtype (their supertype part); never to the subtype's own part
            let types: Vec<String> = r.get(3);
            // named filters without namespace belong to the profile's module, also where the
            // right applies to a subtype of another module
            let cond: Option<String> = r.get::<_, Option<String>>(5).map(|c| i3cm_core::paths::qualify_named(&c, i3cm_core::Model::module_of(&profile)));
            profile_rights.entry(profile.clone()).or_default().push(EffRight { sign, arts, types, room: r.get(4), profile, cond });
        }
        let expand = |profile: &str| -> BTreeSet<String> {
            let mut set: BTreeSet<String> = [profile.to_string()].into();
            loop {
                let more: Vec<String> = includes.iter().filter(|(r, i)| set.contains(r) && !set.contains(i)).map(|(_, i)| i.clone()).collect();
                if more.is_empty() {
                    return set;
                }
                set.extend(more);
            }
        };
        let mut subject = Subject { account, carriers: carriers.clone(), ..Default::default() };
        for r in rows {
            let carrier: Uuid = r.get(0);
            let profile: String = r.get(1);
            for x in expand(&profile) {
                if guardians.contains(&x) {
                    subject.guardian = true;
                }
                if let Some(rs) = profile_rights.get(&x) {
                    subject.rights.entry(carrier).or_default().extend(rs.iter().cloned());
                }
            }
        }

        let rooms = self.load_rooms(c, tenant).await?;
        let inactive = self.inactive_modules(c, tenant).await?;
        let development: bool = c.query_opt("SELECT development FROM tenant WHERE id=$1", &[&tenant]).await.map_err(db)?.map(|r| r.get(0)).unwrap_or(false);
        Ok(Ctx { tenant, subject, rooms, inactive, development })
    }

    /// Modules of the catalog that are neither default nor activated in the tenant.
    pub(crate) async fn inactive_modules<C: GenericClient>(&self, c: &C, tenant: Uuid) -> Result<std::collections::BTreeSet<String>> {
        let active: std::collections::BTreeSet<String> =
            c.query("SELECT name FROM module_active WHERE tenant_id=$1", &[&tenant]).await.map_err(db)?.iter().map(|r| r.get(0)).collect();
        Ok(self.model.catalog.modules.iter().filter(|m| !self.model.is_default_module(&m.name) && !active.contains(&m.name)).map(|m| m.name.clone()).collect())
    }

    pub(crate) async fn load_rooms<C: GenericClient>(&self, c: &C, tenant: Uuid) -> Result<RoomTree> {
        let m = &self.model;
        let room_t = type_table(m.require_type("base:Room")?);
        let parent_t = ref_table(m.ref_def("base:Room.parent").ok_or_else(|| BaseError::internal("catalog lacks base:Room.parent"))?);
        let root: Option<Uuid> = c
            .query_one("SELECT root_room FROM tenant WHERE id=$1", &[&tenant])
            .await
            .map_err(db)?
            .get(0);
        let mut tree = RoomTree { root: root.unwrap_or_default(), parent: HashMap::new() };
        for r in c
            .query(&format!("SELECT object_id FROM {room_t} WHERE tenant_id=$1 AND valid_to IS NULL AND deleted_at IS NULL"), &[&tenant])
            .await
            .map_err(db)?
        {
            tree.parent.insert(r.get(0), None);
        }
        for r in c
            .query(&format!("SELECT a_id, b_id FROM {parent_t} WHERE tenant_id=$1 AND valid_to IS NULL"), &[&tenant])
            .await
            .map_err(db)?
        {
            tree.parent.insert(r.get(0), Some(r.get(1)));
        }
        Ok(tree)
    }
}
