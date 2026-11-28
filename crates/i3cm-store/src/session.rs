//! An open order: one database transaction that stays open across several steps, used by
//! plain orders and by module code (DEC-119: one transaction per order, writes go straight in,
//! any error aborts the whole order).

use crate::context::{db, Ctx};
use crate::order::{Exec, Op, OrderResult};
use crate::Store;
use i3cm_core::Result;
use std::collections::{BTreeSet, HashMap};
use uuid::Uuid;

/// Who acts in an order beyond the account: module, function and tool (DEC-117).
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
}

pub struct Session {
    client: Option<deadpool_postgres::Object>,
    pub txn: i64,
    state: State,
    seq: i32,
}

impl Store {
    /// Open an order: BEGIN, allocate the transaction number, record who acts.
    pub async fn begin(&self, ctx: &Ctx, audit: &Audit) -> Result<Session> {
        let client = self.client().await?;
        client.batch_execute("BEGIN").await.map_err(db)?;
        let mut s = Session { client: Some(client), txn: 0, state: State::default(), seq: 0 };
        let c = s.client();
        let txn: i64 = c.query_one("SELECT nextval('txn_seq')", &[]).await.map_err(db)?.get(0);
        c.execute(
            "INSERT INTO txn (tenant_id, id, account, module, function, tool) VALUES ($1, $2, $3, $4, $5, $6)",
            &[&ctx.tenant, &txn, &ctx.me(), &audit.module, &audit.function, &audit.tool],
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

    /// Apply one operation immediately inside the open transaction; returns the object created
    /// by a `create` (its local name or id).
    pub async fn apply(&mut self, store: &Store, ctx: &mut Ctx, op: &Op) -> Result<Option<Uuid>> {
        if self.state.admin_before.is_none() && self.may_remove_admin(store, ctx, op).await? {
            let had = store.has_admin(self.client(), ctx.tenant).await?;
            self.state.admin_before = Some(had);
        }
        let before: BTreeSet<Uuid> = self.state.new_objects.clone();
        let st = std::mem::take(&mut self.state);
        let client = self.client.as_ref().expect("session is open");
        let admin_before = st.admin_before;
        let mut ex = Exec { tx: client, m: &store.model, ctx, txn: self.txn, local: st.local, new_objects: st.new_objects, touched: st.touched, detached: st.detached, links: st.links };
        let res = ex.apply(op).await;
        let created = ex.new_objects.difference(&before).next().copied();
        self.state = State { local: ex.local, new_objects: ex.new_objects, touched: ex.touched, detached: ex.detached, links: ex.links, admin_before };
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

    /// Whether `op` could take the administrator right away from the tenant's last admin: changes
    /// to existing accounts, groups and group memberships (update, detach, delete) and unlinking a
    /// membership.
    async fn may_remove_admin(&self, store: &Store, ctx: &Ctx, op: &Op) -> Result<bool> {
        let id = match op {
            Op::Unlink { reference, .. } => return Ok(reference.starts_with("base:GroupMembership.")),
            Op::Update { id, .. } | Op::Detach { id, .. } | Op::Delete { id } => id,
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
        let (res, created, links) = {
            let mut ex = Exec { tx: &client, m: &store.model, ctx, txn: self.txn, local: st.local, new_objects: st.new_objects, touched: st.touched, detached: st.detached, links: st.links };
            let res = match admin {
                Ok(()) => ex.check_end_state().await,
                Err(e) => Err(e),
            };
            (res, ex.local.clone().into_iter().collect(), ex.links.clone())
        };
        match res {
            Ok(()) if commit => {
                client.batch_execute("COMMIT").await.map_err(db)?;
                Ok(OrderResult { txn: self.txn, dry_run: false, created, links })
            }
            Ok(()) => {
                client.batch_execute("ROLLBACK").await.map_err(db)?;
                Ok(OrderResult { txn: self.txn, dry_run: true, created, links })
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
