//! Module code as WebAssembly components (milestone 0.6): the example module from
//! `examples/example-module` is compiled to a component, its catalog part built with typecat,
//! and run inside the server: operations, call, impersonate, hooks, abort on error, timeout,
//! activation per tenant and the scheduler.

use i3cm_server::{serve_with, EmbeddedDb};
use i3cm_store::Store;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

static DB_URL: OnceLock<String> = OnceLock::new();
static PG_STOP: std::sync::Mutex<Option<(PathBuf, PathBuf)>> = std::sync::Mutex::new(None);

unsafe extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

extern "C" fn stop_pg() {
    if let Some((pg_ctl, data)) = PG_STOP.lock().ok().and_then(|g| g.clone()) {
        let _ = Command::new(&pg_ctl).args(["stop", "-m", "immediate", "-D"]).arg(&data).output();
        let _ = std::fs::remove_dir_all(&data);
    }
}

fn db_url() -> String {
    DB_URL
        .get_or_init(|| {
            std::thread::spawn(|| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    let db = EmbeddedDb::start(None).await.expect("embedded postgres");
                    let url = db.database("mod_test").await.unwrap();
                    let st = db.pg.settings();
                    *PG_STOP.lock().unwrap() = Some((st.binary_dir().join("pg_ctl"), st.data_dir.clone()));
                    unsafe { atexit(stop_pg) };
                    Box::leak(Box::new(db));
                    url
                })
            })
            .join()
            .unwrap()
        })
        .clone()
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Build the example component and a catalog with the example module; returns (model, wasm dir).
fn example() -> &'static (PathBuf, PathBuf) {
    static EX: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    EX.get_or_init(|| {
        let root = root();
        let ex = root.join("examples/example-module");
        let st = Command::new("cargo")
            .args(["build", "--release", "--target", "wasm32-wasip2", "--manifest-path"])
            .arg(ex.join("ops/Cargo.toml"))
            .status()
            .expect("cargo");
        assert!(st.success(), "building the example component failed (rustup target add wasm32-wasip2)");
        let out = std::env::temp_dir().join(format!("i3cm-mod-test-{}", std::process::id()));
        std::fs::create_dir_all(&out).unwrap();
        let cat = out.join("catalog.json");
        let st = Command::new("node")
            .arg(root.join("tools/typecat/typecat.mjs"))
            .arg(root.join("types"))
            .arg("--module")
            .arg(&ex)
            .arg("--out")
            .arg(&cat)
            .status()
            .expect("node");
        assert!(st.success(), "typecat failed");
        std::fs::copy(ex.join("ops/target/wasm32-wasip2/release/i3cm_example_module.wasm"), out.join("example.wasm")).unwrap();
        (cat, out)
    })
}

struct Env {
    base: String,
    token: String,
    store: Store,
    rt: i3cm_wasm::Runtime,
    tenant: uuid::Uuid,
    http: reqwest::Client,
}

impl Env {
    async fn new() -> Env {
        let (model, dir) = example();
        let store = Store::connect(&db_url(), i3cm_server::load_catalog(model).unwrap(), 16).await.unwrap();
        let rt = i3cm_server::runtime(&store, dir).unwrap();
        let init = store.init_tenant(&format!("m-{}", uuid::Uuid::new_v4()), "admin").await.unwrap();
        let (addr, _t) = serve_with(store.clone(), Some(rt.clone()), "127.0.0.1:0".parse().unwrap()).await.unwrap();
        Env { base: format!("http://{addr}"), token: init.token, store, rt, tenant: init.tenant, http: reqwest::Client::new() }
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.http.post(format!("{}{path}", self.base)).bearer_auth(&self.token).json(&body).send().await.unwrap();
        let code = r.status().as_u16();
        (code, r.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        let r = self.http.get(format!("{}{path}", self.base)).bearer_auth(&self.token).send().await.unwrap();
        let code = r.status().as_u16();
        (code, r.json().await.unwrap_or(Value::Null))
    }

    async fn op(&self, op: &str, params: Value) -> (u16, Value) {
        self.post(&format!("/v1/modules/example/ops/{op}"), params).await
    }

    async fn notes(&self) -> Vec<String> {
        let (code, v) = self.get("/v1/types/example:Note/objects?limit=500").await;
        assert_eq!(code, 200, "{v}");
        v.as_array().unwrap().iter().map(|o| o["displayName"].as_str().unwrap_or_default().to_string()).collect()
    }

    async fn activate(&self) {
        let (code, v) = self.post("/v1/modules/example/activate", json!({})).await;
        assert_eq!(code, 200, "{v}");
    }
}

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
}

#[test]
fn activation_makes_the_module_visible_per_tenant() {
    rt().block_on(async {
        let e = Env::new().await;
        let (code, v) = e.op("greet", json!({})).await;
        assert_eq!((code, v["error_id"].as_str()), (404, Some("module.inactive")));
        let (code, v) = e.post("/v1/orders", json!({"ops": [{"op": "create", "types": {"example:Note": {"title": "x"}}}]})).await;
        assert_eq!((code, v["error_id"].as_str()), (404, Some("module.inactive")), "{v}");
        let (code, v) = e.get("/v1/modules").await;
        assert_eq!(code, 200);
        let ex = v.as_array().unwrap().iter().find(|m| m["name"] == "example").unwrap().clone();
        assert_eq!(ex["active"], false);
        let (code, a) = e.post("/v1/modules/example/activate", json!({})).await;
        assert_eq!(code, 200, "{a}");
        assert_eq!(a["carriers"], json!(["example:system"]));
        assert_eq!(a["profiles"], json!(["example:writer"]));
        // the writer profile reaches the root room: shown red
        assert_eq!(a["rights"][0]["color"], "red");
        let (code, v) = e.post("/v1/modules/example/activate", json!({})).await;
        assert_eq!((code, v["error_id"].as_str()), (409, Some("module.active")));
        let (code, v) = e.op("greet", json!({"name": "Ada"})).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["hello"], "Ada");
        // a second tenant does not see the module
        let other = Env::new().await;
        let (code, _) = other.op("greet", json!({})).await;
        assert_eq!(code, 404);
    });
}

#[test]
fn operations_write_call_and_impersonate_in_one_order() {
    rt().block_on(async {
        let e = Env::new().await;
        e.activate().await;
        let (code, v) = e.op("addNote", json!({"title": "first"})).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["readBack"], v["id"], "module code reads its own uncommitted write");
        assert_eq!(e.notes().await.into_iter().filter(|n| !n.starts_with("tick")).collect::<Vec<_>>(), vec!["first".to_string()]);

        let (code, v) = e.op("callGreet", json!({})).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["hello"], "nested");

        let (code, v) = e.op("stamp", json!({"title": "by system"})).await;
        assert_eq!(code, 200, "{v}");
        assert_ne!(v["as"], v["caller"], "the impersonated handle acts as the module's carrier");
        let sys = e.store.account_by_login(e.tenant, "example:system").await.unwrap();
        assert_eq!(v["as"].as_str().unwrap(), sys.to_string());
        // audit: the order names module and function, the impersonation is recorded
        let c = e.store.client().await.unwrap();
        let row = c
            .query_one("SELECT t.module, t.function, count(a.*) FROM txn t JOIN txn_audit a ON a.tenant_id=t.tenant_id AND a.txn=t.id AND a.action='impersonate' WHERE t.tenant_id=$1 GROUP BY 1,2", &[&e.tenant])
            .await
            .unwrap();
        assert_eq!((row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, i64>(2)), ("example".into(), "stamp".into(), 1));

        let (code, v) = e.op("stampDenied", json!({})).await;
        // the error passes through module code, which returns it as its own result: 400
        assert_eq!((code, v["error_id"].as_str()), (400, Some("module.impersonate_denied")), "{v}");
    });
}

#[test]
fn hooks_errors_and_timeouts_abort_the_whole_order() {
    rt().block_on(async {
        let e = Env::new().await;
        e.activate().await;
        let order = |title: &str| json!({"ops": [{"op": "create", "types": {"example:Note": {"title": title}}}]});
        let (code, v) = e.post("/v1/orders", order("forbidden words")).await;
        assert_eq!((code, v["error_id"].as_str()), (400, Some("example.forbidden_title")), "{v}");
        let (code, v) = e.post("/v1/orders", order("hookwrite")).await;
        assert_eq!(v["error_id"].as_str(), Some("module.read_only"), "{code} {v}");
        let (code, v) = e.post("/v1/orders", order("fine")).await;
        assert_eq!(code, 200, "{v}");
        // a trap after a write rolls the write back
        let (code, v) = e.op("crash", json!({})).await;
        assert_eq!((code, v["error_id"].as_str()), (400, Some("module.trap")), "{v}");
        // an endless loop hits the order deadline
        let (code, v) = e.op("spin", json!({})).await;
        assert_eq!((code, v["error_id"].as_str()), (400, Some("module.timeout")), "{v}");
        // (the scheduler test may add "tick" notes to every tenant with the module)
        assert_eq!(e.notes().await.into_iter().filter(|n| !n.starts_with("tick")).collect::<Vec<_>>(), vec!["fine".to_string()]);
    });
}

#[test]
fn the_scheduler_runs_a_due_operation_exactly_once_with_two_containers() {
    rt().block_on(async {
        let e = Env::new().await;
        e.activate().await;
        let (model, dir) = example();
        // a second "main container": its own store and runtime at the same database
        let store2 = Store::connect(&db_url(), i3cm_server::load_catalog(model).unwrap(), 8).await.unwrap();
        let rt2 = i3cm_server::runtime(&store2, dir).unwrap();
        let (a, b) = tokio::join!(e.rt.tick("one"), rt2.tick("two"));
        let ran = a.unwrap() + b.unwrap();
        let ticks = e.notes().await.into_iter().filter(|n| n.starts_with("tick")).count();
        // other tenants of the test database may also have the module active; this tenant ran once
        assert!(ran >= 1);
        assert_eq!(ticks, 1);
        let again = e.rt.tick("one").await.unwrap() + rt2.tick("two").await.unwrap();
        let _ = again;
        assert_eq!(e.notes().await.into_iter().filter(|n| n.starts_with("tick")).count(), 1, "same period: no second run");
    });
}
