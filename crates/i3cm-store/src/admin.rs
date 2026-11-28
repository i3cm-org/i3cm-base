//! Protection of the last administrator: no order may leave a tenant without an active account
//! that can grant rights at the root room.

use crate::context::db;
use crate::order::GROUP_MEMBERSHIP;
use crate::schema::{ref_table, type_table};
use crate::Store;
use i3cm_core::catalog::{Art, ANY};
use i3cm_core::{BaseError, Result};
use tokio_postgres::GenericClient;
use uuid::Uuid;

/// The error for an order that would remove the last administrator.
pub fn last_admin_error() -> BaseError {
    BaseError::conflict(
        "tenant.last_admin",
        "this would leave the tenant without an administrator; first give another active account the right to grant \
         rights (for example by adding it to the group \"admins\"), then repeat the request",
    )
}

impl Store {
    /// Whether the tenant has at least one active (not deleted, not deactivated) account that may
    /// grant rights at the root room, in the state visible to `c` (inside an open transaction: the
    /// state including its own writes).
    pub(crate) async fn has_admin<C: GenericClient>(&self, c: &C, tenant: Uuid) -> Result<bool> {
        let m = &self.model;
        let acc = type_table(m.require_type("base:Account")?);
        let member = ref_table(m.ref_def("base:GroupMembership.member").ok_or_else(|| BaseError::internal("catalog lacks base:GroupMembership"))?);
        let group = ref_table(m.ref_def("base:GroupMembership.group").ok_or_else(|| BaseError::internal("catalog lacks base:GroupMembership"))?);
        // candidates: active accounts that hold a profile, directly or through groups
        let sql = format!(
            "WITH RECURSIVE c(id) AS (SELECT carrier FROM assignment WHERE tenant_id=$1 AND valid_to IS NULL UNION \
             SELECT mm.b_id FROM c JOIN {group} g ON g.tenant_id=$1 AND g.b_id=c.id AND g.valid_to IS NULL \
             JOIN {member} mm ON mm.tenant_id=$1 AND mm.a_id=g.a_id AND mm.valid_to IS NULL \
             JOIN object_state s ON s.tenant_id=$1 AND s.object_id=g.a_id AND s.valid_to IS NULL AND s.deleted_at IS NULL) \
             SELECT DISTINCT a.object_id FROM c JOIN {acc} a ON a.tenant_id=$1 AND a.object_id=c.id AND a.valid_to IS NULL AND a.deleted_at IS NULL \
             AND coalesce(a.\"deactivated\", false) = false \
             JOIN object_state s ON s.tenant_id=$1 AND s.object_id=a.object_id AND s.valid_to IS NULL AND s.deleted_at IS NULL"
        );
        let candidates: Vec<Uuid> = c.query(&sql, &[&tenant]).await.map_err(db)?.iter().map(|r| r.get(0)).collect();
        for account in candidates {
            let ctx = self.load_ctx(c, tenant, account).await?;
            let root = [ctx.rooms.root];
            if ctx.subject.may(Art::Grant, ANY, &root) || ctx.subject.may(Art::Grant, GROUP_MEMBERSHIP, &root) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
