//! i3cm-store: PostgreSQL storage of the base: object core plus one table per type.
//!
//! * one core table `object` plus `object_state` (room, deletion) and `object_type`;
//! * one table per type holding only the attributes that type defines;
//! * one table per reference;
//! * every row carries a validity range `valid_from`/`valid_to` (transaction marks).

pub mod aspect;
pub mod fs;
pub mod context;
pub mod order;
pub mod read;
pub mod rights;
pub mod schema;
pub mod sql;
pub mod tenant;
pub mod write;

use i3cm_core::i18n::I18n;
use i3cm_core::{BaseError, Model, Result};
use context::db;
use deadpool_postgres::{Config, Pool, Runtime};
use std::sync::Arc;
use tokio_postgres::NoTls;

pub use context::Ctx;
pub use order::{Op, Order, OrderResult};

#[derive(Clone)]
pub struct Store {
    pub pool: Pool,
    pub model: Arc<Model>,
    pub i18n: Arc<I18n>,
    /// The trigram index for the search prefilter exists.
    pub trigram: bool,
}

/// An authenticated request: tenant, account and the token used.
#[derive(Debug, Clone)]
pub struct Auth {
    pub tenant: uuid::Uuid,
    pub account: uuid::Uuid,
    pub token: uuid::Uuid,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Store {
    /// Connect and create or extend the schema for the catalog.
    pub async fn connect(url: &str, model: Model, pool_size: usize) -> Result<Store> {
        let mut cfg = Config::new();
        cfg.url = Some(url.to_string());
        cfg.pool = Some(deadpool_postgres::PoolConfig::new(pool_size));
        let pool = cfg.create_pool(Some(Runtime::Tokio1), NoTls).map_err(|e| BaseError::internal(format!("pool: {e}")))?;
        let i18n = I18n::new(&model)?;
        let mut store = Store { pool, model: Arc::new(model), i18n: Arc::new(i18n), trigram: false };
        store.trigram = store.migrate().await?;
        Ok(store)
    }

    pub async fn client(&self) -> Result<deadpool_postgres::Object> {
        self.pool.get().await.map_err(|e| BaseError::internal(format!("pool: {e}")))
    }

    /// Create or extend the schema; returns whether the trigram index is available.
    async fn migrate(&self) -> Result<bool> {
        let c = self.client().await?;
        c.batch_execute("SELECT pg_advisory_lock(4711)").await.map_err(db)?;
        let res = async {
            c.batch_execute(schema::SYSTEM).await.map_err(db)?;
            for stmt in schema::catalog_ddl(&self.model) {
                c.batch_execute(&stmt).await.map_err(db)?;
            }
            Ok::<_, BaseError>(match c.batch_execute(schema::TRIGRAM).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "pg_trgm unavailable: the grep prefilter scans texts without index");
                    false
                }
            })
        }
        .await;
        c.batch_execute("SELECT pg_advisory_unlock(4711)").await.map_err(db)?;
        res
    }

    /// Resolve a bearer token; expired and revoked tokens are rejected.
    pub async fn authenticate(&self, token: &str) -> Result<Auth> {
        let hash = tenant::hash_token(token);
        let c = self.client().await?;
        let unauth = |id: &str, d: &str| BaseError::new(i3cm_core::ErrorKind::Unauthenticated, id, d);
        let row = c
            .query_opt(
                "SELECT tenant_id, account, id, expires_at, revoked_at IS NOT NULL, expires_at IS NOT NULL AND expires_at <= now(), \
                 last_used IS NULL OR last_used < now() - interval '1 minute' FROM token WHERE hash=$1",
                &[&hash],
            )
            .await
            .map_err(db)?
            .ok_or_else(|| unauth("auth.invalid_token", "unknown token"))?;
        if row.get::<_, bool>(4) {
            return Err(unauth("auth.revoked_token", "the token was revoked"));
        }
        if row.get::<_, bool>(5) {
            return Err(unauth("auth.expired_token", "the token has expired"));
        }
        let id: uuid::Uuid = row.get(2);
        if row.get::<_, bool>(6) {
            c.execute("UPDATE token SET last_used=now() WHERE id=$1", &[&id]).await.map_err(db)?;
        }
        Ok(Auth { tenant: row.get(0), account: row.get(1), token: id, expires_at: row.get(3) })
    }

    /// Load the request context for an authenticated account.
    pub async fn ctx(&self, tenant: uuid::Uuid, account: uuid::Uuid) -> Result<Ctx> {
        let c = self.client().await?;
        self.load_ctx(&**c, tenant, account).await
    }
}
