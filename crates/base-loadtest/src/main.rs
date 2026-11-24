//! Load test: ledger accounts, accountants with their own logins, journal entries created
//! concurrently by the accountants, then navigation over the date fields.
//!
//! `base-loadtest --embedded` starts PostgreSQL and the server in-process;
//! `base-loadtest --server URL --token TOKEN` uses a running server (admin token).

use anyhow::{Context, Result};
use base_client::{Client, ListQuery, Order};
use chrono::{Duration as CDur, NaiveDate};
use clap::Parser;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Parser, Clone)]
#[command(name = "base-loadtest", about = "Load test for the base-erp REST API (accounting example)")]
struct Args {
    /// Start embedded PostgreSQL and the server in this process
    #[arg(long)]
    embedded: bool,
    #[arg(long)]
    server: Option<String>,
    /// Admin token (with --server)
    #[arg(long, env = "BASE_TOKEN")]
    token: Option<String>,
    #[arg(long, default_value_t = 1000)]
    accounts: usize,
    #[arg(long, default_value_t = 100_000)]
    entries: usize,
    #[arg(long, default_value_t = 45)]
    accountants: usize,
    /// Parallel requests
    #[arg(long, default_value_t = 45)]
    concurrency: usize,
    /// Journal entries per order
    #[arg(long, default_value_t = 1)]
    batch: usize,
    #[arg(long, default_value_t = 42)]
    seed: u64,
}

struct Stats {
    lat: Mutex<Vec<Duration>>,
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    if v.is_empty() {
        return Duration::ZERO;
    }
    v.sort();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

async fn timed<T, F: std::future::Future<Output = std::result::Result<T, base_client::ClientError>>>(label: &str, f: F) -> Result<T> {
    let t = Instant::now();
    let r = f.await.with_context(|| label.to_string())?;
    println!("  {:<58} {:>8.1} ms", label, t.elapsed().as_secs_f64() * 1000.0);
    Ok(r)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut _keep = None;
    let mut _store: Option<base_server::base_store::Store> = None;
    let (base, token) = if args.embedded {
        println!("starting embedded PostgreSQL and server ...");
        let db = base_server::EmbeddedDb::start(None).await?;
        let url = db.database("loadtest").await?;
        let store = base_server::connect(&url, &base_server::default_catalog(), (args.concurrency + 8).max(16)).await?;
        let init = store.init_tenant("loadtest", "admin").await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let (addr, task) = base_server::serve(store.clone(), "127.0.0.1:0".parse()?).await?;
        _store = Some(store);
        _keep = Some((db, task));
        (format!("http://{addr}"), init.token)
    } else {
        (args.server.clone().context("--server or --embedded")?, args.token.clone().context("--token")?)
    };
    let admin = Client::new(&base, &token);
    let rooms = admin.rooms().await?;
    let fibu = *rooms.get("fibu").context("module room fibu missing")?;
    let mut rng = StdRng::seed_from_u64(args.seed);

    // accountants
    let t0 = Instant::now();
    let mut o = Order::new();
    o.create("g", None, json!({"base:Group": {"name": format!("accountants-{}", Uuid::new_v4().simple())}}));
    let group = admin.order(&o).await?.created["g"];
    admin.assign("fibu:accountant", group).await?;
    let mut accountants: Vec<(Uuid, Client)> = vec![];
    for chunk in (0..args.accountants).collect::<Vec<_>>().chunks(15) {
        let mut o = Order::new();
        for i in chunk {
            let p = format!("p{i}");
            let m = format!("m{i}");
            let login = format!("acc{i:02}-{}", &Uuid::new_v4().simple().to_string()[..6]);
            o.create(&p, None, json!({"base:Person": {"firstName": "Accountant", "lastName": format!("No. {i}")}, "base:PersonAccount": {"login": login}}));
            o.create(&m, None, json!({"base:GroupMembership": {}}));
            o.link("base:GroupMembership.group", m.as_str(), group).link("base:GroupMembership.member", m.as_str(), p.as_str());
        }
        let r = admin.order(&o).await?;
        for i in chunk {
            let id = r.created[&format!("p{i}")];
            let tok = admin.create_token(id).await?;
            accountants.push((id, admin.with_token(&tok)));
        }
    }
    println!("{} accountants with logins in {:.1} s", accountants.len(), t0.elapsed().as_secs_f64());

    // ledger accounts
    let t0 = Instant::now();
    let kinds = ["asset", "liability", "equity", "expense", "revenue"];
    let mut ledger: Vec<Uuid> = vec![];
    for chunk in (0..args.accounts).collect::<Vec<_>>().chunks(100) {
        let mut o = Order::new();
        for i in chunk {
            o.create(&format!("a{i}"), Some(fibu), json!({"fibu:LedgerAccount": {"number": format!("{}", 10000 + i), "name": format!("Account {}", 10000 + i), "kind": kinds[i % kinds.len()]}}));
        }
        let r = admin.order(&o).await?;
        ledger.extend(chunk.iter().map(|i| r.created[&format!("a{i}")]));
    }
    println!("{} ledger accounts in {:.1} s", ledger.len(), t0.elapsed().as_secs_f64());

    // journal entries
    let start = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    let plan: Vec<(NaiveDate, usize, usize, i64)> = (0..args.entries)
        .map(|_| (start + CDur::days(rng.random_range(0..730)), rng.random_range(0..ledger.len()), rng.random_range(0..ledger.len()), rng.random_range(100..1_000_000)))
        .collect();
    let plan = Arc::new(plan);
    let ledger = Arc::new(ledger);
    let accountants = Arc::new(accountants);
    let next = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));
    let stats = Arc::new(Stats { lat: Mutex::new(Vec::with_capacity(args.entries / args.batch + 1)) });
    let t0 = Instant::now();
    let mut tasks = vec![];
    for w in 0..args.concurrency {
        let (plan, ledger, accountants, next, errors, stats) = (plan.clone(), ledger.clone(), accountants.clone(), next.clone(), errors.clone(), stats.clone());
        let batch = args.batch;
        tasks.push(tokio::spawn(async move {
            let client = &accountants[w % accountants.len()].1;
            loop {
                let first = next.fetch_add(batch, Ordering::Relaxed);
                if first >= plan.len() {
                    break;
                }
                let mut o = Order::new();
                for n in first..(first + batch).min(plan.len()) {
                    let (date, a, b, cents) = plan[n];
                    let amount = format!("{}.{:02}", cents / 100, cents % 100);
                    let e = format!("e{n}");
                    o.create(&e, Some(fibu), json!({"fibu:JournalEntry": {"documentDate": date.to_string(), "postingDate": date.to_string(), "documentNumber": format!("B-{n:06}"), "fiscalYear": 2025 + (date.format("%Y").to_string() == "2026") as i32}}));
                    for (side, acc) in [("debit", a), ("credit", b)] {
                        let l = format!("{e}{side}");
                        o.create(&l, Some(fibu), json!({"fibu:JournalLine": {"side": side, "amount": {"amount": amount, "currency": "EUR"}}}));
                        o.link("fibu:JournalLine.entry", l.as_str(), e.as_str()).link("fibu:JournalLine.account", l.as_str(), ledger[acc]);
                    }
                }
                let t = Instant::now();
                match client.order(&o).await {
                    Ok(_) => stats.lat.lock().unwrap().push(t.elapsed()),
                    Err(e) => {
                        if errors.fetch_add(1, Ordering::Relaxed) < 5 {
                            eprintln!("order failed: {e}");
                        }
                    }
                }
                if first % 10_000 < batch && first > 0 {
                    println!("  ... {first} entries");
                }
            }
        }));
    }
    for t in tasks {
        t.await?;
    }
    let secs = t0.elapsed().as_secs_f64();
    let mut lat = stats.lat.lock().unwrap().clone();
    println!(
        "{} journal entries ({} lines) by {} accountants in {:.1} s: {:.0} entries/s; order latency p50 {:.1} ms, p95 {:.1} ms, p99 {:.1} ms; errors {}",
        args.entries,
        args.entries * 2,
        accountants.len(),
        secs,
        args.entries as f64 / secs,
        pct(&mut lat, 0.5).as_secs_f64() * 1000.0,
        pct(&mut lat, 0.95).as_secs_f64() * 1000.0,
        pct(&mut lat, 0.99).as_secs_f64() * 1000.0,
        errors.load(Ordering::Relaxed)
    );

    if let (Some(store), true) = (&_store, std::env::var("BASE_LOADTEST_NO_ANALYZE").is_err()) {
        let t = Instant::now();
        store.client().await.map_err(|e| anyhow::anyhow!("{e}"))?.batch_execute("ANALYZE").await?;
        println!("ANALYZE in {:.1} s", t.elapsed().as_secs_f64());
    }
    // navigation over the date fields, as one of the accountants
    let nav = &accountants[0].1;
    println!("navigation as accountant No. 0:");
    let years = timed("years (postingDate)", nav.group_counts("fibu:JournalEntry", "postingDate", "year", None, None)).await?;
    for y in &years {
        println!("    {} {:>7}", y.key, y.count);
    }
    let y = years.iter().max_by_key(|g| g.count).context("no entries")?;
    let months = timed(&format!("months of {}", y.key), nav.group_counts("fibu:JournalEntry", "postingDate", "month", Some(&format!("{}-01-01", y.key)), Some(&format!("{}-12-31", y.key)))).await?;
    let m = months.iter().max_by_key(|g| g.count).unwrap();
    println!("    {} months, busiest {} with {}", months.len(), m.key, m.count);
    let last = NaiveDate::parse_from_str(&format!("{}-01", m.key), "%Y-%m-%d")?.checked_add_months(chrono::Months::new(1)).unwrap() - CDur::days(1);
    let days = timed(&format!("days of {}", m.key), nav.group_counts("fibu:JournalEntry", "postingDate", "day", Some(&format!("{}-01", m.key)), Some(&last.to_string()))).await?;
    let d = days.iter().max_by_key(|g| g.count).unwrap();
    println!("    {} days, busiest {} with {}", days.len(), d.key, d.count);
    let q = ListQuery { attr: Some("postingDate".into()), eq: Some(d.key.clone()), limit: Some(100), ..Default::default() };
    let day = timed(&format!("entries of {}", d.key), nav.list("fibu:JournalEntry", &q)).await?;
    println!("    first: {}", day.first().map(|s| s.display_name.as_str()).unwrap_or("-"));
    let q = ListQuery { attr: Some("postingDate".into()), from: Some(format!("{}-01", m.key)), to: Some(last.to_string()), limit: Some(100), offset: Some(100), ..Default::default() };
    timed(&format!("entries of {}, page 2 (100..199)", m.key), nav.list("fibu:JournalEntry", &q)).await?;
    let e = day.first().context("empty day")?.id;
    let entry = timed("open entry", nav.get_object(e, None)).await?;
    let lines = timed("its lines (end `lines`)", nav.ends(e, "lines", 10, 0)).await?;
    let acc = timed("account of first line (end `account`)", nav.ends(lines[0].id, "account", 10, 0)).await?;
    println!("    {} -> {} -> {}", entry.display_name("fibu:JournalEntry").unwrap_or(""), lines[0].display_name, acc[0].display_name);
    let acc_lines = timed("lines of that account, first 50 (end `lines`)", nav.ends(acc[0].id, "lines", 50, 0)).await?;
    println!("    {} lines shown", acc_lines.len());
    let q = ListQuery { attr: Some("documentNumber".into()), eq: Some("B-000042".into()), ..Default::default() };
    timed("find by document number", nav.list("fibu:JournalEntry", &q)).await?;
    timed("versions of entry", nav.versions(e)).await?;
    let mut creators = BTreeSet::new();
    for s in day.iter().take(50) {
        if let Some(c) = nav.get_object(s.id, None).await?.created_by {
            creators.insert(c);
        }
    }
    println!("    the {} entries of {} were created by {} different accountants", day.len().min(50), d.key, creators.len());
    if let Some((db, task)) = _keep {
        task.abort();
        db.stop().await?;
    }
    Ok(())
}
