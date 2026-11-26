//! Test environment: embedded PostgreSQL, a server on its own runtime, a tenant with mails.
#![allow(dead_code)]

use base_client::{Client, Order};
use base_server::{serve, EmbeddedDb};
use base_store::Store;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use uuid::Uuid;

static PG_STOP: std::sync::Mutex<Option<(PathBuf, PathBuf)>> = std::sync::Mutex::new(None);
static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static DB_URL: OnceLock<String> = OnceLock::new();

unsafe extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

extern "C" fn stop_pg() {
    if let Some((pg_ctl, data)) = PG_STOP.lock().ok().and_then(|g| g.clone()) {
        let _ = Command::new(&pg_ctl).args(["stop", "-m", "immediate", "-D"]).arg(&data).output();
        let _ = std::fs::remove_dir_all(&data);
    }
}

pub fn rt() -> &'static tokio::runtime::Runtime {
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap())
}

/// `BASE_TEST_DATABASE_URL` points to an existing PostgreSQL; otherwise one runs embedded.
fn db_url() -> String {
    DB_URL
        .get_or_init(|| {
            if let Ok(url) = std::env::var("BASE_TEST_DATABASE_URL") {
                return url;
            }
            rt().block_on(async {
                let db = EmbeddedDb::start(None).await.expect("embedded postgres");
                let url = db.database("base_cli_test").await.unwrap();
                let st = db.pg.settings();
                *PG_STOP.lock().unwrap() = Some((st.binary_dir().join("pg_ctl"), st.data_dir.clone()));
                unsafe { atexit(stop_pg) };
                Box::leak(Box::new(db));
                url
            })
        })
        .clone()
}

pub struct Env {
    pub url: String,
    pub token: String,
    pub admin: Client,
    pub store: Store,
    pub home: PathBuf,
}

/// A server with a fresh tenant; `mails` mails per day over `days` days, two attachments each.
pub fn env(days: usize, mails: usize) -> Env {
    let url = db_url();
    rt().block_on(async move {
        let model = base_server::load_catalog(&base_server::default_catalog()).unwrap();
        let store = Store::connect(&url, model, 16).await.unwrap();
        let init = store.init_tenant(&format!("t-{}", Uuid::new_v4()), "admin").await.unwrap();
        let (addr, _task) = serve(store.clone(), "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let base = format!("http://{addr}");
        let admin = Client::new(&base, &init.token).with_lang("de");
        for d in 0..days {
            let mut o = Order::new();
            for i in 0..mails {
                let m = format!("m{i}");
                let at = format!("2026-11-{:02}T{:02}:15:00Z", 20 + d, 8 + i % 10);
                o.create(&m, None, json!({"base:Mail": {"receivedAt": at, "sender": format!("s{i}@example.org"), "subject": format!("Betreff {d}-{i}"), "recipients": [format!("r{}@example.org", i % 3)]}}));
                for j in 0..2 {
                    let a = format!("a{i}-{j}");
                    o.create(&a, None, json!({"base:MailAttachment": {"filename": format!("file{j}.pdf")}}));
                    o.link("base:MailAttachment.mail", a.as_str(), m.as_str());
                }
            }
            let r = admin.order(&o).await.unwrap();
            for i in 0..mails {
                let m = r.created[&format!("m{i}")];
                admin.put_content(m, format!("From: s{i}\r\n\r\nHallo {d}-{i}").into_bytes(), "message/rfc822", None).await.unwrap();
                admin.put_text(m, "base:mail.md", &format!("# Hallo\n\nRechnung {d}-{i}\n")).await.unwrap();
                for j in 0..2 {
                    admin.put_content(r.created[&format!("a{i}-{j}")], format!("%PDF {d}-{i}-{j}").into_bytes(), "application/pdf", Some(&format!("file{j}.pdf"))).await.unwrap();
                }
            }
        }
        let home = std::env::temp_dir().join(format!("base-cli-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        Env { url: base, token: init.token, admin, store, home }
    })
}

pub fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_base"))
}

impl Env {
    /// Links to the program in `<home>/bin` (as `base links` creates them).
    pub fn links(&self) -> PathBuf {
        let dir = self.home.join("bin");
        let out = Command::new(bin()).args(["links", dir.to_str().unwrap()]).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        dir
    }

    /// Run `name args…` (a link in `<home>/bin`) with the test configuration directory.
    pub fn run(&self, name: &str, args: &[&str], envs: &[(&str, &str)], stdin: Option<&str>) -> Output {
        let dir = self.home.join("bin");
        let mut c = Command::new(dir.join(name));
        c.args(args).env("XDG_CONFIG_HOME", self.home.join("config")).env("HOME", &self.home).env("LANG", "de_DE.UTF-8");
        for (k, v) in envs {
            c.env(k, v);
        }
        if let Some(s) = stdin {
            use std::io::Write;
            c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
            let mut ch = c.spawn().unwrap();
            ch.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
            return ch.wait_with_output().unwrap();
        }
        c.output().unwrap()
    }

    pub fn init(&self) {
        let out = self.run("base-init", &["--server", &self.url, "--tenant", "t"], &[], Some(&format!("{}\n", self.token)));
        assert!(out.status.success(), "{out:?}");
    }

    pub fn config_file(&self) -> PathBuf {
        self.home.join("config/base-erp/config.toml")
    }
}

pub fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn exists(p: &Path) -> bool {
    p.exists()
}
