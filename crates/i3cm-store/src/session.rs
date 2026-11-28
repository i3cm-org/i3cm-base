//! An open order: one database transaction that stays open across several steps, used by
//! plain orders and by module code (one transaction per order, writes go straight in,
//! any error aborts the whole order).

use crate::context::{db, Ctx};
use crate::order::{Exec, Op, OrderResult};
use crate::Store;
use i3cm_core::Result;
use std::collections::{BTreeSet, HashMap};
use uuid::Uuid;

/// Who acts in an order beyond the account: module, function and tool.
#[derive(Debug, Clone, Default)]
pub struct Audit {
    pub module: Option<String>,
    pub function: Option<String>,
    pub tool: Option<String>,
}

#[derive(Default)]
struct State {
    local: HashMap<String, Uuid>,
    new_objects: BTreeSet<Uuid>,
    touched: BTreeSet<Uuid>,
    detached: BTreeSet<Uuid>,
    links: Vec<Uuid>,
    /// Whether the tenant had an administrator before the first operation that could remove one
    /// (set lazily; `None` while no such operation ran).
    admin_before: Option<bool>,
    warnings: Vec<serde_json::Value>,
    promote: Vec<(String, String)>,
}

pub struct Session {
    client: Option<deadpool_postgres::Object>,
    pub txn: i64,
    state: State,
    seq: i32,
    /// Largest base64 content in this order (orders of clients; `None` for module code).
    inline_limit: Option<usize>,
    /// The tenant's write turn in this process, released when the session ends.
    _turn: tokio::sync::OwnedMutexGuard<()>,
}

/// Start the writing part of a transaction in a tenant: wait until no other writing transaction of
/// the tenant is open, then allocate the transaction number and the tenant's commit sequence.
///
/// Writes of one tenant are serialised by the row lock on `tenant_clock`, held until commit or
/// rollback. Transaction numbers are therefore increasing in commit order within a tenant, so a
/// version is never closed by an older transaction than the one that opened it, and `seq` counts
/// the committed writing transactions of the tenant without gaps (a rollback undoes the step).
/// Call this before taking any other lock of the transaction (lock order: clock, then admins).
/// Seconds a writing transaction waits for the write turn of its tenant held in another container.
pub const WRITE_WAIT_SECS: u64 = 20;

pub(crate) async fn clock<C: tokio_postgres::GenericClient>(c: &C, tenant: Uuid) -> Result<(i64, i64)> {
    // writers of other containers queue here; give up instead of waiting forever
    c.batch_execute(&format!("SET LOCAL lock_timeout = '{}s'", WRITE_WAIT_SECS)).await.map_err(db)?;
    let seq: i64 = c
        .query_one(
            "INSERT INTO tenant_clock (tenant_id, seq) VALUES ($1, 1) ON CONFLICT (tenant_id) DO UPDATE SET seq = tenant_clock.seq + 1 RETURNING seq",
            &[&tenant],
        )
        .await
        .map_err(|e| {
            if e.as_db_error().map(|d| d.code().code() == "55P03").unwrap_or(false) {
                i3cm_core::BaseError::conflict("tenant.busy", "another writing order of this tenant is still running")
                    .with("waitedSeconds", WRITE_WAIT_SECS.to_string())
                    .hint("retry the request (with the same Idempotency-Key)")
            } else {
                db(e)
            }
        })?
        .get(0);
    c.batch_execute("SET LOCAL lock_timeout = 0").await.map_err(db)?;
    let txn: i64 = c.query_one("SELECT nextval('txn_seq')", &[]).await.map_err(db)?.get(0);
    Ok((txn, seq))
}

impl Store {
    /// Open an order: BEGIN, wait for the tenant's write turn, allocate the transaction number,
    /// record who acts.
    pub async fn begin(&self, ctx: &Ctx, audit: &Audit) -> Result<Session> {
        let turn = self.write_turn(ctx.tenant).await;
        let client = self.client().await?;
        client.batch_execute("BEGIN").await.map_err(db)?;
        let inline_limit = if audit.module.is_none() { Some(crate::files::MAX_INLINE_CONTENT) } else { None };
        let mut s = Session { client: Some(client), txn: 0, state: State::default(), seq: 0, inline_limit, _turn: turn };
        let c = s.client();
        let (txn, seq) = clock(c, ctx.tenant).await?;
        c.execute(
            "INSERT INTO txn (tenant_id, id, account, module, function, tool, seq) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[&ctx.tenant, &txn, &ctx.me(), &audit.module, &audit.function, &audit.tool, &seq],
        )
        .await
        .map_err(db)?;
        s.txn = txn;
        Ok(s)
    }
}

impl Session {
    pub fn client(&self) -> &tokio_postgres::Client {
        self.client.as_ref().expect("session is open")
    }

    /// The object an id of an operation names: a UUID or the local name of an object created
    /// earlier in this order.
    pub fn resolve_id(&self, s: &str) -> Option<Uuid> {
        self.state.local.get(s).copied().or_else(|| Uuid::parse_str(s).ok())
    }

    /// Apply one operation immediately inside the open transaction; returns the object created
    /// by a `create` (its local name or id).
    pub async fn apply(&mut self, store: &Store, ctx: &mut Ctx, op: &Op) -> Result<Option<Uuid>> {
        if self.state.admin_before.is_none() && self.may_remove_admin(store, ctx, op).await? {
            crate::admin::lock_admins(self.client(), ctx.tenant).await?;
            let had = store.has_admin(self.client(), ctx.tenant).await?;
            self.state.admin_before = Some(had);
        }
        let before: BTreeSet<Uuid> = self.state.new_objects.clone();
        let st = std::mem::take(&mut self.state);
        let client = self.client.as_ref().expect("session is open");
        let admin_before = st.admin_before;
        let mut ex = Exec {
            store,
            tx: client,
            m: &store.model,
            ctx,
            txn: self.txn,
            local: st.local,
            new_objects: st.new_objects,
            touched: st.touched,
            detached: st.detached,
            links: st.links,
            inline_limit: self.inline_limit,
            warnings: st.warnings,
            promote: st.promote,
        };
        let res = ex.apply(op).await;
        let created = ex.new_objects.difference(&before).next().copied();
        self.state = State { local: ex.local, new_objects: ex.new_objects, touched: ex.touched, detached: ex.detached, links: ex.links, admin_before, warnings: ex.warnings, promote: ex.promote };
        res.map(|_| created)
    }

    /// One line of the audit trail of this order (module code: who did what at which depth).
    pub async fn audit(&mut self, ctx: &Ctx, depth: i32, module: Option<&str>, function: Option<&str>, action: &str, target: Option<&str>) -> Result<()> {
        self.seq += 1;
        let seq = self.seq;
        self.client()
            .execute(
                "INSERT INTO txn_audit (tenant_id, txn, seq, depth, module, function, identity, action, target) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
                &[&ctx.tenant, &self.txn, &seq, &depth, &module, &function, &ctx.me(), &action, &target],
            )
            .await
            .map_err(db)?;
        Ok(())
    }

    /// Whether `op` could take the administrator right away from the tenant's last admin: deleting
    /// an existing object (parts go with their whole), changing an existing account, group or
    /// group membership (update, attach, detach), and linking or unlinking a membership (a group
    /// can carry `-` or `block` rights).
    async fn may_remove_admin(&self, store: &Store, ctx: &Ctx, op: &Op) -> Result<bool> {
        let id = match op {
            Op::Link { reference, .. } | Op::Unlink { reference, .. } => return Ok(reference.starts_with("base:GroupMembership.")),
            Op::Delete { id } => return Ok(Uuid::parse_str(id).is_ok()),
            Op::Update { id, .. } | Op::Attach { id, .. } | Op::Detach { id, .. } => id,
            _ => return Ok(false),
        };
        // objects created in this order (local names) held no rights before it
        let Ok(id) = Uuid::parse_str(id) else { return Ok(false) };
        let set = crate::read::types_of(self.client(), ctx.tenant, id, None).await?;
        Ok(store.model.set_is_a(set.keys(), "base:Carrier") || store.model.set_is_a(set.keys(), crate::order::GROUP_MEMBERSHIP))
    }

    /// Check the end state, then commit (or roll back for a dry run).
    pub async fn finish(mut self, store: &Store, ctx: &mut Ctx, commit: bool) -> Result<OrderResult> {
        let st = std::mem::take(&mut self.state);
        let client = self.client.take().expect("session is open");
        // the last administrator first: its message says more than an integrity violation that
        // removing them may also cause
        let admin = match st.admin_before {
            Some(true) => match store.has_admin(&**client, ctx.tenant).await {
                Ok(true) => Ok(()),
                Ok(false) => Err(crate::admin::last_admin_error()),
                Err(e) => Err(e),
            },
            _ => Ok(()),
        };
        let warnings = st.warnings;
        let promote = st.promote;
        let (res, created, links) = {
            let mut ex = Exec {
                store,
                tx: &client,
                m: &store.model,
                ctx,
                txn: self.txn,
                local: st.local,
                new_objects: st.new_objects,
                touched: st.touched,
                detached: st.detached,
                links: st.links,
                inline_limit: self.inline_limit,
                warnings: vec![],
                promote: vec![],
            };
            let res = match admin {
                Ok(()) => ex.check_end_state().await,
                Err(e) => Err(e),
            };
            (res, ex.local.clone().into_iter().collect(), ex.links.clone())
        };
        match res {
            Ok(()) if commit => {
                client.batch_execute("COMMIT").await.map_err(db)?;
                store.promote_uploads(&promote);
                Ok(OrderResult { txn: self.txn, dry_run: false, created, links, warnings })
            }
            Ok(()) => {
                client.batch_execute("ROLLBACK").await.map_err(db)?;
                Ok(OrderResult { txn: self.txn, dry_run: true, created, links, warnings })
            }
            Err(e) => {
                let _ = client.batch_execute("ROLLBACK").await;
                Err(e)
            }
        }
    }

    pub async fn abort(mut self) {
        if let Some(c) = self.client.take() {
            let _ = c.batch_execute("ROLLBACK").await;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(c) = self.client.take() {
            // never hand a connection with an open transaction back to the pool: detach it from
            // the pool and close it, the server rolls the transaction back
            drop(deadpool_postgres::Object::take(c));
        }
    }
}
