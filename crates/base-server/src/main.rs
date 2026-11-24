use anyhow::Result;
use base_server::{connect, default_catalog, serve, EmbeddedDb};
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "base-server", version, about = "base-erp core server")]
struct Cli {
    /// PostgreSQL URL
    #[arg(long, env = "BASE_DATABASE_URL", global = true)]
    db: Option<String>,
    /// catalog.json produced by typecat
    #[arg(long, env = "BASE_CATALOG", global = true)]
    catalog: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the REST API
    Serve {
        #[arg(long, env = "BASE_LISTEN", default_value = "0.0.0.0:8480")]
        listen: SocketAddr,
        #[arg(long, default_value_t = 32)]
        pool: usize,
    },
    /// Create a tenant with root room, roles and an admin login; prints the admin token
    InitTenant {
        name: String,
        #[arg(long, default_value = "admin")]
        admin: String,
    },
    /// Issue a token for an existing login
    CreateToken { tenant: String, login: String },
    /// Development: embedded PostgreSQL in a data directory, tenant "dev", serve
    Dev {
        #[arg(long, default_value = ".pgdata")]
        data: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8480")]
        listen: SocketAddr,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let cli = Cli::parse();
    let catalog = cli.catalog.clone().unwrap_or_else(default_catalog);
    let db = || cli.db.clone().ok_or_else(|| anyhow::anyhow!("--db or BASE_DATABASE_URL required"));
    match cli.cmd {
        Cmd::Serve { listen, pool } => {
            let store = connect(&db()?, &catalog, pool).await?;
            let (addr, task) = serve(store, listen).await?;
            tracing::info!(%addr, "listening");
            task.await?;
        }
        Cmd::InitTenant { name, admin } => {
            let store = connect(&db()?, &catalog, 4).await?;
            let init = store.init_tenant(&name, &admin).await.map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", serde_json::to_string_pretty(&init)?);
        }
        Cmd::CreateToken { tenant, login } => {
            let store = connect(&db()?, &catalog, 4).await?;
            let t = store.tenant_by_name(&tenant).await.map_err(|e| anyhow::anyhow!("{e}"))?;
            let a = store.account_by_login(t, &login).await.map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", store.create_token_unchecked(t, a).await.map_err(|e| anyhow::anyhow!("{e}"))?);
        }
        Cmd::Dev { data, listen } => {
            let pg = EmbeddedDb::start(Some(std::fs::canonicalize(".").unwrap_or_default().join(data))).await?;
            let url = pg.database("base").await?;
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
