//! Kernel support for the studio (milestone 0.7): development tenants, modules only for
//! development tenants, checking module sources (typecat + catalog checker) and the sources of
//! installed modules.

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
                    let url = db.database("studio_test").await.unwrap();
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

/// Catalog: kernel types, the bundled dms module (sources from deploy/modules) and `devonly`.
fn catalog() -> &'static PathBuf {
    static C: OnceLock<PathBuf> = OnceLock::new();
    C.get_or_init(|| {
        let root = root();
        let out = std::env::temp_dir().join(format!("i3cm-studio-test-{}.json", std::process::id()));
        let st = Command::new("node")
            .arg(root.join("tools/typecat/typecat.mjs"))
            .arg(root.join("types"))
            .args(["--module"])
            .arg(root.join("deploy/modules/src/dms"))
            .args(["--module"])
            .arg(root.join("tests/modules/devonly"))
            .arg("--out")
            .arg(&out)
            .status()
            .expect("node");
        assert!(st.success(), "typecat failed");
        out
    })
}

struct Env {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Env {
    async fn new(development: bool) -> Env {
        let store = Store::connect(&db_url(), i3cm_server::load_catalog(catalog()).unwrap(), 8).await.unwrap();
        let init = store.init_tenant_with(&format!("s-{}", uuid::Uuid::new_v4()), "admin", development).await.unwrap();
        let (addr, _t) = serve_with(store, None, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        Env { base: format!("http://{addr}"), token: init.token, http: reqwest::Client::new() }
    }
    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.http.post(format!("{}{path}", self.base)).bearer_auth(&self.token).json(&body).send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }
    async fn get(&self, path: &str) -> (u16, Value) {
        let r = self.http.get(format!("{}{path}", self.base)).bearer_auth(&self.token).send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }
}

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
}

const MODULE_TS: &str = r#"export const module = { name: "probe", requires: ["base", "dms"] };
export const rooms: string[] = [];
export const profiles = [];
"#;

#[test]
fn development_only_modules_need_a_development_tenant() {
    rt().block_on(async {
        let e = Env::new(false).await;
        let (code, v) = e.get("/v1/me").await;
        assert_eq!((code, v["_tenant"]["development"].as_bool()), (200, Some(false)), "{v}");
        assert!(v["_tenant"]["name"].as_str().is_some_and(|n| n.starts_with("s-")), "the tenant's name: {v}");
        let (code, v) = e.post("/v1/modules/devonly/activate", json!({})).await;
        assert_eq!((code, v["error_id"].as_str()), (400, Some("module.development_only")), "{v}");

        let d = Env::new(true).await;
        let (_, v) = d.get("/v1/me").await;
        assert_eq!(v["_tenant"]["development"].as_bool(), Some(true));
        let (code, v) = d.post("/v1/modules/devonly/activate", json!({})).await;
        assert_eq!(code, 200, "{v}");
        let (_, v) = d.get("/v1/modules").await;
        let m = v.as_array().unwrap().iter().find(|m| m["name"] == "devonly").unwrap().clone();
        assert_eq!((m["developmentOnly"].as_bool(), m["active"].as_bool()), (Some(true), Some(true)));
    })
}

#[test]
fn check_reports_all_issues_with_positions_in_development_tenants_only() {
    rt().block_on(async {
        let files = json!([
            {"path": "i3cm/module.ts", "text": MODULE_TS},
            {"path": "i3cm/types/a.ts", "text": "import { ns, Ref } from \"@i3cm/lib\";\nimport { Document } from \"@i3cm/dms\";\n@ns(\"probe\") export class A {\n  _bad!: string;\n  doc!: Ref<Document, \"probes\">;\n  missing!: Nope;\n}\n"}
        ]);
        let e = Env::new(false).await;
        let (code, v) = e.post("/v1/catalog/check", json!({"modules": [{"name": "probe", "files": files}]})).await;
        assert_eq!((code, v["error_id"].as_str()), (400, Some("check.not_development")), "{v}");

        let d = Env::new(true).await;
        let (code, v) = d.post("/v1/catalog/check", json!({"modules": [{"name": "probe", "files": files}]})).await;
        assert_eq!(code, 200, "{v}");
        assert!(v["catalog"].is_null());
        let issues = v["issues"].as_array().unwrap();
        let ts = issues.iter().find(|i| i["source"] == "typescript").expect("typescript issue");
        assert_eq!((ts["file"].as_str(), ts["line"].as_i64()), (Some("probe:i3cm/types/a.ts"), Some(6)), "{v}");
        let tc = issues.iter().find(|i| i["error_id"] == "typecat.reserved_name").expect("typecat issue");
        assert_eq!(tc["line"].as_i64(), Some(4));

        // fixed: builds against the installed dms (its sources from the package), no issues
        let good = json!([
            {"path": "i3cm/module.ts", "text": MODULE_TS},
            {"path": "i3cm/types/a.ts", "text": "import { ns, Ref } from \"@i3cm/lib\";\nimport { Document } from \"@i3cm/dms\";\n@ns(\"probe\") export class A {\n  title!: string;\n  doc!: Ref<Document, \"probes\">;\n}\n"}
        ]);
        let (code, v) = d.post("/v1/catalog/check", json!({"modules": [{"name": "probe", "files": good}]})).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["issues"].as_array().map(|a| a.len()), Some(0), "{v}");
        assert!(v["catalog"]["types"].as_array().unwrap().iter().any(|t| t["name"] == "probe:A"));
        assert_eq!(v["positions"]["type:probe:A"]["file"].as_str(), Some("probe:i3cm/types/a.ts"));
        assert_eq!(v["positions"]["type:dms:Document"]["file"].as_str(), Some("dms:i3cm/types/documents.ts"));
        assert_eq!(v["positions"]["type:base:Person"]["file"].as_str(), Some("kernel:base/parties.ts"));

        let (code, v) = d.post("/v1/catalog/check", json!({"modules": [{"name": "probe", "files": [{"path": "../x.ts", "text": ""}]}]})).await;
        assert_eq!((code, v["error_id"].as_str()), (400, Some("check.bad_path")));
    })
}

#[test]
fn sources_of_installed_modules() {
    rt().block_on(async {
        let e = Env::new(false).await;
        let (code, v) = e.get("/v1/modules/dms/sources").await;
        assert_eq!(code, 200, "{v}");
        let files = v["files"].as_array().unwrap();
        assert!(files.iter().any(|f| f["path"] == "i3cm/module.ts"));
        let (code, v) = e.get("/v1/modules/devonly/sources").await;
        assert_eq!((code, v["error_id"].as_str()), (404, Some("module.not_installed")), "inactive outside development: {v}");
        let d = Env::new(true).await;
        let (code, v) = d.get("/v1/modules/devonly/sources").await;
        assert_eq!((code, v["error_id"].as_str()), (404, Some("module.no_sources")), "{v}");
    })
}
