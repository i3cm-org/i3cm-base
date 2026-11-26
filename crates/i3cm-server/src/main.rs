use anyhow::Result;
use i3cm_server::{connect, connect_with, default_catalog, runtime, serve, serve_with, spawn_scheduler, EmbeddedDb};
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
    },
    /// Issue a token for an existing login; prints token and expiry
    CreateToken {
        tenant: String,
        login: String,
        /// Lifetime in days (default 90, at most 365)
        #[arg(long)]
        ttl: Option<i64>,
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
enum ModuleCmd {
    /// Verify and install `<name>-<version>.tar.gz` (signature in `<file>.sig`) into --modules-dir;
    /// in force after the next start
    Install {
        package: PathBuf,
        /// Trusted ed25519 public keys (hex), comma-separated
        #[arg(long, env = "I3CM_TRUSTED_KEYS", value_delimiter = ',')]
        trusted: Vec<String>,
    },
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
            let rt = match &cli.modules_dir {
                Some(d) => Some(runtime(&store, d)?),
                None => None,
            };
            if let Some(rt) = &rt {
                let worker = format!("{}-{}", std::env::var("HOSTNAME").unwrap_or_else(|_| "server".into()), std::process::id());
                spawn_scheduler(rt.clone(), worker, std::time::Duration::from_secs(20));
            }
            let (addr, task) = serve_with(store, rt, listen).await?;
            tracing::info!(%addr, "listening");
            task.await?;
        }
        Cmd::Module { cmd: ModuleCmd::Install { package, trusted } } => {
            let dir = cli.modules_dir.clone().ok_or_else(|| anyhow::anyhow!("--modules-dir or I3CM_MODULES_DIR required"))?;
            if trusted.is_empty() {
                anyhow::bail!("no trusted keys (--trusted or I3CM_TRUSTED_KEYS)");
            }
            let r = i3cm_server::install::install(&db()?, &catalog, &dir, &package, &trusted).await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        Cmd::InitTenant { name, admin } => {
            let store = connect(&db()?, &catalog, 4).await?;
            let init = store.init_tenant(&name, &admin).await.map_err(|e| anyhow::anyhow!("{e}"))?;
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
