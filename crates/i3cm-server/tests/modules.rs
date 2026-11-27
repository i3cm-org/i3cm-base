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

#[test]
fn signed_packages_install_and_tampered_ones_are_refused() {
    rt().block_on(async {
        let (_, _) = example();
        let root = root();
        let work = std::env::temp_dir().join(format!("i3cm-pkg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let pack = root.join("tools/typecat/pack.mjs");
        let out = Command::new("node").arg(&pack).arg("keygen").arg(&work).output().unwrap();
        assert!(out.status.success());
        let pubkey = String::from_utf8(out.stdout).unwrap().trim().to_string();
        let wasm = root.join("examples/example-module/ops/target/wasm32-wasip2/release/i3cm_example_module.wasm");
        let st = Command::new("node")
            .arg(&pack)
            .arg(root.join("types"))
            .arg(root.join("examples/example-module"))
            .args(["--wasm"])
            .arg(&wasm)
            .args(["--version", "0.6.0", "--out"])
            .arg(work.join("dist"))
            .args(["--key"])
            .arg(work.join("signing.pem"))
            .status()
            .unwrap();
        assert!(st.success());
        let pkg = work.join("dist/example-0.6.0.tar.gz");
        let mods = work.join("modules");
        std::fs::create_dir_all(&mods).unwrap();
        let base = i3cm_server::default_catalog();
        // a wrong key is refused
        let wrong = "00".repeat(32);
        let e = i3cm_server::install::install(&db_url(), &base, &mods, &pkg, std::slice::from_ref(&wrong)).await.unwrap_err();
        assert!(format!("{e:#}").contains("signature"), "{e:#}");
        // the right key installs: module part, code, tables
        let r = i3cm_server::install::install(&db_url(), &base, &mods, &pkg, std::slice::from_ref(&pubkey)).await.unwrap();
        assert_eq!(r.module, "example");
        assert!(mods.join("example.wasm").exists() && mods.join("example.json").exists());
        let model = i3cm_server::install::load_model(&base, Some(&mods)).unwrap();
        assert!(model.catalog.modules.iter().any(|m| m.name == "example" && !m.operations.is_empty()));
        // a tampered package fails the signature check
        let mut bytes = std::fs::read(&pkg).unwrap();
        let n = bytes.len() / 2;
        bytes[n] ^= 0xff;
        let bad = work.join("dist/example-0.6.1.tar.gz");
        std::fs::write(&bad, &bytes).unwrap();
        std::fs::copy(work.join("dist/example-0.6.0.tar.gz.sig"), work.join("dist/example-0.6.1.tar.gz.sig")).unwrap();
        let e = i3cm_server::install::install(&db_url(), &base, &mods, &bad, std::slice::from_ref(&pubkey)).await.unwrap_err();
        assert!(format!("{e:#}").contains("signature"), "{e:#}");
        // from a registry (file), through the operator surface over HTTP
        let entry: Value = serde_json::from_slice(&std::fs::read(work.join("dist/example-0.6.0.json")).unwrap()).unwrap();
        let reg = work.join("registry.json");
        std::fs::write(&reg, json!({"format": 1, "modules": [{
            "name": "example", "version": "0.6.0", "requires": ["base"], "sha256": entry["sha256"],
            "package": pkg.display().to_string(), "signature": format!("{}.sig", pkg.display())
        }]}).to_string()).unwrap();
        let mods2 = work.join("modules2");
        std::fs::create_dir_all(&mods2).unwrap();
        let store = Store::connect(&db_url(), i3cm_server::load_catalog(&base).unwrap(), 4).await.unwrap();
        let op = i3cm_server::Operator {
            token: "x".repeat(40),
            db_url: db_url(),
            catalog: base.clone(),
            modules_dir: mods2.clone(),
            registry: Some(reg.display().to_string()),
            trusted: vec![pubkey.clone()],
        };
        let (addr, _t) = i3cm_server::serve_full(store, None, Some(op), "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let http = reqwest::Client::new();
        let r = http.post(format!("http://{addr}/v1/operator/modules/install")).bearer_auth("wrong").json(&json!({"name": "example"})).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 401);
        let r = http.get(format!("http://{addr}/v1/operator/registry")).bearer_auth("x".repeat(40)).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 200);
        let r = http.post(format!("http://{addr}/v1/operator/modules/install")).bearer_auth("x".repeat(40)).json(&json!({"name": "example", "version": "0.6.0"})).send().await.unwrap();
        let code = r.status().as_u16();
        assert_eq!(code, 200, "{}", r.text().await.unwrap());
        assert!(mods2.join("example.wasm").exists());
        // a registry entry with a wrong checksum is refused before anything is installed
        std::fs::write(&reg, json!({"format": 1, "modules": [{
            "name": "example", "version": "0.6.0", "sha256": "00", "package": pkg.display().to_string(), "signature": format!("{}.sig", pkg.display())
        }]}).to_string()).unwrap();
        let e = i3cm_server::install::install_from_registry(&db_url(), &base, &mods2, &reg.display().to_string(), "example", None, std::slice::from_ref(&pubkey)).await.unwrap_err();
        assert!(format!("{e:#}").contains("checksum"), "{e:#}");
        // the signed package unpacks with all listed files
        let files = i3cm_server::install::verify(&std::fs::read(&pkg).unwrap(), &std::fs::read_to_string(work.join("dist/example-0.6.0.tar.gz.sig")).unwrap(), std::slice::from_ref(&pubkey)).unwrap();
        assert!(files.contains_key("module.wasm") && files.contains_key("module.json"));
    });
}

/// Views of the tasks module (repository `i3cm-tasks` next to the kernel checkout); skipped when
/// that checkout is not present.
#[test]
fn task_views_mine_overdue_and_deadlines() {
    let tasks = root().join("../../i3cm-tasks/product");
    if !tasks.join("i3cm/module.ts").exists() {
        eprintln!("skipped: {} not checked out", tasks.display());
        return;
    }
    rt().block_on(async {
        let out = std::env::temp_dir().join(format!("i3cm-tasks-test-{}", std::process::id()));
        std::fs::create_dir_all(&out).unwrap();
        let cat = out.join("catalog.json");
        let st = Command::new("node").arg(root().join("tools/typecat/typecat.mjs")).arg(root().join("types")).arg("--module").arg(&tasks).arg("--out").arg(&cat).status().unwrap();
        assert!(st.success());
        let store = Store::connect(&db_url(), i3cm_server::load_catalog(&cat).unwrap(), 8).await.unwrap();
        let init = store.init_tenant(&format!("tk-{}", uuid::Uuid::new_v4()), "admin").await.unwrap();
        let (addr, _t) = serve_with(store.clone(), None, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let http = reqwest::Client::new();
        let base = format!("http://{addr}");
        let post = |path: &str, body: Value| {
            let r = http.post(format!("{base}{path}")).bearer_auth(&init.token).json(&body).send();
            async move {
                let r = r.await.unwrap();
                let code = r.status().as_u16();
                (code, r.json::<Value>().await.unwrap_or(Value::Null))
            }
        };
        // tasks is mandatory: active without activation
        let me = init.admin_account.to_string();
        let (code, v) = post("/v1/orders", json!({"ops": [
            {"op": "create", "ref": "other", "types": {"base:Person": {"lastName": "Other"}, "base:PersonAccount": {"login": "other"}}},
            {"op": "create", "ref": "t1", "types": {"tasks:Task": {"subject": "late", "status": "open", "due": "2020-01-01T10:00:00Z"}}},
            {"op": "create", "ref": "t2", "types": {"tasks:Task": {"subject": "finished", "status": "done", "due": "2020-01-01T10:00:00Z"}}},
            {"op": "create", "ref": "t3", "types": {"tasks:Task": {"subject": "not mine", "status": "open"}}},
            {"op": "create", "ref": "t4", "types": {"tasks:Task": {"subject": "later", "status": "in_progress", "due": "2999-01-01T10:00:00Z"}}},
            {"op": "create", "ref": "t5", "types": {"tasks:Task": {"subject": "court", "status": "open", "due": "2999-02-01T10:00:00Z"}, "tasks:Deadline": {"direction": "against_us"}}},
            {"op": "link", "reference": "tasks:Task.assignees", "from": "t1", "to": me},
            {"op": "link", "reference": "tasks:Task.assignees", "from": "t2", "to": me},
            {"op": "link", "reference": "tasks:Task.assignees", "from": "t3", "to": "other"},
            {"op": "link", "reference": "tasks:Task.assignees", "from": "t4", "to": me},
            {"op": "link", "reference": "tasks:Task.assignees", "from": "t5", "to": me}
        ]})).await;
        assert_eq!(code, 200, "{v}");
        let names = |aspect: &'static str| {
            let http = http.clone();
            let base = base.clone();
            let token = init.token.clone();
            async move {
                let get = |node: String| {
                    let r = http.get(format!("{base}/v1/aspects/{aspect}/children")).query(&[("node", node)]).bearer_auth(&token).send();
                    async move { r.await.unwrap().json::<Value>().await.unwrap() }
                };
                let mut out = vec![];
                for g in get(String::new()).await.as_array().cloned().unwrap_or_default() {
                    for o in get(g["node"].as_str().unwrap().to_string()).await.as_array().cloned().unwrap_or_default() {
                        if !o["file"].as_bool().unwrap_or(false) {
                            out.push(o["displayName"].as_str().unwrap_or_default().to_string());
                        }
                    }
                }
                out.sort();
                out
            }
        };
        let mine = names("tasks:mine").await;
        assert!(mine.iter().any(|n| n.contains("late")) && mine.iter().any(|n| n.contains("later")) && mine.iter().any(|n| n.contains("court")), "{mine:?}");
        assert!(!mine.iter().any(|n| n.contains("finished") || n.contains("not mine")), "only open tasks of the account: {mine:?}");
        let overdue = names("tasks:overdue").await;
        assert_eq!(overdue.len(), 1, "{overdue:?}");
        assert!(overdue[0].contains("late"));
        let deadlines = names("tasks:deadlines").await;
        // exactly the task that carries tasks:Deadline (its display name comes from the module's texts)
        assert_eq!(deadlines.len(), 1, "{deadlines:?}");
    });
}

#[test]
fn mandatory_modules_of_the_image_are_active_in_new_tenants() {
    rt().block_on(async {
        // the kernel catalog plus deploy/modules (tasks, dms), as the image starts
        let kernel = root().join("target/kernel-catalog.json");
        let st = Command::new("node").arg(root().join("tools/typecat/typecat.mjs")).arg(root().join("types")).arg("--out").arg(&kernel).stdout(std::process::Stdio::null()).status().unwrap();
        assert!(st.success());
        let model = i3cm_server::install::load_model(&kernel, Some(&root().join("deploy/modules"))).unwrap();
        assert!(model.type_def("base:Mail").is_none() && model.type_def("base:Task").is_none(), "no business types in the kernel");
        let store = Store::connect(&db_url(), model, 8).await.unwrap();
        let init = store.init_tenant(&format!("mand-{}", uuid::Uuid::new_v4()), "admin").await.unwrap();
        let ctx = store.ctx(init.tenant, init.admin_account).await.unwrap();
        // tasks and dms are active without activation; only the optional fibu example is not
        assert_eq!(ctx.inactive.iter().cloned().collect::<Vec<_>>(), vec!["fibu".to_string()]);
        let (addr, _t) = serve_with(store.clone(), None, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let http = reqwest::Client::new();
        let r = http
            .post(format!("http://{addr}/v1/orders"))
            .bearer_auth(&init.token)
            .json(&json!({"ops": [
                {"op": "create", "types": {"tasks:Task": {"subject": "first", "status": "open"}}},
                {"op": "create", "types": {"dms:Document": {"title": "Statutes"}}}
            ]}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 200, "{}", r.text().await.unwrap());
        let mods: Value = http.get(format!("http://{addr}/v1/modules")).bearer_auth(&init.token).send().await.unwrap().json().await.unwrap();
        for m in ["tasks", "dms"] {
            let e = mods.as_array().unwrap().iter().find(|x| x["name"] == m).unwrap();
            assert_eq!((e["mandatory"].as_bool(), e["active"].as_bool()), (Some(true), Some(true)), "{m}");
        }
    });
}

/// Aspects of a module appear only once it is active (mail from `i3cm-mail` next to the kernel
/// checkout); skipped when the module checkouts are not present.
#[test]
fn aspects_of_inactive_modules_are_hidden() {
    let dms = root().join("../../i3cm-dms/product");
    let mail = root().join("../../i3cm-mail/product");
    if !mail.join("i3cm/module.ts").exists() || !dms.join("i3cm/module.ts").exists() {
        eprintln!("skipped: module checkouts not present");
        return;
    }
    rt().block_on(async {
        let cat = std::env::temp_dir().join(format!("i3cm-mail-test-{}.json", std::process::id()));
        let st = Command::new("node").arg(root().join("tools/typecat/typecat.mjs")).arg(root().join("types")).args(["--module"]).arg(&dms).args(["--module"]).arg(&mail).arg("--out").arg(&cat).stdout(std::process::Stdio::null()).status().unwrap();
        assert!(st.success());
        let store = Store::connect(&db_url(), i3cm_server::load_catalog(&cat).unwrap(), 8).await.unwrap();
        let init = store.init_tenant(&format!("mail-{}", uuid::Uuid::new_v4()), "admin").await.unwrap();
        let (addr, _t) = serve_with(store.clone(), None, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let http = reqwest::Client::new();
        let names = || async {
            let v: Value = http.get(format!("http://{addr}/v1/aspects")).bearer_auth(&init.token).send().await.unwrap().json().await.unwrap();
            v.as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        };
        assert!(!names().await.iter().any(|n| n.starts_with("mail:")), "mail is not active yet");
        let r = http.get(format!("http://{addr}/v1/aspects/mail:mailbox/children")).bearer_auth(&init.token).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 404);
        let r = http.post(format!("http://{addr}/v1/modules/mail/activate")).bearer_auth(&init.token).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 200, "{}", r.text().await.unwrap());
        assert!(names().await.iter().any(|n| n == "mail:mailbox"));
    });
}
