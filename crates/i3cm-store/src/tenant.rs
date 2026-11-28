//! Tenant bootstrap and tokens.

use crate::context::{db, Ctx, RoomTree};
use crate::order::{Op, Order};
use crate::schema::type_table;
use crate::{Auth, Store};
use chrono::{DateTime, Duration, Utc};
use i3cm_core::catalog::{Model, Sign};
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
    /// Problems that did not stop the creation (e.g. a module sync to be repeated at server start).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn attrs(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

impl Store {
    /// Create a tenant: root room, module rooms, profiles from the catalog, group "admins" with the
    /// guardian profile and an admin person with login and token.
    pub async fn init_tenant(&self, name: &str, admin_login: &str) -> Result<TenantInit> {
        self.init_tenant_with(name, admin_login, false).await
    }

    /// Like `init_tenant`; `development` marks a development tenant (studio). Whether
    /// the instance allows development tenants is the operator's decision, checked by the caller.
    pub async fn init_tenant_with(&self, name: &str, admin_login: &str, development: bool) -> Result<TenantInit> {
        let tenant = Uuid::new_v4();
        let root = Uuid::new_v4();
        {
            let mut c = self.client().await?;
            let tx = c.transaction().await.map_err(db)?;
            tx.execute("INSERT INTO tenant (id, name, root_room, development) VALUES ($1,$2,$3,$4)", &[&tenant, &name, &root, &development])
                .await
                .map_err(|e| match e.as_db_error().map(|d| d.code().code().to_string()) {
                    Some(code) if code == "23505" => BaseError::conflict("tenant.exists", "tenant name already used").with("name", name),
                    _ => db(e),
                })?;
            let (txn, seq) = crate::session::clock(&*tx, tenant).await?;
            tx.execute("INSERT INTO txn (tenant_id, id, seq) VALUES ($1,$2,$3)", &[&tenant, &txn, &seq]).await.map_err(db)?;
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
            inactive: Default::default(),
            development,
        };
        let mut ops = vec![];
        for m in self.model.catalog.modules.iter().filter(|m| self.model.is_default_module(&m.name)) {
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

        let turn = self.write_turn(tenant).await;
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db)?;
        // the write turn first (a module sync of another container may already work on the tenant)
        let (txn, seq) = crate::session::clock(&*tx, tenant).await?;
        let room_of = |name: &str| -> Uuid { if name == "root" { root } else { created[&format!("room:{name}")] } };
        for m in self.model.catalog.modules.iter().filter(|m| self.model.is_default_module(&m.name)) {
            for r in &m.rooms {
                tx.execute("INSERT INTO module_room (tenant_id, name, room_id) VALUES ($1,$2,$3)", &[&tenant, r, &room_of(r)]).await.map_err(db)?;
            }
        }
        for profile in self.model.catalog.modules.iter().filter(|m| self.model.is_default_module(&m.name)).flat_map(|m| m.profiles.iter()) {
            tx.execute("INSERT INTO profile (tenant_id, name, guardian) VALUES ($1,$2,$3)", &[&tenant, &profile.name, &profile.guardian]).await.map_err(db)?;
            for (i, r) in profile.rights.iter().enumerate() {
                let sign = match r.sign {
                    Sign::Plus => "+",
                    Sign::Minus => "-",
                    Sign::Block => "block",
                };
                let arts: Vec<String> = r.art.iter().map(|a| a.as_str().to_string()).collect();
                tx.execute(
                    "INSERT INTO profile_right (tenant_id, profile, idx, sign, arts, types, room_id, cond) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
                    &[&tenant, &profile.name, &(i as i32), &sign, &arts, &r.types, &room_of(&r.room), &r.when],
                )
                .await
                .map_err(db)?;
            }
            for inc in &profile.includes {
                tx.execute("INSERT INTO profile_include (tenant_id, profile, includes) VALUES ($1,$2,$3)", &[&tenant, &profile.name, inc]).await.map_err(db)?;
            }
        }
        tx.execute("INSERT INTO txn (tenant_id, id, seq) VALUES ($1,$2,$3)", &[&tenant, &txn, &seq]).await.map_err(db)?;
        let admins = created["admins"];
        tx.execute(
            "INSERT INTO assignment (tenant_id, id, profile, carrier, valid_from) VALUES ($1,$2,'base:admin',$3,$4)",
            &[&tenant, &Uuid::new_v4(), &admins, &txn],
        )
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        drop(c);
        // the sync below takes the write turn itself
        drop(turn);
        let admin = created["admin"];
        // the first token is a start key: valid for one day
        // carriers (agent accounts) of the mandatory modules, and anything else their definition needs;
        // a failure here does not undo the tenant: the sync is repeated at every server start
        let mut warnings = vec![];
        match self.sync_modules(tenant).await {
            Ok(s) => warnings.extend(s.warnings),
            Err(e) => warnings.push(format!("module sync failed, repeated at the next server start: {} {}", e.error_id, Value::Object(e.params.clone()))),
        }
        let token = self.create_token_unchecked(tenant, admin, Some(INIT_TOKEN_TTL_DAYS)).await?.token;
        Ok(TenantInit { tenant, root_room: root, admin_account: admin, admin_group: admins, token, warnings })
    }

    /// Name of a tenant (`GET /v1/me` reports it so that tools can check where they write).
    pub async fn tenant_name(&self, tenant: Uuid) -> Result<String> {
        let c = self.client().await?;
        c.query_opt("SELECT name FROM tenant WHERE id=$1", &[&tenant])
            .await
            .map_err(db)?
            .map(|r| r.get(0))
            .ok_or_else(|| BaseError::not_found("tenant.not_found", "unknown tenant"))
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

    /// Issue a token without rights check (command line, bootstrap). `ttl_days`: default 90,
    /// at most 365; every token expires.
    pub async fn create_token_unchecked(&self, tenant: Uuid, account: Uuid, ttl_days: Option<i64>) -> Result<Issued> {
        let days = check_ttl(ttl_days)?;
        self.issue_token(tenant, account, Some("command line"), Some(Utc::now() + Duration::days(days))).await
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
        let days = check_ttl(ttl_days)?;
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
/// Lifetime of the first token of a new tenant (start key).
pub const INIT_TOKEN_TTL_DAYS: i64 = 1;
/// Longest lifetime of a token.
pub const TOKEN_TTL_MAX_DAYS: i64 = 365;

fn check_ttl(ttl_days: Option<i64>) -> Result<i64> {
    let days = ttl_days.unwrap_or(TOKEN_TTL_DAYS);
    if !(1..=TOKEN_TTL_MAX_DAYS).contains(&days) {
        return Err(BaseError::invalid("token.bad_ttl", "ttl must be between 1 and 365 days").with("ttl", days));
    }
    Ok(days)
}

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

impl Store {
    /// Whether a module is active in a tenant: a mandatory module (or `base`) always, any other
    /// module once activated. The same rule as the request context uses.
    pub async fn module_active(&self, tenant: Uuid, module: &str) -> Result<bool> {
        let c = self.client().await?;
        Ok(!self.inactive_modules(&**c, tenant).await?.contains(module))
    }
}

/// What an activation did; the rights it adds are listed so the tenant administrator sees them
/// (new rights are shown, red where they reach existing data).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Activation {
    pub module: String,
    pub txn: i64,
    pub rooms: Vec<String>,
    pub profiles: Vec<String>,
    pub carriers: Vec<String>,
    /// Per right of the module's profiles: `red` if it reaches types of other modules or the
    /// root room, else `green`.
    pub rights: Vec<serde_json::Value>,
    /// Objects that received imprinted types (catch-up).
    pub imprinted: i64,
}

impl Store {
    /// Activate an installed module in a tenant: dependencies must be active; rooms, profiles and
    /// carriers of the module are created; imprinting rules are applied to existing objects. All
    /// or nothing.
    pub async fn activate_module(&self, ctx: &Ctx, name: &str) -> Result<Activation> {
        if !ctx.subject.may(i3cm_core::Art::Grant, i3cm_core::ANY, &[ctx.rooms.root]) {
            return Err(BaseError::not_found("rights.denied", "not allowed or not found").with("art", "grant"));
        }
        let m = self.model.catalog.modules.iter().find(|m| m.name == name).ok_or_else(|| BaseError::not_found("module.not_installed", "module is not installed").with("module", name))?.clone();
        if !ctx.inactive.contains(name) {
            return Err(BaseError::conflict("module.active", "module is already active").with("module", name));
        }
        if m.development_only && !ctx.development {
            return Err(BaseError::invalid("module.development_only", "this module can only be activated in a development tenant").with("module", name));
        }
        for r in &m.requires {
            if ctx.inactive.contains(r) {
                return Err(BaseError::invalid("module.requires_inactive", "a required module is not active").with("module", name).with("requires", r.clone()));
            }
        }
        let tenant = ctx.tenant;
        let root = ctx.rooms.root;
        // guardian context with the module visible, for creating its rooms and carriers
        let mut gctx = ctx.clone();
        gctx.subject.guardian = true;
        gctx.inactive.remove(name);
        let mut sess = self.begin(&gctx, &crate::session::Audit { module: Some(name.into()), function: Some("activate".into()), tool: None }).await?;
        // checked again with the write turn held: a concurrent activation may have won
        let already = sess.client().query_opt("SELECT 1 FROM module_active WHERE tenant_id=$1 AND name=$2", &[&tenant, &name]).await.map_err(db)?.is_some();
        if already {
            sess.abort().await;
            return Err(BaseError::conflict("module.active", "module is already active").with("module", name));
        }
        let res = async {
            let mut created: BTreeMap<String, Uuid> = BTreeMap::new();
            for r in &m.rooms {
                let local = format!("room:{r}");
                let id = sess.apply(self, &mut gctx, &Op::Create { local: Some(local.clone()), room: None, types: [("base:Room".into(), attrs(json!({"name": r})))].into() }).await?;
                sess.apply(self, &mut gctx, &Op::Link { reference: "base:Room.parent".into(), from: local, to: root.to_string(), rank: None }).await?;
                created.insert(r.clone(), id.ok_or_else(|| BaseError::internal("room not created"))?);
            }
            let room_of = |n: &str| -> Uuid { if n == "root" { root } else { created.get(n).copied().unwrap_or(root) } };
            let c = sess.client();
            for r in &m.rooms {
                c.execute("INSERT INTO module_room (tenant_id, name, room_id) VALUES ($1,$2,$3)", &[&tenant, r, &room_of(r)]).await.map_err(db)?;
            }
            let mut rights = vec![];
            for profile in &m.profiles {
                c.execute("INSERT INTO profile (tenant_id, name, guardian) VALUES ($1,$2,$3)", &[&tenant, &profile.name, &profile.guardian]).await.map_err(db)?;
                for (i, r) in profile.rights.iter().enumerate() {
                    let sign = match r.sign {
                        Sign::Plus => "+",
                        Sign::Minus => "-",
                        Sign::Block => "block",
                    };
                    let arts: Vec<String> = r.art.iter().map(|a| a.as_str().to_string()).collect();
                    c.execute(
                        "INSERT INTO profile_right (tenant_id, profile, idx, sign, arts, types, room_id, cond) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
                        &[&tenant, &profile.name, &(i as i32), &sign, &arts, &r.types, &room_of(&r.room), &r.when],
                    )
                    .await
                    .map_err(db)?;
                    let foreign = r.room == "root" || r.types.iter().any(|t| t == "*" || Model::module_of(t) != name);
                    rights.push(json!({ "profile": profile.name, "art": arts, "types": r.types, "room": r.room, "color": if foreign && sign == "+" { "red" } else { "green" } }));
                }
                for inc in &profile.includes {
                    c.execute("INSERT INTO profile_include (tenant_id, profile, includes) VALUES ($1,$2,$3)", &[&tenant, &profile.name, inc]).await.map_err(db)?;
                }
            }
            // carriers: agent accounts of the module with their profiles
            let mut carriers = vec![];
            for cr in &m.carriers {
                let id = sess
                    .apply(self, &mut gctx, &Op::Create { local: Some(format!("carrier:{}", cr.name)), room: None, types: [("base:AgentAccount".into(), attrs(json!({"login": cr.name, "purpose": format!("module {name}")})))].into() })
                    .await?
                    .ok_or_else(|| BaseError::internal("carrier not created"))?;
                let txn = sess.txn;
                sess.client()
                    .execute("INSERT INTO module_carrier (tenant_id, module, name, account) VALUES ($1,$2,$3,$4)", &[&tenant, &name, &cr.name, &id])
                    .await
                    .map_err(db)?;
                for p in &cr.profiles {
                    sess.client()
                        .execute("INSERT INTO assignment (tenant_id, id, profile, carrier, valid_from) VALUES ($1,$2,$3,$4,$5)", &[&tenant, &Uuid::new_v4(), p, &id, &txn])
                        .await
                        .map_err(db)?;
                }
                carriers.push(cr.name.clone());
            }
            // catch-up of imprinting rules on existing objects
            let mut imprinted = 0i64;
            for t in self.model.catalog.types.iter().filter(|t| Model::module_of(&t.name) == name && !t.imprints_on.is_empty()) {
                if t.attributes.iter().any(|a| a.required) {
                    return Err(BaseError::invalid("module.catch_up_value", "an imprinted type with required attributes needs catch-up values").with("type", t.name.clone()));
                }
                let tb = type_table(t);
                let n = sess
                    .client()
                    .execute(
                        &format!(
                            "WITH hit AS (SELECT DISTINCT ot.object_id FROM object_type ot WHERE ot.tenant_id=$1 AND ot.valid_to IS NULL AND ot.deleted_at IS NULL AND ot.type_name = ANY($2) \
                             AND NOT EXISTS (SELECT 1 FROM object_type x WHERE x.tenant_id=$1 AND x.object_id=ot.object_id AND x.type_name=$3 AND x.valid_to IS NULL AND x.deleted_at IS NULL)), \
                             ins AS (INSERT INTO object_type (tenant_id, object_id, type_name, implied, valid_from) SELECT $1, object_id, $3, true, $4 FROM hit RETURNING object_id) \
                             INSERT INTO {tb} (tenant_id, object_id, valid_from) SELECT $1, object_id, $4 FROM ins"
                        ),
                        &[&tenant, &t.imprints_on.iter().flat_map(|x| self.model.subtypes_closure(x)).collect::<Vec<String>>(), &t.name, &sess.txn],
                    )
                    .await
                    .map_err(db)?;
                imprinted += n as i64;
            }
            sess.client().execute("INSERT INTO module_active (tenant_id, name, txn) VALUES ($1,$2,$3)", &[&tenant, &name, &sess.txn]).await.map_err(db)?;
            Ok::<_, BaseError>((m.rooms.clone(), m.profiles.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), carriers, rights, imprinted))
        }
        .await;
        match res {
            Ok((rooms, profiles, carriers, rights, imprinted)) => {
                let txn = sess.txn;
                sess.finish(self, &mut gctx, true).await?;
                Ok(Activation { module: name.into(), txn, rooms, profiles, carriers, rights, imprinted })
            }
            Err(e) => {
                sess.abort().await;
                Err(e)
            }
        }
    }
}

impl Store {
    /// The account of a carrier owned by a module in a tenant (created on activation); only these
    /// accounts can be impersonated.
    pub async fn module_carrier(&self, tenant: Uuid, module: &str, carrier: &str) -> Result<Uuid> {
        let c = self.client().await?;
        self.module_carrier_on(&**c, tenant, module, carrier).await
    }

    /// Like `module_carrier`, on a given connection (inside an open order: no second connection).
    pub async fn module_carrier_on<C: tokio_postgres::GenericClient>(&self, c: &C, tenant: Uuid, module: &str, carrier: &str) -> Result<Uuid> {
        c.query_opt("SELECT account FROM module_carrier WHERE tenant_id=$1 AND module=$2 AND name=$3", &[&tenant, &module, &carrier])
            .await
            .map_err(db)?
            .map(|r| r.get(0))
            .ok_or_else(|| BaseError::not_found("module.unknown_carrier", "the module has no such carrier in this tenant").with("module", module).with("carrier", carrier))
    }
}

/// Key of the advisory lock that serialises module syncs of several containers.
const SYNC_LOCK: i64 = 0x6933_636d_7379_6e63;

/// One row of `profile_right`: index, sign, arts, types, room, condition.
type RightRow = (i32, String, Vec<String>, Vec<String>, Uuid, Option<String>);

/// What `sync_modules` changed or noticed in one tenant.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ModuleSync {
    pub tenant: Uuid,
    /// Entries like `tasks: profile tasks:assignee created`.
    pub changes: Vec<String>,
    /// Entries like `tasks: profile tasks:old is no longer defined by the module (kept)`.
    pub warnings: Vec<String>,
}

impl Store {
    /// Bring every module that is active in the tenant (mandatory modules and activated ones) to
    /// the definition of the installed version: missing module rooms, profiles and carriers are
    /// created, the rights and includes of the module's profiles are set to the module's, missing
    /// profile assignments of carriers are added. Profiles of the module that the installed
    /// version no longer defines are kept and reported. Idempotent; each module in one transaction.
    pub async fn sync_modules(&self, tenant: Uuid) -> Result<ModuleSync> {
        let mut out = ModuleSync { tenant, ..Default::default() };
        let (rooms, inactive, development) = {
            let c = self.client().await?;
            let rooms = self.load_rooms(&**c, tenant).await?;
            let inactive = self.inactive_modules(&**c, tenant).await?;
            let development: bool = c.query_opt("SELECT development FROM tenant WHERE id=$1", &[&tenant]).await.map_err(db)?.map(|r| r.get(0)).unwrap_or(false);
            (rooms, inactive, development)
        };
        let root = rooms.root;
        let mods: Vec<_> = self.model.catalog.modules.iter().filter(|m| !inactive.contains(&m.name)).cloned().collect();
        for m in mods {
            let name = m.name.clone();
            let mut gctx = Ctx {
                tenant,
                subject: Subject { account: Uuid::nil(), carriers: vec![], rights: HashMap::new(), guardian: true },
                rooms: rooms.clone(),
                inactive: inactive.clone(),
                development,
            };
            let mut sess = self.begin(&gctx, &crate::session::Audit { module: Some(name.clone()), function: Some("sync".into()), tool: None }).await?;
            // what exists, read with the write turn held
            let (have_rooms, have_profiles, have_carriers) = {
                let c = sess.client();
                let have_rooms: HashMap<String, Uuid> =
                    c.query("SELECT name, room_id FROM module_room WHERE tenant_id=$1", &[&tenant]).await.map_err(db)?.iter().map(|r| (r.get(0), r.get(1))).collect();
                let have_profiles: Vec<String> = c
                    .query("SELECT name FROM profile WHERE tenant_id=$1 AND name LIKE $2", &[&tenant, &format!("{name}:%")])
                    .await
                    .map_err(db)?
                    .iter()
                    .map(|r| r.get(0))
                    .collect();
                let have_carriers: HashMap<String, Uuid> = c
                    .query("SELECT name, account FROM module_carrier WHERE tenant_id=$1 AND module=$2", &[&tenant, &name])
                    .await
                    .map_err(db)?
                    .iter()
                    .map(|r| (r.get(0), r.get(1)))
                    .collect();
                (have_rooms, have_profiles, have_carriers)
            };
            for p in &have_profiles {
                if !m.profiles.iter().any(|d| &d.name == p) {
                    out.warnings.push(format!("{name}: profile {p} is no longer defined by the module (kept)"));
                }
            }
            let mut warnings: Vec<String> = vec![];
            let res = async {
                let mut changes = vec![];
                let mut room_ids = have_rooms.clone();
                for r in m.rooms.iter().filter(|r| !have_rooms.contains_key(*r)) {
                    let local = format!("room:{r}");
                    let id = sess
                        .apply(self, &mut gctx, &Op::Create { local: Some(local.clone()), room: None, types: [("base:Room".into(), attrs(json!({"name": r})))].into() })
                        .await?
                        .ok_or_else(|| BaseError::internal("room not created"))?;
                    sess.apply(self, &mut gctx, &Op::Link { reference: "base:Room.parent".into(), from: local, to: root.to_string(), rank: None }).await?;
                    sess.client().execute("INSERT INTO module_room (tenant_id, name, room_id) VALUES ($1,$2,$3)", &[&tenant, r, &id]).await.map_err(db)?;
                    room_ids.insert(r.clone(), id);
                    changes.push(format!("{name}: room {r} created"));
                }
                let room_of = |n: &str| -> Uuid { if n == "root" { root } else { room_ids.get(n).copied().unwrap_or(root) } };
                for p in &m.profiles {
                    let c = sess.client();
                    let want: Vec<RightRow> = p
                        .rights
                        .iter()
                        .enumerate()
                        .map(|(i, r)| {
                            let sign = match r.sign {
                                Sign::Plus => "+",
                                Sign::Minus => "-",
                                Sign::Block => "block",
                            };
                            (i as i32, sign.to_string(), r.art.iter().map(|a| a.as_str().to_string()).collect(), r.types.clone(), room_of(&r.room), r.when.clone())
                        })
                        .collect();
                    let mut want_inc = p.includes.clone();
                    want_inc.sort();
                    if !have_profiles.contains(&p.name) {
                        c.execute("INSERT INTO profile (tenant_id, name, guardian) VALUES ($1,$2,$3)", &[&tenant, &p.name, &p.guardian]).await.map_err(db)?;
                        changes.push(format!("{name}: profile {} created", p.name));
                    } else {
                        let have: Vec<RightRow> = c
                            .query("SELECT idx, sign, arts, types, room_id, cond FROM profile_right WHERE tenant_id=$1 AND profile=$2 ORDER BY idx", &[&tenant, &p.name])
                            .await
                            .map_err(db)?
                            .iter()
                            .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5)))
                            .collect();
                        let mut have_inc: Vec<String> =
                            c.query("SELECT includes FROM profile_include WHERE tenant_id=$1 AND profile=$2", &[&tenant, &p.name]).await.map_err(db)?.iter().map(|r| r.get(0)).collect();
                        have_inc.sort();
                        let guardian: bool = c.query_one("SELECT guardian FROM profile WHERE tenant_id=$1 AND name=$2", &[&tenant, &p.name]).await.map_err(db)?.get(0);
                        if have == want && have_inc == want_inc && guardian == p.guardian {
                            continue;
                        }
                        c.execute("UPDATE profile SET guardian=$3 WHERE tenant_id=$1 AND name=$2", &[&tenant, &p.name, &p.guardian]).await.map_err(db)?;
                        c.execute("DELETE FROM profile_right WHERE tenant_id=$1 AND profile=$2", &[&tenant, &p.name]).await.map_err(db)?;
                        c.execute("DELETE FROM profile_include WHERE tenant_id=$1 AND profile=$2", &[&tenant, &p.name]).await.map_err(db)?;
                        changes.push(format!("{name}: profile {} updated", p.name));
                    }
                    for (i, sign, arts, types, room, cond) in &want {
                        c.execute(
                            "INSERT INTO profile_right (tenant_id, profile, idx, sign, arts, types, room_id, cond) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
                            &[&tenant, &p.name, i, sign, arts, types, room, cond],
                        )
                        .await
                        .map_err(db)?;
                    }
                    for inc in &want_inc {
                        c.execute("INSERT INTO profile_include (tenant_id, profile, includes) VALUES ($1,$2,$3)", &[&tenant, &p.name, inc]).await.map_err(db)?;
                    }
                }
                for cr in &m.carriers {
                    let owner: Option<String> = sess
                        .client()
                        .query_opt("SELECT module FROM module_carrier WHERE tenant_id=$1 AND name=$2", &[&tenant, &cr.name])
                        .await
                        .map_err(db)?
                        .map(|r| r.get(0));
                    if let Some(o) = owner.filter(|o| o != &name) {
                        warnings.push(format!("{name}: carrier {} belongs to module {o} (skipped)", cr.name));
                        continue;
                    }
                    let id = match have_carriers.get(&cr.name) {
                        Some(id) => *id,
                        None => {
                            let id = sess
                                .apply(self, &mut gctx, &Op::Create { local: Some(format!("carrier:{}", cr.name)), room: None, types: [("base:AgentAccount".into(), attrs(json!({"login": cr.name, "purpose": format!("module {name}")})))].into() })
                                .await?
                                .ok_or_else(|| BaseError::internal("carrier not created"))?;
                            sess.client()
                                .execute("INSERT INTO module_carrier (tenant_id, module, name, account) VALUES ($1,$2,$3,$4)", &[&tenant, &name, &cr.name, &id])
                                .await
                                .map_err(db)?;
                            changes.push(format!("{name}: carrier {} created", cr.name));
                            id
                        }
                    };
                    let txn = sess.txn;
                    for p in &cr.profiles {
                        let has: bool = sess
                            .client()
                            .query_one("SELECT EXISTS (SELECT 1 FROM assignment WHERE tenant_id=$1 AND carrier=$2 AND profile=$3 AND valid_to IS NULL)", &[&tenant, &id, p])
                            .await
                            .map_err(db)?
                            .get(0);
                        if !has {
                            sess.client()
                                .execute("INSERT INTO assignment (tenant_id, id, profile, carrier, valid_from) VALUES ($1,$2,$3,$4,$5)", &[&tenant, &Uuid::new_v4(), p, &id, &txn])
                                .await
                                .map_err(db)?;
                            changes.push(format!("{name}: carrier {} assigned {p}", cr.name));
                        }
                    }
                }
                Ok::<_, BaseError>(changes)
            }
            .await;
            out.warnings.append(&mut warnings);
            match res {
                Ok(changes) if changes.is_empty() => sess.abort().await,
                Ok(changes) => {
                    sess.finish(self, &mut gctx, true).await?;
                    out.changes.extend(changes);
                }
                Err(e) => {
                    sess.abort().await;
                    return Err(e.with("module", name));
                }
            }
        }
        Ok(out)
    }

    /// `sync_modules` for every tenant (server start). Several containers starting at once wait
    /// for each other (advisory lock), so the second finds nothing left to do.
    pub async fn sync_modules_all(&self) -> Result<Vec<ModuleSync>> {
        // the lock needs a connection of its own besides the ones the sync uses
        let lock = if self.pool.status().max_size >= 2 {
            let c = self.client().await?;
            c.execute("SELECT pg_advisory_lock($1)", &[&SYNC_LOCK]).await.map_err(db)?;
            Some(c)
        } else {
            None
        };
        let res = async {
            let mut out = vec![];
            for t in self.tenant_ids().await? {
                out.push(self.sync_modules(t).await?);
            }
            Ok(out)
        }
        .await;
        if let Some(c) = lock {
            let _ = c.execute("SELECT pg_advisory_unlock($1)", &[&SYNC_LOCK]).await;
        }
        res
    }
}
