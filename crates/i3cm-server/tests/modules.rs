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

static DB: OnceLock<&'static EmbeddedDb> = OnceLock::new();
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

/// A database of the shared embedded server. Tests with different catalogs use different
/// databases: migrating one catalog takes table locks that would deadlock orders of another
/// test running at the same time.
fn db_named(name: &str) -> String {
    let name = name.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let db = *DB.get_or_init(|| {
                std::thread::spawn(|| {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let db = EmbeddedDb::start(None).await.expect("embedded postgres");
                        let st = db.pg.settings();
                        *PG_STOP.lock().unwrap() = Some((st.binary_dir().join("pg_ctl"), st.data_dir.clone()));
                        unsafe { atexit(stop_pg) };
                        &*Box::leak(Box::new(db))
                    })
                })
                .join()
                .unwrap()
            });
            db.database(&name).await.unwrap()
        })
    })
    .join()
    .unwrap()
}

/// The database of the example catalog.
fn db_url() -> String {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| db_named("mod_test")).clone()
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
        assert_eq!(a["profiles"], json!(["example:writer", "example:roomReader"]));
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
fn after_hooks_write_in_the_same_order() {
    rt().block_on(async {
        let e = Env::new().await;
        e.activate().await;
        let (code, v) = e.post("/v1/orders", json!({"ops": [{"op": "create", "ref": "n", "types": {"example:Note": {"title": "report", "status": "open"}}}]})).await;
        assert_eq!(code, 200, "{v}");
        let id = v["created"]["n"].as_str().unwrap().to_string();
        let txn = v["txn"].as_i64().unwrap();
        // the hook stamps the note and writes a log note, in the same transaction as the update
        let (code, v) = e.post("/v1/orders", json!({"ops": [{"op": "update", "id": id, "types": {"example:Note": {"status": "done"}}}]})).await;
        assert_eq!(code, 200, "{v}");
        let upd = v["txn"].as_i64().unwrap();
        assert!(upd > txn);
        let (_, note) = e.get(&format!("/v1/objects/{id}")).await;
        let attrs = &note["parts"][0]["attributes"];
        assert_eq!(attrs["status"], "done");
        assert!(attrs["doneAt"].is_string(), "{note}");
        assert!(e.notes().await.iter().any(|n| n == "log: report done"));
        let c = e.store.client().await.unwrap();
        let txns: Vec<i64> = c
            .query("SELECT DISTINCT t.id FROM txn t WHERE t.tenant_id=$1 AND t.id > $2", &[&e.tenant, &txn])
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(txns, vec![upd], "update, stamp and log note are one transaction");
        // a failing after hook rolls back the whole order, the update included
        let (code, v) = e.post("/v1/orders", json!({"ops": [{"op": "create", "ref": "f", "types": {"example:Note": {"title": "will fail", "status": "open"}}}]})).await;
        assert_eq!(code, 200, "{v}");
        let fid = v["created"]["f"].as_str().unwrap().to_string();
        let (code, v) = e.post("/v1/orders", json!({"ops": [{"op": "update", "id": fid, "types": {"example:Note": {"status": "done"}}}]})).await;
        assert_eq!((code, v["error_id"].as_str()), (400, Some("example.after_failed")), "{v}");
        let (_, note) = e.get(&format!("/v1/objects/{fid}")).await;
        assert_eq!(note["parts"][0]["attributes"]["status"], "open", "rolled back");
        // a hook that writes on every update ends at the depth limit with an error
        let (code, v) = e.post("/v1/orders", json!({"ops": [{"op": "create", "ref": "l", "types": {"example:Note": {"title": "loop", "author": "a"}}}]})).await;
        assert_eq!(code, 200, "{v}");
        let lid = v["created"]["l"].as_str().unwrap().to_string();
        let (code, v) = e.post("/v1/orders", json!({"ops": [{"op": "update", "id": lid, "types": {"example:Note": {"author": "b"}}}]})).await;
        assert_eq!((code, v["error_id"].as_str()), (400, Some("module.depth")), "{v}");
        let (_, note) = e.get(&format!("/v1/objects/{lid}")).await;
        assert_eq!(note["parts"][0]["attributes"]["author"], "a", "rolled back");
    });
}

#[test]
fn module_queries_page_through_filter_expressions_and_read_references() {
    rt().block_on(async {
        let e = Env::new().await;
        e.activate().await;
        // 230 open notes due in the past, 20 done, 10 open in the future
        let mut ops = vec![];
        for i in 0..260 {
            let (status, due) = if i < 230 { ("open", "2020-01-01T00:00:00Z") } else if i < 250 { ("done", "2020-01-01T00:00:00Z") } else { ("open", "2999-01-01T00:00:00Z") };
            ops.push(json!({"op": "create", "types": {"example:Note": {"title": format!("q{i}"), "status": status, "due": due}}}));
        }
        let (code, v) = e.post("/v1/orders", json!({"ops": ops})).await;
        assert_eq!(code, 200, "{v}");
        let (code, v) = e.op("findOpen", json!({"where": "due < now() and status != 'done'", "limit": 50})).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["count"], 230, "all matches, beyond 200");
        assert_eq!(v["pages"], 5);
        // references: a parent with two children, one of them in a room the reader cannot see
        let (code, v) = e.post("/v1/orders", json!({"ops": [
            {"op": "create", "ref": "p", "types": {"example:Note": {"title": "parent"}}},
            {"op": "create", "ref": "c1", "types": {"example:Note": {"title": "child one"}}},
            {"op": "create", "ref": "c2", "types": {"example:Note": {"title": "child two"}}},
            {"op": "link", "reference": "example:Note.parent", "from": "c1", "to": "p"},
            {"op": "link", "reference": "example:Note.parent", "from": "c2", "to": "p"}
        ]})).await;
        assert_eq!(code, 200, "{v}");
        let p = v["created"]["p"].as_str().unwrap().to_string();
        let c1 = v["created"]["c1"].as_str().unwrap().to_string();
        let (code, v) = e.op("refs", json!({"id": p})).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["references"]["children"].as_array().map(|a| a.len()), Some(2), "{v}");
        assert_eq!(v["children"].as_array().map(|a| a.len()), Some(2), "{v}");
        // a reader who may read only the module room does not see the child in the root room
        let (_, rooms) = e.get("/v1/rooms").await;
        let room = rooms["example"].as_str().unwrap().to_string();
        let (code, v) = e.post("/v1/orders", json!({"ops": [
            {"op": "move", "id": p, "room": room},
            {"op": "move", "id": c1, "room": room},
            {"op": "create", "ref": "bob", "types": {"base:Person": {"lastName": "Bob"}, "base:PersonAccount": {"login": "bob"}}}
        ]})).await;
        assert_eq!(code, 200, "{v}");
        let bob = v["created"]["bob"].as_str().unwrap().to_string();
        let (code, v) = e.post("/v1/assignments", json!({"profile": "example:roomReader", "carrier": bob})).await;
        assert_eq!(code, 200, "{v}");
        let (code, t) = e.post("/v1/tokens", json!({"account": bob, "label": "test", "ttl": 1})).await;
        assert_eq!(code, 200, "{t}");
        let r = e.http.post(format!("{}/v1/modules/example/ops/refs", e.base)).bearer_auth(t["token"].as_str().unwrap()).json(&json!({"id": p})).send().await.unwrap();
        let code = r.status().as_u16();
        let v: Value = r.json().await.unwrap();
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["references"]["children"], json!([c1]), "{v}");
        assert_eq!(v["children"], json!([c1]), "{v}");
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
        // FREQ=MINUTELY: keep both ticks (and the repeated pair below) inside one minute
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() % 60;
        if secs > 50 {
            tokio::time::sleep(std::time::Duration::from_secs(61 - secs)).await;
        }
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
        let pkg_db = db_named("pkg_test");
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
        let e = i3cm_server::install::install(&pkg_db, &base, &mods, &pkg, std::slice::from_ref(&wrong)).await.unwrap_err();
        assert!(format!("{e:#}").contains("signature"), "{e:#}");
        // the right key installs: module part, code, tables
        let r = i3cm_server::install::install(&pkg_db, &base, &mods, &pkg, std::slice::from_ref(&pubkey)).await.unwrap();
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
        let e = i3cm_server::install::install(&pkg_db, &base, &mods, &bad, std::slice::from_ref(&pubkey)).await.unwrap_err();
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
        let store = Store::connect(&pkg_db, i3cm_server::load_catalog(&base).unwrap(), 4).await.unwrap();
        let op = i3cm_server::Operator {
            token: "x".repeat(40),
            db_url: pkg_db.clone(),
            catalog: base.clone(),
            modules_dir: mods2.clone(),
            registry: Some(reg.display().to_string()),
            trust: i3cm_server::install::Trust::keys(std::slice::from_ref(&pubkey)),
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
        let e = i3cm_server::install::install_from_registry(&pkg_db, &base, &mods2, &reg.display().to_string(), "example", None, std::slice::from_ref(&pubkey)).await.unwrap_err();
        assert!(format!("{e:#}").contains("checksum"), "{e:#}");
        // the signed package unpacks with all listed files
        let files = i3cm_server::install::verify(&std::fs::read(&pkg).unwrap(), &std::fs::read_to_string(work.join("dist/example-0.6.0.tar.gz.sig")).unwrap(), std::slice::from_ref(&pubkey)).unwrap();
        assert!(files.contains_key("module.wasm") && files.contains_key("module.json"));
    });
}

#[test]
fn trust_built_in_and_operator_keys_and_unsigned_packages_on_development_instances() {
    use i3cm_server::install::{verify_with, Trust};
    let (_, _) = example();
    let root = root();
    let work = std::env::temp_dir().join(format!("i3cm-trust-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let pack = root.join("tools/typecat/pack.mjs");
    let keygen = |dir: &Path| {
        let out = Command::new("node").arg(&pack).arg("keygen").arg(dir).output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let (org_dir, own_dir) = (work.join("org"), work.join("own"));
    let (org, own) = (keygen(&org_dir), keygen(&own_dir));
    let build = |key: &Path, out: &str| {
        let st = Command::new("node")
            .arg(&pack)
            .arg(root.join("types"))
            .arg(root.join("examples/example-module"))
            .arg("--wasm")
            .arg(root.join("examples/example-module/ops/target/wasm32-wasip2/release/i3cm_example_module.wasm"))
            .args(["--version", "0.8.0", "--out"])
            .arg(work.join(out))
            .arg("--key")
            .arg(key.join("signing.pem"))
            .status()
            .unwrap();
        assert!(st.success());
        let pkg = std::fs::read(work.join(out).join("example-0.8.0.tar.gz")).unwrap();
        let sig = std::fs::read_to_string(work.join(out).join("example-0.8.0.tar.gz.sig")).unwrap();
        (pkg, sig)
    };
    let (org_pkg, org_sig) = build(&org_dir, "org");
    let (own_pkg, own_sig) = build(&own_dir, "own");
    // the built-in key file (comments allowed) makes org packages trusted without further keys
    let builtin = format!("# i3cm-org\n{org}  # release key\n");
    let plain = Trust::build(&builtin, &[], "", None, false);
    assert_eq!(plain.keys, vec![org.clone()]);
    assert!(verify_with(&org_pkg, Some(&org_sig), &plain).is_ok());
    // a self-signed package is refused until the operator adds the key (list or key file)
    assert!(verify_with(&own_pkg, Some(&own_sig), &plain).is_err());
    let with_file = Trust::build(&builtin, &[], "", Some(&format!("# own modules\n{own}\n")), false);
    assert!(verify_with(&own_pkg, Some(&own_sig), &with_file).is_ok());
    assert!(verify_with(&org_pkg, Some(&org_sig), &with_file).is_ok(), "operator keys add to the built-in key");
    let with_list = Trust::build(&builtin, &[], &format!(" {own} ,"), None, false);
    assert!(verify_with(&own_pkg, Some(&own_sig), &with_list).is_ok());
    // unsigned: refused, except on a development instance (with a warning); checksums still count
    let e = verify_with(&own_pkg, None, &plain).unwrap_err();
    assert!(format!("{e:#}").contains("no signature"), "{e:#}");
    let dev = Trust::build(&builtin, &[], "", None, true);
    let (files, warnings) = verify_with(&own_pkg, None, &dev).unwrap();
    assert!(files.contains_key("module.wasm"));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let (_, warnings) = verify_with(&own_pkg, Some(&own_sig), &dev).unwrap();
    assert_eq!(warnings.len(), 1, "not trusted, accepted with a warning");
    let (_, warnings) = verify_with(&org_pkg, Some(&org_sig), &dev).unwrap();
    assert!(warnings.is_empty());
    let mut bad = own_pkg.clone();
    let n = bad.len() / 2;
    bad[n] ^= 0xff;
    assert!(verify_with(&bad, None, &dev).is_err(), "a damaged package fails even on a development instance");
    // the shipped key file parses (no key until i3cm-org publishes one)
    let _ = Trust::builtin();
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
        let store = Store::connect(&db_named("tasks_test"), i3cm_server::load_catalog(&cat).unwrap(), 8).await.unwrap();
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
        // a task with the facet tasks:Deadline is named by its host type (subject), not by the facet
        let r = http.get(format!("{base}/v1/objects/{me}/ends/assignedTasks")).bearer_auth(&init.token).send().await.unwrap();
        let ends: Value = r.json().await.unwrap();
        let court = ends.as_array().unwrap().iter().find(|o| o["types"].as_array().unwrap().iter().any(|t| t == "tasks:Deadline")).cloned().unwrap_or(Value::Null);
        assert_eq!(court["displayName"], "court", "{ends}");
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
        let store = Store::connect(&db_named("image_test"), model, 8).await.unwrap();
        let init = store.init_tenant(&format!("mand-{}", uuid::Uuid::new_v4()), "admin").await.unwrap();
        let ctx = store.ctx(init.tenant, init.admin_account).await.unwrap();
        // tasks and dms are active without activation; only the optional fibu example is not
        assert_eq!(ctx.inactive.iter().cloned().collect::<Vec<_>>(), vec!["fibu".to_string()]);
        // the code of tasks comes with the image (deploy/modules/tasks.wasm)
        let rt = i3cm_server::runtime(&store, &root().join("deploy/modules")).unwrap();
        let (addr, _t) = serve_with(store.clone(), Some(rt), "127.0.0.1:0".parse().unwrap()).await.unwrap();
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
        let code = r.status().as_u16();
        let created: Value = r.json().await.unwrap();
        assert_eq!(code, 200, "{created}");
        // the operation of the bundled code runs in the new tenant
        let task = created["created"].as_object().and_then(|m| m.values().next()).and_then(|v| v.as_str()).map(String::from);
        let task = match task {
            Some(t) => t,
            None => {
                let v: Value = http.get(format!("http://{addr}/v1/types/tasks:Task/objects")).bearer_auth(&init.token).send().await.unwrap().json().await.unwrap();
                v[0]["id"].as_str().unwrap().to_string()
            }
        };
        let r = http.post(format!("http://{addr}/v1/modules/tasks/ops/complete")).bearer_auth(&init.token).json(&json!({"id": task})).send().await.unwrap();
        let code = r.status().as_u16();
        let v: Value = r.json().await.unwrap();
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["changed"], true, "{v}");
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
        let store = Store::connect(&db_named("mail_test"), i3cm_server::load_catalog(&cat).unwrap(), 8).await.unwrap();
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

/// A catalog with the example module made mandatory, optionally changed by `edit`.
fn example_catalog_variant(tag: &str, edit: impl Fn(&mut Value)) -> PathBuf {
    let (model, _) = example();
    let mut cat: Value = serde_json::from_str(&std::fs::read_to_string(model).unwrap()).unwrap();
    let ex = cat["modules"].as_array_mut().unwrap().iter_mut().find(|m| m["name"] == "example").unwrap();
    ex["mandatory"] = json!(true);
    edit(ex);
    let p = model.with_file_name(format!("catalog-{tag}.json"));
    std::fs::write(&p, cat.to_string()).unwrap();
    p
}

#[test]
fn new_tenants_get_profiles_and_carriers_of_mandatory_modules_and_updates_are_synced() {
    rt().block_on(async {
        let (_, dir) = example();
        let url = db_named("sync_test");
        // a mandatory module with a carrier: a new tenant has its profiles and its carrier
        let v1 = example_catalog_variant("mand-v1", |_| {});
        let store = Store::connect(&url, i3cm_server::load_catalog(&v1).unwrap(), 8).await.unwrap();
        let init = store.init_tenant(&format!("sync-{}", uuid::Uuid::new_v4()), "admin").await.unwrap();
        let c = store.client().await.unwrap();
        let profiles: Vec<String> = c
            .query("SELECT name FROM profile WHERE tenant_id=$1 AND name LIKE 'example:%' ORDER BY name", &[&init.tenant])
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(profiles, vec!["example:roomReader".to_string(), "example:writer".to_string()]);
        let sys = store.module_carrier(init.tenant, "example", "example:system").await.expect("carrier of the mandatory module");
        assert!(store.module_active(init.tenant, "example").await.unwrap());
        assert!(!store.module_active(init.tenant, "fibu").await.unwrap(), "an optional module is not active in a new tenant");
        // an operation that impersonates the carrier runs without any activation
        let rt = i3cm_server::runtime(&store, dir).unwrap();
        let (addr, _t) = serve_with(store.clone(), Some(rt), "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let r = reqwest::Client::new().post(format!("http://{addr}/v1/modules/example/ops/stamp")).bearer_auth(&init.token).json(&json!({"title": "x"})).send().await.unwrap();
        let code = r.status().as_u16();
        let v: Value = r.json().await.unwrap();
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["as"].as_str().unwrap(), sys.to_string());
        // nothing to do right after the creation
        let s = store.sync_modules(init.tenant).await.unwrap();
        assert!(s.changes.is_empty() && s.warnings.is_empty(), "{s:?}");

        // a new version: one profile more, one less, changed rights, a second carrier
        let v2 = example_catalog_variant("mand-v2", |ex| {
            let profiles = ex["profiles"].as_array_mut().unwrap();
            profiles.retain(|p| p["name"] != "example:roomReader");
            profiles[0]["rights"][0]["art"] = json!(["read", "write"]);
            profiles.push(json!({"name": "example:auditor", "rights": [{"sign": "+", "art": ["read"], "types": ["example:Note"], "room": "example"}]}));
            ex["carriers"].as_array_mut().unwrap().push(json!({"name": "example:helper", "profiles": ["example:auditor"]}));
        });
        let store2 = Store::connect(&url, i3cm_server::load_catalog(&v2).unwrap(), 4).await.unwrap();
        let s = store2.sync_modules(init.tenant).await.unwrap();
        assert!(s.changes.contains(&"example: profile example:auditor created".to_string()), "{s:?}");
        assert!(s.changes.contains(&"example: profile example:writer updated".to_string()), "{s:?}");
        assert!(s.changes.contains(&"example: carrier example:helper created".to_string()), "{s:?}");
        assert_eq!(s.warnings, vec!["example: profile example:roomReader is no longer defined by the module (kept)".to_string()]);
        let arts: Vec<String> = c.query_one("SELECT arts FROM profile_right WHERE tenant_id=$1 AND profile='example:writer' AND idx=0", &[&init.tenant]).await.unwrap().get(0);
        assert_eq!(arts, vec!["read".to_string(), "write".to_string()]);
        let helper = store2.module_carrier(init.tenant, "example", "example:helper").await.unwrap();
        let n: i64 = c
            .query_one("SELECT count(*) FROM assignment WHERE tenant_id=$1 AND carrier=$2 AND profile='example:auditor' AND valid_to IS NULL", &[&init.tenant, &helper])
            .await
            .unwrap()
            .get(0);
        assert_eq!(n, 1);
        // idempotent
        let s = store2.sync_modules(init.tenant).await.unwrap();
        assert!(s.changes.is_empty(), "{s:?}");
        assert_eq!(s.warnings.len(), 1);
        // two containers starting at once with a third version: one creates the carrier, neither fails
        let v3 = example_catalog_variant("mand-v3", |ex| {
            ex["carriers"].as_array_mut().unwrap().push(json!({"name": "example:third", "profiles": ["example:writer"]}));
        });
        let a = Store::connect(&url, i3cm_server::load_catalog(&v3).unwrap(), 4).await.unwrap();
        let b = Store::connect(&url, i3cm_server::load_catalog(&v3).unwrap(), 4).await.unwrap();
        let (ra, rb) = tokio::join!(a.sync_modules_all(), b.sync_modules_all());
        let created = [ra.unwrap(), rb.unwrap()]
            .iter()
            .flatten()
            .filter(|s| s.tenant == init.tenant)
            .flat_map(|s| s.changes.clone())
            .filter(|c| c == "example: carrier example:third created")
            .count();
        assert_eq!(created, 1);
    });
}

#[test]
fn hooks_run_on_every_write_path_not_only_orders() {
    rt().block_on(async {
        let e = Env::new().await;
        e.activate().await;
        // the single-operation shortcut PATCH /v1/objects/{id} runs the after hook as an order does
        let (code, v) = e.post("/v1/objects", json!({"types": {"example:Note": {"title": "patched", "status": "open"}}})).await;
        assert_eq!(code, 200, "{v}");
        let id = v["created"]["new"].as_str().unwrap().to_string();
        let r = e.http.patch(format!("{}/v1/objects/{id}", e.base)).bearer_auth(&e.token).json(&json!({"types": {"example:Note": {"status": "done"}}})).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 200, "{}", r.text().await.unwrap());
        let (_, note) = e.get(&format!("/v1/objects/{id}")).await;
        assert!(note["parts"][0]["attributes"]["doneAt"].is_string(), "{note}");
        assert!(e.notes().await.iter().any(|n| n == "log: patched done"));
        // a failing after hook rolls the shortcut back as well
        let (_, v) = e.post("/v1/objects", json!({"types": {"example:Note": {"title": "will fail", "status": "open"}}})).await;
        let fid = v["created"]["new"].as_str().unwrap().to_string();
        let r = e.http.patch(format!("{}/v1/objects/{fid}", e.base)).bearer_auth(&e.token).json(&json!({"types": {"example:Note": {"status": "done"}}})).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 400);
        let (_, note) = e.get(&format!("/v1/objects/{fid}")).await;
        assert_eq!(note["parts"][0]["attributes"]["status"], "open", "rolled back");
    });
}

#[test]
fn filter_hooks_fire_on_enter_and_leave_and_content_hooks_on_every_write_path() {
    rt().block_on(async {
        let e = Env::new().await;
        e.activate().await;
        let count = |notes: &[String], what: &str, id: &str| notes.iter().filter(|n| *n == &format!("{what} {id}")).count();
        let create = |title: &str, status: &str| json!({"ops": [{"op": "create", "ref": "n", "types": {"example:Note": {"title": title, "status": status}}}]});
        // created inside the filter: enter; created outside: nothing
        let (code, v) = e.post("/v1/orders", create("born done", "done")).await;
        assert_eq!(code, 200, "{v}");
        let born = v["created"]["n"].as_str().unwrap().to_string();
        let (_, v) = e.post("/v1/orders", create("plain", "open")).await;
        let id = v["created"]["n"].as_str().unwrap().to_string();
        let notes = e.notes().await;
        assert_eq!(count(&notes, "enter", &born), 1, "{notes:?}");
        assert_eq!(count(&notes, "enter", &id), 0, "{notes:?}");
        let upd = |id: &str, attrs: Value| json!({"ops": [{"op": "update", "id": id, "types": {"example:Note": attrs}}]});
        // open -> done: enter exactly once (the after hook writing doneAt keeps it inside)
        let (code, v) = e.post("/v1/orders", upd(&id, json!({"status": "done"}))).await;
        assert_eq!(code, 200, "{v}");
        // a change inside the filter: nothing
        let (code, v) = e.post("/v1/orders", upd(&id, json!({"author": "x"}))).await;
        assert_eq!(code, 200, "{v}");
        let notes = e.notes().await;
        assert_eq!((count(&notes, "enter", &id), count(&notes, "leave", &id)), (1, 0), "{notes:?}");
        // done -> open: leave
        let (code, v) = e.post("/v1/orders", upd(&id, json!({"status": "open"}))).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(count(&e.notes().await, "leave", &id), 1);
        // out and back in within one order: no transition (state at first touch against the end)
        let (code, v) = e.post("/v1/orders", json!({"ops": [
            {"op": "update", "id": born, "types": {"example:Note": {"status": "open"}}},
            {"op": "update", "id": born, "types": {"example:Note": {"status": "done"}}}]})).await;
        assert_eq!(code, 200, "{v}");
        let notes = e.notes().await;
        assert_eq!((count(&notes, "enter", &born), count(&notes, "leave", &born)), (1, 0), "{notes:?}");
        // deleting an object inside the filter: leave
        let (code, v) = e.post("/v1/orders", json!({"ops": [{"op": "delete", "id": born}]})).await;
        assert_eq!(code, 200, "{v}");
        let notes = e.notes().await;
        assert_eq!(count(&notes, "leave", &born), 1, "{notes:?}");

        // content and text hooks: order, PUT .../content, PUT .../texts, the tools (FUSE path)
        let (code, v) = e.post("/v1/orders", json!({"ops": [
            {"op": "create", "ref": "d", "types": {"example:Doc": {"name": "doc"}}},
            {"op": "content", "id": "d", "mediaType": "text/plain", "filename": "a.txt", "base64": "YQ=="}]})).await;
        assert_eq!(code, 200, "{v}");
        let doc = v["created"]["d"].as_str().unwrap().to_string();
        assert_eq!(count(&e.notes().await, "content", &doc), 1, "content written in the creating order");
        let r = e.http.put(format!("{}/v1/objects/{doc}/content", e.base)).bearer_auth(&e.token).header("Content-Type", "Text/Plain; charset=UTF-8").body("b").send().await.unwrap();
        assert_eq!(r.status().as_u16(), 200, "{}", r.text().await.unwrap());
        assert_eq!(count(&e.notes().await, "content", &doc), 2, "PUT .../content runs the hook");
        let (_, obj) = e.get(&format!("/v1/objects/{doc}")).await;
        assert_eq!(obj["content"]["mediaType"], "text/plain", "media type stored normalized: {obj}");
        let put_text = |tt: &str, body: &str| e.http.put(format!("{}/v1/objects/{doc}/texts/{tt}", e.base)).bearer_auth(&e.token).body(body.to_string()).send();
        assert_eq!(put_text("example:summary", "sum").await.unwrap().status().as_u16(), 200);
        assert_eq!(put_text("example:remark", "rem").await.unwrap().status().as_u16(), 200);
        assert_eq!(count(&e.notes().await, "summary", &doc), 1, "only the hooked text type");
        let (code, v) = e.post("/v1/tools/exec", json!({"cwd": "/", "argv": ["ls", format!("/_id/{doc}")]})).await;
        assert_eq!(code, 200, "{v}");
        let listing = v.to_string();
        let file = ["summary.txt", "summary"].into_iter().find(|f| listing.contains(f)).unwrap_or("summary.txt");
        let (code, v) = e.post("/v1/tools/exec", json!({"cwd": "/", "argv": ["sed", "-i", "s/sum/SUM/", format!("/_id/{doc}/{file}")]})).await;
        assert_eq!(code, 200, "{v} {listing}");
        assert_eq!(count(&e.notes().await, "summary", &doc), 2, "writing through the tools runs the hook: {listing}");
    });
}

#[test]
fn hooks_refuse_filters_with_me_time_or_paths_at_load() {
    for (tag, wher) in [("now", "due < now()"), ("path", "parent.status = 'done'")] {
        let p = example_catalog_variant(&format!("unbindable-{tag}"), |ex| {
            ex["filters"] = json!([{ "name": "example:done", "type": "example:Note", "where": wher }]);
        });
        let e = i3cm_server::load_catalog(&p).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(e.contains("filter.not_bindable"), "{tag}: {e}");
    }
    // the same filters are fine where no hook binds them (views, profiles)
    let p = example_catalog_variant("unbound-now", |ex| {
        ex["filters"] = json!([{ "name": "example:done", "type": "example:Note", "where": "status = 'done'" }, { "name": "example:late", "type": "example:Note", "where": "due < now()" }]);
    });
    assert!(i3cm_server::load_catalog(&p).is_ok());
}
