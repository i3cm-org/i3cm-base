use anyhow::Result;
use i3cm_server::{connect, connect_with, default_catalog, runtime, serve, serve_full, spawn_scheduler, EmbeddedDb, Operator};
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "i3cm-server", version, about = "i3cm core server")]
struct Cli {
    /// PostgreSQL URL
    #[arg(long, env = "I3CM_DATABASE_URL", global = true)]
    db: Option<String>,
    /// catalog.json produced by typecat
    #[arg(long, env = "I3CM_CATALOG", global = true)]
    catalog: Option<PathBuf>,
    /// Directory with module code (`<module>.wasm`); without it, module operations and hooks are off
    #[arg(long, env = "I3CM_MODULES_DIR", global = true)]
    modules_dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the REST API
    Serve {
        #[arg(long, env = "I3CM_LISTEN", default_value = "0.0.0.0:8480")]
        listen: SocketAddr,
        #[arg(long, default_value_t = 32)]
        pool: usize,
    },
    /// Create a tenant with root room, profiles and an admin login; prints the admin token (valid one day)
    InitTenant {
        name: String,
        #[arg(long, default_value = "admin")]
        admin: String,
        /// Development tenant (studio); only if the operator allows it with I3CM_ALLOW_DEVELOPMENT=true
        #[arg(long)]
        development: bool,
    },
    /// Issue a token for an existing login; prints token and expiry
    CreateToken {
        tenant: String,
        login: String,
        /// Lifetime in days (default 90, at most 365)
        #[arg(long)]
        ttl: Option<i64>,
    },
    /// Catalog: check a catalog.json with the server's checker, without a database
    Catalog {
        #[command(subcommand)]
        cmd: CatalogCmd,
    },
    /// Modules: install a signed package (operator)
    Module {
        #[command(subcommand)]
        cmd: ModuleCmd,
    },
    /// Development: embedded PostgreSQL in a data directory, tenant "dev", serve
    Dev {
        #[arg(long, default_value = ".pgdata")]
        data: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8480")]
        listen: SocketAddr,
    },
}

#[derive(Subcommand)]
enum CatalogCmd {
    /// Check a catalog (default: --catalog / I3CM_CATALOG); prints the result as JSON, exit 1 on an error
    Check {
        /// catalog.json to check
        file: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ModuleCmd {
    /// Verify and install `<name>-<version>.tar.gz` (signature in `<file>.sig`) into --modules-dir;
    /// in force after the next start
    Install {
        /// Package file, or `name[@version]` with --registry
        package: PathBuf,
        /// Registry (URL or file of registry.json): install `name[@version]` from it
        #[arg(long, env = "I3CM_REGISTRY")]
        registry: Option<String>,
        /// Further trusted ed25519 public keys (hex), comma-separated; added to the built-in key of
        /// i3cm-org, I3CM_TRUSTED_KEYS and I3CM_TRUSTED_KEYS_FILE
        #[arg(long, value_delimiter = ',')]
        trusted: Vec<String>,
    },
}

/// The operator allows development tenants (and unsigned module packages) on this instance.
fn development_allowed() -> bool {
    matches!(std::env::var("I3CM_ALLOW_DEVELOPMENT").as_deref(), Ok("true") | Ok("1") | Ok("yes"))
}

#[tokio::main]
async fn main() -> Result<()> {
    // logs go to stderr so that command output on stdout stays machine-readable
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tokio_postgres=warn".into()))
        .init();
    let cli = Cli::parse();
    let catalog = cli.catalog.clone().unwrap_or_else(default_catalog);
    let db = || cli.db.clone().ok_or_else(|| anyhow::anyhow!("--db or I3CM_DATABASE_URL required"));
    match cli.cmd {
        Cmd::Serve { listen, pool } => {
            let store = connect_with(&db()?, &catalog, cli.modules_dir.as_deref(), pool).await?;
            // bring tenants to the installed module versions (new profiles, rights, carriers)
            match store.sync_modules_all().await {
                Ok(all) => {
                    for s in all {
                        for c in &s.changes {
                            tracing::info!(tenant = %s.tenant, "module sync: {c}");
                        }
                        for w in &s.warnings {
                            tracing::warn!(tenant = %s.tenant, "module sync: {w}");
                        }
                    }
                }
                // a failed sync must not keep the instance down; the next start tries again
                Err(e) => tracing::error!(error = %e.description, params = %serde_json::Value::Object(e.params.clone()), "module sync failed"),
            }
            let rt = match &cli.modules_dir {
                Some(d) => Some(runtime(&store, d)?),
                None => None,
            };
            if let Some(rt) = &rt {
                let worker = format!("{}-{}", std::env::var("HOSTNAME").unwrap_or_else(|_| "server".into()), std::process::id());
                spawn_scheduler(rt.clone(), worker, std::time::Duration::from_secs(20));
            }
            // operator surface over HTTP: only with a root secret (I3CM_OPERATOR_TOKEN)
            let op = match (std::env::var("I3CM_OPERATOR_TOKEN").ok().filter(|t| t.len() >= 32), &cli.modules_dir) {
                (Some(token), Some(dir)) => Some(Operator {
                    token,
                    db_url: db()?,
                    catalog: catalog.clone(),
                    modules_dir: dir.clone(),
                    registry: std::env::var("I3CM_REGISTRY").ok(),
                    trust: i3cm_server::install::Trust::from_env(&[])?,
                }),
                _ => None,
            };
            let (addr, task) = serve_full(store, rt, op, listen).await?;
            tracing::info!(%addr, "listening");
            task.await?;
        }
        Cmd::Catalog { cmd: CatalogCmd::Check { file } } => {
            let path = file.unwrap_or(catalog);
            let json = std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
            match i3cm_core::Model::from_json(&json) {
                Ok(m) => println!("{}", serde_json::json!({ "ok": true, "modules": m.catalog.modules.len(), "types": m.catalog.types.len() })),
                Err(e) => {
                    println!("{}", serde_json::json!({ "ok": false, "error_id": e.error_id, "description": e.description, "params": e.params }));
                    std::process::exit(1);
                }
            }
        }
        Cmd::Module { cmd: ModuleCmd::Install { package, registry, trusted } } => {
            let dir = cli.modules_dir.clone().ok_or_else(|| anyhow::anyhow!("--modules-dir or I3CM_MODULES_DIR required"))?;
            let trust = i3cm_server::install::Trust::from_env(&trusted)?;
            if trust.keys.is_empty() && !trust.allow_unsigned {
                anyhow::bail!("no trusted keys (built-in key of i3cm-org, --trusted, I3CM_TRUSTED_KEYS or I3CM_TRUSTED_KEYS_FILE)");
            }
            let r = match &registry {
                Some(src) if !package.exists() => {
                    let spec = package.to_string_lossy().to_string();
                    let (name, version) = match spec.split_once('@') {
                        Some((n, v)) => (n.to_string(), Some(v.to_string())),
                        None => (spec, None),
                    };
                    i3cm_server::install::install_from_registry_with(&db()?, &catalog, &dir, src, &name, version.as_deref(), &trust).await?
                }
                _ => i3cm_server::install::install_with(&db()?, &catalog, &dir, &package, &trust).await?,
            };
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        Cmd::InitTenant { name, admin, development } => {
            if development && !development_allowed() {
                anyhow::bail!("development tenants are not allowed on this instance (operator: I3CM_ALLOW_DEVELOPMENT=true)");
            }
            let store = connect_with(&db()?, &catalog, cli.modules_dir.as_deref(), 4).await?;
            let init = store.init_tenant_with(&name, &admin, development).await.map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", serde_json::to_string_pretty(&init)?);
        }
        Cmd::CreateToken { tenant, login, ttl } => {
            let store = connect(&db()?, &catalog, 4).await?;
            let t = store.tenant_by_name(&tenant).await.map_err(|e| anyhow::anyhow!("{e}"))?;
            let a = store.account_by_login(t, &login).await.map_err(|e| anyhow::anyhow!("{e}"))?;
            let issued = store.create_token_unchecked(t, a, ttl).await.map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Some(exp) = issued.expires_at {
                eprintln!("expires: {}", exp.to_rfc3339());
            }
            println!("{}", issued.token);
        }
        Cmd::Dev { data, listen } => {
            let pg = EmbeddedDb::start(Some(std::fs::canonicalize(".").unwrap_or_default().join(data))).await?;
            let url = pg.database("i3cm").await?;
            let store = connect(&url, &catalog, 32).await?;
            match store.tenant_by_name("dev").await {
                Ok(_) => tracing::info!("tenant dev exists; use create-token for a new token"),
                Err(_) => {
                    let init = store.init_tenant("dev", "admin").await.map_err(|e| anyhow::anyhow!("{e}"))?;
                    println!("admin token: {}", init.token);
                }
            }
            let (addr, task) = serve(store, listen).await?;
            println!("database: {url}\nlistening: http://{addr}");
            tokio::select! {
                _ = task => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            pg.stop().await?;
        }
    }
    Ok(())
}
