//! i3cm-server: wiring of catalog, store and API; optional embedded PostgreSQL for
//! development, tests and load tests.

use anyhow::{Context, Result};
use i3cm_core::Model;
use i3cm_store::Store;
use postgresql_embedded::{PostgreSQL, Settings};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub use i3cm_store;

/// Load and check catalog.json.
pub fn load_catalog(path: &Path) -> Result<Model> {
    let json = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    Model::from_json(&json).map_err(|e| anyhow::anyhow!("{}: {} {}", e.error_id, e.description, serde_json::Value::Object(e.params)))
}

/// Default location of catalog.json (repository root), overridable by I3CM_CATALOG.
pub fn default_catalog() -> PathBuf {
    std::env::var("I3CM_CATALOG").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../catalog.json")))
}

/// A PostgreSQL server run as a child process; stopped on drop of the handle owner.
pub struct EmbeddedDb {
    pub pg: PostgreSQL,
}

impl EmbeddedDb {
    /// Start a server. `data_dir = None` gives a temporary cluster removed afterwards.
    pub async fn start(data_dir: Option<PathBuf>) -> Result<EmbeddedDb> {
        let mut s = Settings::default();
        match data_dir {
            Some(d) => {
                s.data_dir = d;
                s.temporary = false;
            }
            None => s.temporary = true,
        }
        s.configuration.insert("max_connections".into(), "300".into());
        s.configuration.insert("fsync".into(), "off".into());
        s.configuration.insert("synchronous_commit".into(), "off".into());
        s.configuration.insert("shared_buffers".into(), "256MB".into());
        let mut pg = PostgreSQL::new(s);
        pg.setup().await.context("embedded PostgreSQL setup")?;
        pg.start().await.context("embedded PostgreSQL start")?;
        Ok(EmbeddedDb { pg })
    }

    /// Create (if needed) a database and return its URL.
    pub async fn database(&self, name: &str) -> Result<String> {
        if !self.pg.database_exists(name).await? {
            self.pg.create_database(name).await?;
        }
        Ok(self.pg.settings().url(name))
    }

    pub async fn stop(&self) -> Result<()> {
        self.pg.stop().await?;
        Ok(())
    }
}

pub async fn connect(url: &str, catalog: &Path, pool: usize) -> Result<Store> {
    let model = load_catalog(catalog)?;
    Store::connect(url, model, pool).await.map_err(|e| anyhow::anyhow!("{}: {} {:?}", e.error_id, e.description, e.params))
}

/// Serve the API; returns the bound address and the server task.
pub async fn serve(store: Store, addr: SocketAddr) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let app = i3cm_api::router(store);
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(%e, "server stopped");
        }
    });
    Ok((bound, task))
}
