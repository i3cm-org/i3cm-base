//! Tenant bootstrap and tokens.

use crate::context::{db, Ctx, RoomTree};
use crate::order::{Op, Order};
use crate::schema::type_table;
use crate::Store;
use base_core::catalog::Sign;
use base_core::rights::Subject;
use base_core::{BaseError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

pub fn hash_token(token: &str) -> String {
    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(token.as_bytes()))
}

fn new_token() -> String {
    format!("bt_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantInit {
    pub tenant: Uuid,
    pub root_room: Uuid,
    pub admin_account: Uuid,
    pub admin_group: Uuid,
    pub token: String,
}

fn attrs(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

impl Store {
    /// Create a tenant: root room, module rooms, roles from the catalog, group "admins" with the
    /// guardian role and an admin person with login and token.
    pub async fn init_tenant(&self, name: &str, admin_login: &str) -> Result<TenantInit> {
        let tenant = Uuid::new_v4();
        let root = Uuid::new_v4();
        {
            let mut c = self.client().await?;
            let tx = c.transaction().await.map_err(db)?;
            tx.execute("INSERT INTO tenant (id, name, root_room) VALUES ($1,$2,$3)", &[&tenant, &name, &root])
                .await
                .map_err(|e| match e.as_db_error().map(|d| d.code().code().to_string()) {
                    Some(code) if code == "23505" => BaseError::conflict("tenant.exists", "tenant name already used").with("name", name),
                    _ => db(e),
                })?;
            let txn: i64 = tx.query_one("SELECT nextval('txn_seq')", &[]).await.map_err(db)?.get(0);
            tx.execute("INSERT INTO txn (tenant_id, id) VALUES ($1,$2)", &[&tenant, &txn]).await.map_err(db)?;
            tx.execute("INSERT INTO object (tenant_id, id, created_txn) VALUES ($1,$2,$3)", &[&tenant, &root, &txn]).await.map_err(db)?;
            tx.execute("INSERT INTO object_state (tenant_id, object_id, room_id, valid_from) VALUES ($1,$2,$2,$3)", &[&tenant, &root, &txn]).await.map_err(db)?;
            tx.execute("INSERT INTO object_type (tenant_id, object_id, type_name, implied, valid_from) VALUES ($1,$2,'base:Room',false,$3)", &[&tenant, &root, &txn])
                .await
                .map_err(db)?;
            let rt = type_table(self.model.require_type("base:Room")?);
            tx.execute(&format!("INSERT INTO {rt} (tenant_id, object_id, valid_from, \"name\") VALUES ($1,$2,$3,'root')"), &[&tenant, &root, &txn])
                .await
                .map_err(db)?;
            tx.commit().await.map_err(db)?;
        }
        let mut ctx = Ctx {
            tenant,
            subject: Subject { account: Uuid::nil(), carriers: vec![], rights: HashMap::new(), guardian: true },
            rooms: RoomTree { root, parent: [(root, None)].into() },
        };
        let mut ops = vec![];
        for m in &self.model.catalog.modules {
            for r in &m.rooms {
                let local = format!("room:{r}");
                ops.push(Op::Create { local: Some(local.clone()), room: None, types: [("base:Room".into(), attrs(json!({"name": r})))].into() });
                ops.push(Op::Link { reference: "base:Room.parent".into(), from: local, to: root.to_string(), rank: None });
            }
        }
        ops.push(Op::Create { local: Some("admins".into()), room: None, types: [("base:Group".into(), attrs(json!({"name": "admins"})))].into() });
        ops.push(Op::Create {
            local: Some("admin".into()),
            room: None,
            types: [("base:Person".into(), attrs(json!({"lastName": admin_login}))), ("base:PersonAccount".into(), attrs(json!({"login": admin_login})))].into(),
        });
        ops.push(Op::Create { local: Some("m".into()), room: None, types: [("base:GroupMembership".into(), Map::new())].into() });
        ops.push(Op::Link { reference: "base:GroupMembership.group".into(), from: "m".into(), to: "admins".into(), rank: None });
        ops.push(Op::Link { reference: "base:GroupMembership.member".into(), from: "m".into(), to: "admin".into(), rank: None });
        let res = self.execute(&mut ctx, &Order { dry_run: false, ops }).await?;
        let created: BTreeMap<String, Uuid> = res.created;

        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db)?;
        let room_of = |name: &str| -> Uuid { if name == "root" { root } else { created[&format!("room:{name}")] } };
        for m in &self.model.catalog.modules {
            for r in &m.rooms {
                tx.execute("INSERT INTO module_room (tenant_id, name, room_id) VALUES ($1,$2,$3)", &[&tenant, r, &room_of(r)]).await.map_err(db)?;
            }
        }
        for role in self.model.roles() {
            tx.execute("INSERT INTO role (tenant_id, name, guardian) VALUES ($1,$2,$3)", &[&tenant, &role.name, &role.guardian]).await.map_err(db)?;
            for (i, r) in role.rights.iter().enumerate() {
                let sign = match r.sign {
                    Sign::Plus => "+",
                    Sign::Minus => "-",
                    Sign::Block => "block",
                };
                let arts: Vec<String> = r.art.iter().map(|a| a.as_str().to_string()).collect();
                tx.execute(
                    "INSERT INTO role_right (tenant_id, role, idx, sign, arts, types, room_id) VALUES ($1,$2,$3,$4,$5,$6,$7)",
                    &[&tenant, &role.name, &(i as i32), &sign, &arts, &r.types, &room_of(&r.room)],
                )
                .await
                .map_err(db)?;
            }
            for inc in &role.includes {
                tx.execute("INSERT INTO role_include (tenant_id, role, includes) VALUES ($1,$2,$3)", &[&tenant, &role.name, inc]).await.map_err(db)?;
            }
        }
        let txn: i64 = tx.query_one("SELECT nextval('txn_seq')", &[]).await.map_err(db)?.get(0);
        tx.execute("INSERT INTO txn (tenant_id, id) VALUES ($1,$2)", &[&tenant, &txn]).await.map_err(db)?;
        let admins = created["admins"];
        tx.execute(
            "INSERT INTO assignment (tenant_id, id, role, carrier, valid_from) VALUES ($1,$2,'base:admin',$3,$4)",
            &[&tenant, &Uuid::new_v4(), &admins, &txn],
        )
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        let admin = created["admin"];
        let token = self.create_token_unchecked(tenant, admin).await?;
        Ok(TenantInit { tenant, root_room: root, admin_account: admin, admin_group: admins, token })
    }

    pub async fn tenant_by_name(&self, name: &str) -> Result<Uuid> {
        let c = self.client().await?;
        c.query_opt("SELECT id FROM tenant WHERE name=$1", &[&name])
            .await
            .map_err(db)?
            .map(|r| r.get(0))
            .ok_or_else(|| BaseError::not_found("tenant.not_found", "unknown tenant").with("name", name))
    }

    pub async fn account_by_login(&self, tenant: Uuid, login: &str) -> Result<Uuid> {
        let c = self.client().await?;
        let t = type_table(self.model.require_type("base:Account")?);
        c.query_opt(&format!("SELECT object_id FROM {t} WHERE tenant_id=$1 AND \"login\"=$2 AND valid_to IS NULL AND deleted_at IS NULL"), &[&tenant, &login])
            .await
            .map_err(db)?
            .map(|r| r.get(0))
            .ok_or_else(|| BaseError::not_found("account.not_found", "unknown login").with("login", login))
    }

    /// Issue a token without rights check (command line, bootstrap).
    pub async fn create_token_unchecked(&self, tenant: Uuid, account: Uuid) -> Result<String> {
        let token = new_token();
        let c = self.client().await?;
        c.execute("INSERT INTO token (hash, tenant_id, account) VALUES ($1,$2,$3)", &[&hash_token(&token), &tenant, &account]).await.map_err(db)?;
        Ok(token)
    }

    /// Issue a token for an account (needs the right to grant rights).
    pub async fn create_token(&self, ctx: &Ctx, account: Uuid) -> Result<String> {
        if !ctx.subject.may(base_core::Art::Grant, base_core::ANY, &[ctx.rooms.root]) {
            return Err(BaseError::not_found("rights.denied", "not allowed or not found").with("art", "grant"));
        }
        let c = self.client().await?;
        let set = crate::read::types_of(&**c, ctx.tenant, account, None).await?;
        if !self.model.set_is_a(set.keys(), "base:Account") {
            return Err(BaseError::invalid("token.not_an_account", "tokens can only be issued for accounts"));
        }
        drop(c);
        self.create_token_unchecked(ctx.tenant, account).await
    }
}
