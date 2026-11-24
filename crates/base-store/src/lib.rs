//! base-store: PostgreSQL storage of the base: object core plus one table per type.
//!
//! * one core table `object` plus `object_state` (room, deletion) and `object_type`;
//! * one table per type holding only the attributes that type defines;
//! * one table per reference;
//! * every row carries a validity range `valid_from`/`valid_to` (transaction marks).

pub mod aspect;
pub mod context;
pub mod order;
pub mod read;
pub mod schema;
pub mod sql;
pub mod tenant;

use base_core::i18n::I18n;
use base_core::{BaseError, Model, Result};
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
}

impl Store {
    /// Connect and create or extend the schema for the catalog.
    pub async fn connect(url: &str, model: Model, pool_size: usize) -> Result<Store> {
        let mut cfg = Config::new();
        cfg.url = Some(url.to_string());
        cfg.pool = Some(deadpool_postgres::PoolConfig::new(pool_size));
        let pool = cfg.create_pool(Some(Runtime::Tokio1), NoTls).map_err(|e| BaseError::internal(format!("pool: {e}")))?;
        let i18n = I18n::new(&model)?;
        let store = Store { pool, model: Arc::new(model), i18n: Arc::new(i18n) };
        store.migrate().await?;
        Ok(store)
    }

    pub async fn client(&self) -> Result<deadpool_postgres::Object> {
        self.pool.get().await.map_err(|e| BaseError::internal(format!("pool: {e}")))
    }

    async fn migrate(&self) -> Result<()> {
        let c = self.client().await?;
        c.batch_execute("SELECT pg_advisory_lock(4711)").await.map_err(db)?;
        let res = async {
            c.batch_execute(schema::SYSTEM).await.map_err(db)?;
            for stmt in schema::catalog_ddl(&self.model) {
                c.batch_execute(&stmt).await.map_err(db)?;
            }
            Ok::<_, BaseError>(())
        }
        .await;
        c.batch_execute("SELECT pg_advisory_unlock(4711)").await.map_err(db)?;
        res
    }

    /// Resolve a bearer token to (tenant, account).
    pub async fn authenticate(&self, token: &str) -> Result<(uuid::Uuid, uuid::Uuid)> {
        let hash = tenant::hash_token(token);
        let c = self.client().await?;
        let row = c
            .query_opt("SELECT tenant_id, account FROM token WHERE hash=$1", &[&hash])
            .await
            .map_err(db)?
            .ok_or_else(|| BaseError::new(base_core::ErrorKind::Unauthenticated, "auth.invalid_token", "unknown token"))?;
        Ok((row.get(0), row.get(1)))
    }

    /// Load the request context for an authenticated account.
    pub async fn ctx(&self, tenant: uuid::Uuid, account: uuid::Uuid) -> Result<Ctx> {
        let c = self.client().await?;
        self.load_ctx(&**c, tenant, account).await
    }
}
