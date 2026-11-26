//! Tenant bootstrap and tokens.

use crate::context::{db, Ctx, RoomTree};
use crate::order::{Op, Order};
use crate::schema::type_table;
use crate::{Auth, Store};
use chrono::{DateTime, Duration, Utc};
use i3cm_core::catalog::Sign;
use i3cm_core::rights::Subject;
use i3cm_core::{BaseError, Result};
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

    /// Issue a token without rights check (command line, bootstrap); no expiry.
    pub async fn create_token_unchecked(&self, tenant: Uuid, account: Uuid) -> Result<String> {
        Ok(self.issue_token(tenant, account, None, None).await?.token)
    }

    /// Issue a token without rights check.
    pub async fn issue_token(&self, tenant: Uuid, account: Uuid, label: Option<&str>, expires_at: Option<DateTime<Utc>>) -> Result<Issued> {
        let token = new_token();
        let c = self.client().await?;
        let id: Uuid = c
            .query_one(
                "INSERT INTO token (hash, tenant_id, account, label, expires_at) VALUES ($1,$2,$3,$4,$5) RETURNING id",
                &[&hash_token(&token), &tenant, &account, &label, &expires_at],
            )
            .await
            .map_err(db)?
            .get(0);
        Ok(Issued { id, token, expires_at })
    }

    fn may_grant(ctx: &Ctx) -> bool {
        ctx.subject.may(i3cm_core::Art::Grant, i3cm_core::ANY, &[ctx.rooms.root])
    }

    /// Issue a token for an account (needs the right to grant rights).
    pub async fn create_token(&self, ctx: &Ctx, account: Uuid) -> Result<String> {
        let auth = Auth { tenant: ctx.tenant, account: ctx.me(), token: Uuid::nil(), expires_at: None };
        Ok(self.create_token_with(ctx, &auth, Some(account), None, None).await?.token)
    }

    /// Issue a token. Without `account` a caller issues one for itself (no right needed; it
    /// expires no later than the token of the request); for another account the right to grant
    /// rights is needed. `ttl_days`: default 90, at most 365.
    pub async fn create_token_with(&self, ctx: &Ctx, auth: &Auth, account: Option<Uuid>, label: Option<&str>, ttl_days: Option<i64>) -> Result<Issued> {
        let days = ttl_days.unwrap_or(TOKEN_TTL_DAYS);
        if !(1..=TOKEN_TTL_MAX_DAYS).contains(&days) {
            return Err(BaseError::invalid("token.bad_ttl", "ttl must be between 1 and 365 days").with("ttl", days));
        }
        let mut expires = Utc::now() + Duration::days(days);
        let account = match account {
            Some(a) if a != ctx.me() => {
                if !Self::may_grant(ctx) {
                    return Err(BaseError::not_found("rights.denied", "not allowed or not found").with("art", "grant"));
                }
                let c = self.client().await?;
                let set = crate::read::types_of(&**c, ctx.tenant, a, None).await?;
                if !self.model.set_is_a(set.keys(), "base:Account") {
                    return Err(BaseError::invalid("token.not_an_account", "tokens can only be issued for accounts"));
                }
                a
            }
            _ => {
                if let Some(limit) = auth.expires_at {
                    expires = expires.min(limit);
                }
                ctx.me()
            }
        };
        self.issue_token(ctx.tenant, account, label, Some(expires)).await
    }

    /// Tokens of the caller; with the right to grant rights all tokens of the tenant (optionally
    /// of one account). Secrets are never returned.
    pub async fn tokens(&self, ctx: &Ctx, account: Option<Uuid>) -> Result<Vec<TokenInfo>> {
        let all = Self::may_grant(ctx);
        let filter = match (all, account) {
            (true, a) => a,
            (false, _) => Some(ctx.me()),
        };
        let c = self.client().await?;
        let rows = c
            .query(
                "SELECT id, account, label, created_at, expires_at, revoked_at, last_used FROM token WHERE tenant_id=$1 AND ($2::uuid IS NULL OR account=$2) ORDER BY created_at, id",
                &[&ctx.tenant, &filter],
            )
            .await
            .map_err(db)?;
        Ok(rows
            .iter()
            .map(|r| TokenInfo { id: r.get(0), account: r.get(1), label: r.get(2), created_at: r.get(3), expires_at: r.get(4), revoked_at: r.get(5), last_used: r.get(6) })
            .collect())
    }

    /// Revoke a token: one's own, or any of the tenant with the right to grant rights.
    pub async fn revoke_token(&self, ctx: &Ctx, id: Uuid) -> Result<()> {
        let c = self.client().await?;
        let owner: Option<Uuid> = c.query_opt("SELECT account FROM token WHERE tenant_id=$1 AND id=$2", &[&ctx.tenant, &id]).await.map_err(db)?.map(|r| r.get(0));
        match owner {
            Some(a) if a == ctx.me() || Self::may_grant(ctx) => {
                c.execute("UPDATE token SET revoked_at=now() WHERE id=$1 AND revoked_at IS NULL", &[&id]).await.map_err(db)?;
                Ok(())
            }
            _ => Err(BaseError::not_found("token.not_found", "not allowed or not found").with("id", id.to_string())),
        }
    }
}

pub const TOKEN_TTL_DAYS: i64 = 90;
pub const TOKEN_TTL_MAX_DAYS: i64 = 365;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issued {
    pub id: Uuid,
    pub token: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    pub id: Uuid,
    pub account: Uuid,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used: Option<DateTime<Utc>>,
}
