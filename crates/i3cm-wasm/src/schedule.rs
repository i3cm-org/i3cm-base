//! Scheduler: runs the scheduled operations of modules per tenant, in the identity of the
//! module's first carrier. Every due run is a row; claiming it (`FOR UPDATE SKIP LOCKED`), running
//! the operation and marking it done happen in one order, so a run happens exactly once even with
//! several main containers at one database.

use crate::Runtime;
use i3cm_core::{BaseError, Result};
use i3cm_store::session::Audit;
use serde_json::json;

/// The period start a run is due for: supported RRULEs are `FREQ=MINUTELY|HOURLY|DAILY|WEEKLY`
/// with an optional `INTERVAL=n` (the full RFC 5545 set follows when needed).
pub fn period_seconds(rrule: &str) -> Result<i64> {
    let mut freq = None;
    let mut interval = 1i64;
    for part in rrule.split(';') {
        match part.split_once('=') {
            Some(("FREQ", f)) => freq = Some(f.to_string()),
            Some(("INTERVAL", n)) => interval = n.parse().map_err(|_| BaseError::invalid("schedule.bad_rrule", "INTERVAL must be a number").with("rrule", rrule))?,
            _ => {}
        }
    }
    let base = match freq.as_deref() {
        Some("MINUTELY") => 60,
        Some("HOURLY") => 3600,
        Some("DAILY") => 86400,
        Some("WEEKLY") => 7 * 86400,
        _ => return Err(BaseError::invalid("schedule.bad_rrule", "supported: FREQ=MINUTELY|HOURLY|DAILY|WEEKLY;INTERVAL=n").with("rrule", rrule)),
    };
    Ok(base * interval.max(1))
}

fn dberr(e: tokio_postgres::Error) -> BaseError {
    BaseError::internal(format!("database: {}", e.as_db_error().map(|d| d.message().to_string()).unwrap_or_else(|| e.to_string())))
}

impl Runtime {
    /// One scheduler pass: create the due rows for this period, then run every open one this
    /// container can claim. Returns the number of runs executed here.
    pub async fn tick(&self, worker: &str) -> Result<usize> {
        let store = self.store();
        let mut ran = 0;
        let tenants = store.tenant_ids().await?;
        let modules: Vec<_> = store.model.catalog.modules.iter().filter(|m| !m.schedules.is_empty()).cloned().collect();
        for tenant in tenants {
            for m in &modules {
                if !store.module_active(tenant, &m.name).await? {
                    continue;
                }
                let Some(carrier) = m.carriers.first() else { continue };
                let Ok(account) = store.module_carrier(tenant, &m.name, &carrier.name).await else { continue };
                let ctx = store.ctx(tenant, account).await?;
                for sc in &m.schedules {
                    let secs = period_seconds(&sc.rrule)?;
                    {
                        let c = store.client().await?;
                        c.execute(
                            "INSERT INTO schedule_run (tenant_id, module, op, due) VALUES ($1, $2, $3, to_timestamp(floor(extract(epoch FROM now()) / $4::float8) * $4::float8)) ON CONFLICT DO NOTHING",
                            &[&tenant, &m.name, &sc.op, &(secs as f64)],
                        )
                        .await
                        .map_err(dberr)?;
                    }
                    loop {
                        let audit = Audit { module: Some(m.name.clone()), function: Some(sc.op.clone()), tool: Some("scheduler".into()) };
                        let session = store.begin(&ctx, &audit).await?;
                        let row = session
                            .client()
                            .query_opt(
                                "SELECT due FROM schedule_run WHERE tenant_id=$1 AND module=$2 AND op=$3 AND done_at IS NULL AND due <= now() ORDER BY due LIMIT 1 FOR UPDATE SKIP LOCKED",
                                &[&tenant, &m.name, &sc.op],
                            )
                            .await
                            .map_err(dberr)?;
                        let Some(row) = row else {
                            session.abort().await;
                            break;
                        };
                        let due: chrono::DateTime<chrono::Utc> = row.get(0);
                        session
                            .client()
                            .execute("UPDATE schedule_run SET done_at=now(), worker=$5 WHERE tenant_id=$1 AND module=$2 AND op=$3 AND due=$4", &[&tenant, &m.name, &sc.op, &due, &worker])
                            .await
                            .map_err(dberr)?;
                        let params = json!({ "scheduled": due.to_rfc3339() });
                        match self.run_in(session, &ctx, &m.name, &sc.op, &params).await {
                            Ok(_) => ran += 1,
                            Err(e) => {
                                // the order (claim included) is rolled back; note the failure so it is not retried forever
                                tracing::warn!(module = %m.name, op = %sc.op, error = %e.description, "scheduled run failed");
                                // a few attempts; if the database stays away the run is retried on a later
                                // pass (at least once, not exactly once, in that case)
                                for _ in 0..3 {
                                    let Ok(c) = store.client().await else { continue };
                                    if c.execute("UPDATE schedule_run SET done_at=now(), worker=$5, result=$6 WHERE tenant_id=$1 AND module=$2 AND op=$3 AND due=$4", &[&tenant, &m.name, &sc.op, &due, &worker, &e.error_id])
                                        .await
                                        .is_ok()
                                    {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(ran)
    }
}
