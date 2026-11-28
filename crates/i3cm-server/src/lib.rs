//! i3cm-server: wiring of catalog, store and API; optional embedded PostgreSQL for
//! development, tests and load tests.

pub mod install;

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

/// Catalog for the kernel's own tests: the kernel types plus the test module
/// `tests/modules/sample` (mail, documents, tasks). Built once per process with typecat.
pub fn test_catalog() -> PathBuf {
    static CAT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CAT.get_or_init(|| {
        let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        if let Ok(p) = std::env::var("I3CM_TEST_CATALOG") {
            return PathBuf::from(p);
        }
        let out = std::env::temp_dir().join(format!("i3cm-test-catalog-{}.json", std::process::id()));
        let st = std::process::Command::new("node")
            .arg(root.join("tools/typecat/typecat.mjs"))
            .arg(root.join("types"))
            .arg("--module")
            .arg(root.join("tests/modules/sample"))
            .arg("--out")
            .arg(&out)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("node (typecat) for the test catalog");
        assert!(st.success(), "typecat failed for the test catalog");
        out
    })
    .clone()
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
    connect_with(url, catalog, None, pool).await
}

/// Connect with the base catalog plus the module parts installed in `modules_dir`.
pub async fn connect_with(url: &str, catalog: &Path, modules_dir: Option<&Path>, pool: usize) -> Result<Store> {
    let model = install::load_model(catalog, modules_dir)?;
    Store::connect(url, model, pool).await.map_err(|e| anyhow::anyhow!("{}: {} {:?}", e.error_id, e.description, e.params))
}

/// Serve the API; returns the bound address and the server task.
pub async fn serve(store: Store, addr: SocketAddr) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    serve_with(store, None, addr).await
}

/// Load the module code from a directory (`<module>.wasm` per module with operations).
pub fn runtime(store: &Store, dir: &Path) -> Result<i3cm_wasm::Runtime> {
    i3cm_wasm::Runtime::new(store.clone(), dir).map_err(|e| anyhow::anyhow!("{}: {} {:?}", e.error_id, e.description, e.params))
}

/// Run the scheduler every `every` (exactly once per due run across containers).
pub fn spawn_scheduler(rt: i3cm_wasm::Runtime, worker: String, every: std::time::Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(e) = rt.tick(&worker).await {
                tracing::warn!(error = %e.description, "scheduler pass failed");
            }
            tokio::time::sleep(every).await;
        }
    })
}

/// Serve the API with optional module code.
pub async fn serve_with(store: Store, rt: Option<i3cm_wasm::Runtime>, addr: SocketAddr) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let app = i3cm_api::router_with(store, rt);
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(%e, "server stopped");
        }
    });
    Ok((bound, task))
}

/// Operator surface over HTTP (proposal admin-surface-over-http): off unless a root secret is
/// configured; separate from tenant tokens.
#[derive(Clone)]
pub struct Operator {
    pub token: String,
    pub db_url: String,
    pub catalog: PathBuf,
    pub modules_dir: PathBuf,
    pub registry: Option<String>,
    /// Keys and whether unsigned packages are accepted ([`install::Trust::from_env`]).
    pub trust: install::Trust,
}

/// `GET /v1/operator/registry`, `POST /v1/operator/modules/install` (`{"name", "version"?}` from
/// the registry); in force after the next start of the main container.
pub fn operator_router(op: Operator) -> axum::Router {
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::Json;
    use serde_json::{json, Value};
    fn ok(op: &Operator, h: &HeaderMap) -> bool {
        h.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).is_some_and(|t| {
            // constant-time comparison of the root secret
            t.len() == op.token.len() && t.bytes().zip(op.token.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
        })
    }
    fn fail(code: StatusCode, id: &str, msg: String) -> (StatusCode, Json<Value>) {
        (code, Json(json!({"error_id": id, "description": msg, "params": {}})))
    }
    axum::Router::new()
        .route(
            "/v1/operator/registry",
            get(|State(op): State<Operator>, h: HeaderMap| async move {
                if !ok(&op, &h) {
                    return fail(StatusCode::UNAUTHORIZED, "operator.denied", "operator secret required".into());
                }
                let Some(src) = &op.registry else { return fail(StatusCode::NOT_FOUND, "operator.no_registry", "no registry configured".into()) };
                match install::registry(src).await {
                    Ok(list) => (StatusCode::OK, Json(json!(list))),
                    Err(e) => fail(StatusCode::BAD_GATEWAY, "operator.registry", format!("{e:#}")),
                }
            }),
        )
        .route(
            "/v1/operator/modules/install",
            post(|State(op): State<Operator>, h: HeaderMap, Json(b): Json<Value>| async move {
                if !ok(&op, &h) {
                    return fail(StatusCode::UNAUTHORIZED, "operator.denied", "operator secret required".into());
                }
                let Some(src) = &op.registry else { return fail(StatusCode::NOT_FOUND, "operator.no_registry", "no registry configured".into()) };
                let Some(name) = b["name"].as_str() else { return fail(StatusCode::BAD_REQUEST, "operator.bad_request", "name required".into()) };
                match install::install_from_registry_with(&op.db_url, &op.catalog, &op.modules_dir, src, name, b["version"].as_str(), &op.trust).await {
                    Ok(r) => (StatusCode::OK, Json(json!({"installed": r, "note": "in force after the next start"}))),
                    Err(e) => fail(StatusCode::BAD_REQUEST, "operator.install_failed", format!("{e:#}")),
                }
            }),
        )
        .with_state(op)
}

/// Serve the API with optional module code and optional operator surface.
pub async fn serve_full(store: Store, rt: Option<i3cm_wasm::Runtime>, op: Option<Operator>, addr: SocketAddr) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let mut app = i3cm_api::router_with(store, rt);
    if let Some(op) = op {
        app = app.merge(operator_router(op));
    }
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(%e, "server stopped");
        }
    });
    Ok((bound, task))
}
