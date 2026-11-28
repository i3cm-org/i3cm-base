//! Integration tests: real PostgreSQL (embedded), real HTTP server, Rust client.
//! The acceptance criteria of milestone 0.2 are covered here.

use i3cm_client::{Client, ListQuery, Order};
use i3cm_core::catalog::Catalog;
use i3cm_core::Model;
use i3cm_server::{serve, EmbeddedDb};
use i3cm_store::tenant::TenantInit;
use i3cm_store::Store;
use serde_json::{json, Value};
use std::sync::OnceLock;
use uuid::Uuid;

static DB_URL: OnceLock<String> = OnceLock::new();
static PG_STOP: std::sync::Mutex<Option<(std::path::PathBuf, std::path::PathBuf)>> = std::sync::Mutex::new(None);

unsafe extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

/// Stop the embedded PostgreSQL when the test binary exits.
extern "C" fn stop_pg() {
    if let Some((pg_ctl, data)) = PG_STOP.lock().ok().and_then(|g| g.clone()) {
        let _ = std::process::Command::new(&pg_ctl).args(["stop", "-m", "immediate", "-D"]).arg(&data).output();
        let _ = std::fs::remove_dir_all(&data);
    }
}

/// One embedded PostgreSQL for the whole test binary, started on a dedicated runtime.
fn db_url() -> String {
    DB_URL
        .get_or_init(|| {
            std::thread::spawn(|| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    let db = EmbeddedDb::start(None).await.expect("embedded postgres");
                    let url = db.database("base_test").await.unwrap();
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

/// The shipped catalog plus a test module with a sub room and minus/block profiles.
fn test_model() -> Model {
    let json = std::fs::read_to_string(i3cm_server::test_catalog()).expect("run typecat first: catalog.json missing");
    let mut c: Catalog = serde_json::from_str(&json).unwrap();
    // rights paths: team leaders read and write the leave requests of their team members
    let ty = |name: &str, attrs: serde_json::Value, requires: &[&str]| {
        let (ns, local) = name.split_once(':').unwrap();
        serde_json::from_value(json!({"name": name, "ns": ns, "local": local, "abstract": false, "extends": null, "attributes": attrs,
            "requires": {"mode": "any", "types": requires}, "imprintsOn": [], "brings": [], "content": null, "textTypes": []})).unwrap()
    };
    let at = |n: &str, kind: &str| json!({"name": n, "vt": {"kind": kind}, "required": false, "list": false, "indexed": false, "unique": false});
    c.types.push(ty("t:Team", json!([at("name", "string")]), &[]));
    c.types.push(ty("t:Staff", json!([]), &["base:Person"]));
    c.types.push(ty("t:LeaveRequest", json!([at("days", "int")]), &[]));
    let rf = |name: &str, h: &str, he: &str, t: &str, te: &str| {
        serde_json::from_value(json!({"name": name, "holder": {"type": h, "end": he, "min": 0, "max": 1}, "target": {"type": t, "end": te, "min": 0, "max": null},
            "partOf": false, "ordered": false, "rightsRelevant": true})).unwrap()
    };
    c.references.push(rf("t:Staff.team", "t:Staff", "team", "t:Team", "members"));
    c.references.push(rf("t:Team.leader", "t:Team", "leader", "base:Person", "leads"));
    c.references.push(rf("t:LeaveRequest.applicant", "t:LeaveRequest", "applicant", "base:Person", "leaveRequests"));
    // not used by any rights path: an ordinary reference
    c.references.push(rf("t:Team.mascot", "t:Team", "mascot", "base:Person", "mascotOf"));
    c.modules.push(
        serde_json::from_value(json!({
            "name": "t", "requires": ["base"], "rooms": ["tsub"], "i18n": {},
            "aspects": [{"name": "t:mail-drop", "entry": {"type": "sample:Mail", "groupBy": []},
                "nodes": {"sample:Mail": {"attributes": [], "subNodes": [{"label": "attachments", "path": "attachments", "folder": true, "dropType": "sample:MailAttachment"}],
                    "files": [{"name": "mail.md", "path": "_texts.`mail.md`"}, {"name": "mail.eml", "path": "_content"}], "tabs": [], "functions": [], "filesFirst": false}}},
                {"name": "t:due-mine", "entry": {"type": "sample:Task", "groupBy": [], "where": "due <= now() and status != 'done' and assignees = %me"},
                "nodes": {"sample:Task": {"attributes": [{"label": "subject", "path": "subject"}], "subNodes": [], "files": [], "tabs": [], "functions": [], "filesFirst": true}}}],
            "profiles": [
                {"name": "t:no-person-sub", "rights": [{"sign": "-", "art": ["read"], "types": ["base:Person"], "room": "tsub"}]},
                {"name": "t:block-person", "rights": [{"sign": "block", "art": ["read"], "types": ["base:Person"], "room": "root"}]},
                {"name": "t:lead-leave", "rights": [{"sign": "+", "art": ["write"], "types": ["t:LeaveRequest"], "room": "root", "when": "applicant.team.leader = %me"}]},
                {"name": "t:read-persons", "rights": [{"sign": "+", "art": ["read"], "types": ["base:Person"], "room": "root"}]},
                {"name": "t:team-writer", "rights": [{"sign": "+", "art": ["create", "write"], "types": ["t:Team"], "room": "root"}]},
                {"name": "t:team-granter", "rights": [{"sign": "+", "art": ["write", "grant"], "types": ["t:Team"], "room": "root"}]},
                {"name": "t:member-writer", "rights": [{"sign": "+", "art": ["create", "write"], "types": ["base:GroupMembership"], "room": "root"},
                    {"sign": "+", "art": ["read"], "types": ["base:Group", "base:Person"], "room": "root"}]},
                {"name": "t:task-worker", "rights": [{"sign": "+", "art": ["read", "write"], "types": ["sample:Task"], "room": "root", "when": "assignees = %me"},
                    {"sign": "+", "art": ["read"], "types": ["base:Person"], "room": "root"}]},
                {"name": "t:person-writer", "rights": [{"sign": "+", "art": ["write"], "types": ["base:Person"], "room": "root"},
                    {"sign": "+", "art": ["read"], "types": ["t:Team"], "room": "root"}]}
            ]
        }))
        .unwrap(),
    );
    // all modules active from the start, as before activation existed (0.6)
    for m in &mut c.modules {
        m.mandatory = true;
    }
    Model::new(c).unwrap()
}

struct Env {
    store: Store,
    admin: Client,
    init: TenantInit,
    rooms: std::collections::BTreeMap<String, Uuid>,
}

async fn env() -> Env {
    env_with(Store::connect(&db_url(), test_model(), 16).await.unwrap().with_files(&data_dir()).unwrap()).await
}

/// A fresh data directory for one test.
fn data_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("i3cm-test-data-{}", Uuid::new_v4()))
}

async fn env_with(store: Store) -> Env {
    let init = store.init_tenant(&format!("t-{}", Uuid::new_v4()), "admin").await.unwrap();
    let (addr, _task) = serve(store.clone(), "127.0.0.1:0".parse().unwrap()).await.unwrap();
    let admin = Client::new(&format!("http://{addr}"), &init.token);
    let rooms = admin.rooms().await.unwrap();
    Env { store, admin, init, rooms }
}

impl Env {
    /// Person with login, optionally member of groups; returns (id, client for that person).
    async fn person(&self, login: &str, groups: &[Uuid]) -> (Uuid, Client) {
        let mut o = Order::new();
        o.create("p", None, json!({"base:Person": {"firstName": "Lea", "lastName": login}, "base:PersonAccount": {"login": login}}));
        for (i, g) in groups.iter().enumerate() {
            let m = format!("m{i}");
            o.create(&m, None, json!({"base:GroupMembership": {}}));
            o.link("base:GroupMembership.group", m.as_str(), *g).link("base:GroupMembership.member", m.as_str(), "p");
        }
        let id = self.admin.order(&o).await.unwrap().created["p"];
        let token = self.admin.create_token(id).await.unwrap();
        (id, self.admin.with_token(&token))
    }

    async fn group(&self, name: &str, parent: Option<Uuid>) -> Uuid {
        let mut o = Order::new();
        o.create("g", None, json!({"base:Group": {"name": name}}));
        if let Some(p) = parent {
            o.create("m", None, json!({"base:GroupMembership": {}}));
            o.link("base:GroupMembership.group", "m", p).link("base:GroupMembership.member", "m", "g");
        }
        self.admin.order(&o).await.unwrap().created["g"]
    }

    async fn add_member(&self, group: Uuid, member: Uuid) {
        let mut o = Order::new();
        o.create("m", None, json!({"base:GroupMembership": {}}));
        o.link("base:GroupMembership.group", "m", group).link("base:GroupMembership.member", "m", member);
        self.admin.order(&o).await.unwrap();
    }

    fn journal(&self, o: &mut Order, n: usize, date: &str, accounts: (Uuid, Uuid)) {
        let room = Some(self.rooms["fibu"]);
        let e = format!("e{n}");
        o.create(&e, room, json!({"fibu:JournalEntry": {"documentDate": date, "postingDate": date, "documentNumber": format!("D-{n}"), "fiscalYear": 2026}}));
        for (side, acc) in [("debit", accounts.0), ("credit", accounts.1)] {
            let l = format!("{e}-{side}");
            o.create(&l, room, json!({"fibu:JournalLine": {"side": side, "amount": {"amount": "10.00", "currency": "EUR"}}}));
            o.link("fibu:JournalLine.entry", l.as_str(), e.as_str()).link("fibu:JournalLine.account", l.as_str(), acc);
        }
    }

    async fn accounts(&self) -> (Uuid, Uuid) {
        let room = Some(self.rooms["fibu"]);
        let mut o = Order::new();
        o.create("a", room, json!({"fibu:LedgerAccount": {"number": "1200", "name": "Bank", "kind": "asset"}}));
        o.create("b", room, json!({"fibu:LedgerAccount": {"number": "8400", "name": "Revenue", "kind": "revenue"}}));
        let r = self.admin.order(&o).await.unwrap();
        (r.created["a"], r.created["b"])
    }
}

#[test]
fn catalog_checker_rejects_cycles() {
    let json = std::fs::read_to_string(i3cm_server::test_catalog()).unwrap();
    assert!(Model::from_json(&json).is_ok());
    let mut c: serde_json::Value = serde_json::from_str(&json).unwrap();
    for t in c["types"].as_array_mut().unwrap() {
        if t["name"] == "base:Person" {
            t["brings"] = json!(["base:Company"]);
        }
        if t["name"] == "base:Company" {
            t["brings"] = json!(["base:Person"]);
        }
    }
    let e = Model::from_json(&c.to_string()).unwrap_err();
    assert_eq!(e.error_id, "type.implication_cycle");
}

#[tokio::test]
async fn person_with_account_is_me() {
    let env = env().await;
    let (id, lea) = env.person("lea", &[]).await;
    let me = lea.me().await.unwrap();
    assert_eq!(me.id, id);
    assert!(me.has_type("base:PersonAccount"));
    assert_eq!(me.display_name("base:PersonAccount"), Some("Login lea"));
    assert!(me.display_name("base:Person").is_none(), "person data needs a right");
    // without a profile Lea sees nothing else
    assert_eq!(lea.get_object(env.init.admin_account, None).await.unwrap_err().status(), Some(404));
}

#[tokio::test]
async fn groups_in_groups_resolve() {
    let env = env().await;
    let a = env.group("A", None).await;
    let b = env.group("B", Some(a)).await;
    let (lea_id, lea) = env.person("lea", &[b]).await;
    let mine: Vec<Uuid> = lea.my_groups().await.unwrap().iter().map(|s| s.id).collect();
    assert!(mine.contains(&a) && mine.contains(&b), "{mine:?}");
    let direct: Vec<Uuid> = env.admin.members(a, false).await.unwrap().iter().map(|s| s.id).collect();
    assert_eq!(direct, vec![b]);
    let all: Vec<Uuid> = env.admin.members(a, true).await.unwrap().iter().map(|s| s.id).collect();
    assert!(all.contains(&lea_id) && all.contains(&b));
}

#[tokio::test]
async fn rights_inherit_minus_per_carrier_and_block() {
    let env = env().await;
    let a = env.group("A", None).await;
    let b = env.group("B", Some(a)).await;
    let c = env.group("C", None).await;
    let (lea_id, lea) = env.person("lea", &[b]).await;
    let mut o = Order::new();
    o.create("p", Some(env.rooms["tsub"]), json!({"base:Person": {"lastName": "InSub"}}));
    o.create("q", None, json!({"base:Person": {"lastName": "InRoot"}}));
    let r = env.admin.order(&o).await.unwrap();
    let (p, q) = (r.created["p"], r.created["q"]);

    assert_eq!(lea.get_object(p, None).await.unwrap_err().status(), Some(404));
    env.admin.assign("base:reader", a).await.unwrap();
    assert!(lea.get_object(p, None).await.is_ok(), "inherited from root via group A (through B)");

    env.admin.assign("t:no-person-sub", a).await.unwrap();
    assert_eq!(lea.get_object(p, None).await.unwrap_err().status(), Some(404), "minus in the sub room");
    assert!(lea.get_object(q, None).await.is_ok(), "root unaffected");

    env.add_member(c, lea_id).await;
    env.admin.assign("base:reader", c).await.unwrap();
    assert!(lea.get_object(p, None).await.is_ok(), "minus of A does not cancel the plus of C");

    env.admin.assign("t:block-person", b).await.unwrap();
    assert_eq!(lea.get_object(p, None).await.unwrap_err().status(), Some(404), "block wins");
    assert_eq!(lea.get_object(q, None).await.unwrap_err().status(), Some(404), "block wins");
    // writing needs write rights
    let mut o = Order::new();
    o.update(q, json!({"base:Person": {"lastName": "x"}}));
    assert_eq!(lea.order(&o).await.unwrap_err().status(), Some(404));
}

#[tokio::test]
async fn versions_only_for_changed_parts() {
    let env = env().await;
    let (id, _) = env.person("vera", &[]).await;
    let before = env.admin.versions(id).await.unwrap();
    let v1 = before.last().unwrap().version;
    let mut o = Order::new();
    o.update(id, json!({"base:Person": {"lastName": "vera-neu"}}));
    env.admin.order(&o).await.unwrap();
    let after = env.admin.versions(id).await.unwrap();
    assert_eq!(after.len(), before.len() + 1);
    let old = env.admin.get_object(id, Some(v1)).await.unwrap();
    assert_eq!(old.attr("lastName"), Some(&json!("vera")));
    let cur = env.admin.get_object(id, None).await.unwrap();
    assert_eq!(cur.attr("lastName"), Some(&json!("vera-neu")));
    let c = env.store.client().await.unwrap();
    let n = |t: &'static str| {
        let c = &c;
        async move { c.query_one(&format!("SELECT count(*) FROM {t} WHERE object_id=$1"), &[&id]).await.unwrap().get::<_, i64>(0) }
    };
    assert_eq!(n("t_base_person").await, 2, "person part versioned");
    assert_eq!(n("t_base_account").await, 1, "account part untouched");
}

#[tokio::test]
async fn integrity_rejects_and_dry_run_reports() {
    let env = env().await;
    let g = env.group("G", None).await;
    let (lea_id, _) = env.person("lea", &[g]).await;
    // detaching the account while the membership needs a carrier
    let mut o = Order::new().dry_run();
    o.detach(lea_id, "base:PersonAccount");
    let e = env.admin.order(&o).await.unwrap_err();
    assert_eq!(e.error_id(), Some("integrity.violated"));
    assert!(e.to_string().contains("reference.type_lost"), "{e}");
    // journal entry without lines
    let mut o = Order::new();
    o.create("e", Some(env.rooms["fibu"]), json!({"fibu:JournalEntry": {"documentDate": "2026-01-01", "postingDate": "2026-01-01", "documentNumber": "X", "fiscalYear": 2026}}));
    let e = env.admin.order(&o).await.unwrap_err();
    assert!(e.to_string().contains("cardinality.violated"), "{e}");
    // a successful dry run leaves nothing behind
    let mut o = Order::new().dry_run();
    o.create("p", None, json!({"base:Person": {"lastName": "ghost"}}));
    let r = env.admin.order(&o).await.unwrap();
    assert!(r.dry_run);
    assert_eq!(env.admin.get_object(r.created["p"], None).await.unwrap_err().status(), Some(404));
    // values are type checked
    let mut o = Order::new();
    o.create("p", None, json!({"base:Person": {"lastName": "x", "birthDate": "31.12.2000"}}));
    assert_eq!(env.admin.order(&o).await.unwrap_err().error_id(), Some("value.invalid"));
}

#[tokio::test]
async fn parts_follow_the_whole() {
    let env = env().await;
    let acc = env.accounts().await;
    let mut o = Order::new();
    env.journal(&mut o, 1, "2026-02-03", acc);
    let r = env.admin.order(&o).await.unwrap();
    let (e, line) = (r.created["e1"], r.created["e1-debit"]);
    let lines = env.admin.ends(e, "lines", 10, 0).await.unwrap();
    assert_eq!(lines.len(), 2);
    let acc_of_line = env.admin.ends(line, "account", 10, 0).await.unwrap();
    assert_eq!(acc_of_line[0].id, acc.0);
    let mut o = Order::new();
    o.move_to(e, env.rooms["root"]);
    env.admin.order(&o).await.unwrap();
    assert_eq!(env.admin.get_object(line, None).await.unwrap().room, env.rooms["root"]);
    let mut o = Order::new();
    o.delete(e);
    env.admin.order(&o).await.unwrap();
    assert_eq!(env.admin.get_object(line, None).await.unwrap_err().status(), Some(404));
    assert!(env.admin.ends(acc.0, "lines", 10, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn mail_with_attachments_content_and_texts() {
    let env = env().await;
    let mut o = Order::new();
    o.create("m", None, json!({"sample:Mail": {"receivedAt": "2026-11-24T08:15:00+01:00", "sender": "anna@example.org", "subject": "Angebot"}}));
    for i in 0..2 {
        let a = format!("a{i}");
        o.create(&a, None, json!({"sample:MailAttachment": {"filename": format!("file{i}.pdf")}}));
        o.link("sample:MailAttachment.mail", a.as_str(), "m");
    }
    let r = env.admin.order(&o).await.unwrap();
    let m = r.created["m"];
    env.admin.put_content(m, b"From: anna@example.org\r\n\r\nHallo".to_vec(), "message/rfc822", Some("mail.eml")).await.unwrap();
    assert_eq!(env.admin.ends(m, "attachments", 10, 0).await.unwrap().len(), 2);
    assert!(env.admin.get_content(m).await.unwrap().ends_with(b"Hallo"));
    env.admin.put_text(m, "sample:mail.md", "Hallo").await.unwrap();
    assert_eq!(env.admin.get_text(m, "sample:mail.md").await.unwrap(), "Hallo");
    let view = env.admin.get_object(m, None).await.unwrap();
    assert_eq!(view.texts, vec!["sample:mail.md".to_string()]);
    assert_eq!(view.display_name("sample:Mail"), Some("anna@example.org - Angebot"));
    let en = env.admin.clone().with_lang("en").get_object(m, None).await.unwrap();
    let de = env.admin.clone().with_lang("de-DE").get_object(m, None).await.unwrap();
    assert!(en.types[0].display_description.starts_with("Mail received"));
    assert!(de.types[0].display_description.starts_with("Mail vom"));
    assert!(env.admin.put_text(m, "sample:body.html", "<p>x</p>").await.unwrap() > 0);
    assert_eq!(env.admin.put_text(m, "base:nope", "x").await.unwrap_err().error_id(), Some("text.unknown_type"));
}

#[tokio::test]
async fn date_navigation() {
    let env = env().await;
    let acc = env.accounts().await;
    let mut o = Order::new();
    for (i, d) in ["2025-12-30", "2026-01-05", "2026-01-20", "2026-02-01"].iter().enumerate() {
        env.journal(&mut o, i, d, acc);
    }
    env.admin.order(&o).await.unwrap();
    let years = env.admin.group_counts("fibu:JournalEntry", "postingDate", "year", None, None).await.unwrap();
    assert_eq!(years.iter().map(|g| (g.key.as_str(), g.count)).collect::<Vec<_>>(), vec![("2025", 1), ("2026", 3)]);
    let months = env.admin.group_counts("fibu:JournalEntry", "postingDate", "month", Some("2026-01-01"), Some("2026-12-31")).await.unwrap();
    assert_eq!(months.iter().map(|g| (g.key.as_str(), g.count)).collect::<Vec<_>>(), vec![("2026-01", 2), ("2026-02", 1)]);
    let q = ListQuery { attr: Some("postingDate".into()), from: Some("2026-01-01".into()), to: Some("2026-01-31".into()), ..Default::default() };
    let jan = env.admin.list("fibu:JournalEntry", &q).await.unwrap();
    assert_eq!(jan.iter().map(|s| s.value.clone().unwrap()).collect::<Vec<_>>(), vec![json!("2026-01-05"), json!("2026-01-20")]);
    assert!(jan[0].display_name.contains("D-1"));
}

#[tokio::test]
async fn accountant_role_and_created_by() {
    let env = env().await;
    let acc = env.accounts().await;
    let g = env.group("accountants", None).await;
    env.admin.assign("fibu:accountant", g).await.unwrap();
    let (kai_id, kai) = env.person("kai", &[g]).await;
    let mut o = Order::new();
    env.journal(&mut o, 7, "2026-03-01", acc);
    let r = kai.order(&o).await.unwrap();
    let e = kai.get_object(r.created["e7"], None).await.unwrap();
    assert_eq!(e.created_by, Some(kai_id));
    // accountants cannot create ledger accounts
    let mut o = Order::new();
    o.create("a", Some(env.rooms["fibu"]), json!({"fibu:LedgerAccount": {"number": "9", "name": "x", "kind": "asset"}}));
    assert_eq!(kai.order(&o).await.unwrap_err().status(), Some(404));
    // nor journal entries outside the module room
    let mut o = Order::new();
    o.create("e", None, json!({"fibu:JournalEntry": {"documentDate": "2026-01-01", "postingDate": "2026-01-01", "documentNumber": "X", "fiscalYear": 2026}}));
    assert_eq!(kai.order(&o).await.unwrap_err().status(), Some(404));
}

#[tokio::test]
async fn tenants_are_isolated() {
    let env1 = env().await;
    let env2 = env().await;
    let mut o = Order::new();
    o.create("p", None, json!({"base:Person": {"lastName": "Secret"}}));
    let p = env1.admin.order(&o).await.unwrap().created["p"];
    assert_eq!(env2.admin.get_object(p, None).await.unwrap_err().status(), Some(404));
    let q = ListQuery { attr: Some("lastName".into()), eq: Some("Secret".into()), ..Default::default() };
    assert!(env2.admin.list("base:Person", &q).await.unwrap().is_empty());
    assert_eq!(env1.admin.list("base:Person", &q).await.unwrap().len(), 1);
    // unique values are per tenant
    let mut o = Order::new();
    o.create("g", None, json!({"base:Group": {"name": "admins"}}));
    assert_eq!(env1.admin.order(&o).await.unwrap_err().error_id(), Some("value.not_unique"));
}

#[tokio::test]
async fn unauthenticated_is_rejected() {
    let env = env().await;
    let bad = env.admin.with_token("bt_invalid");
    assert_eq!(bad.me().await.unwrap_err().status(), Some(401));
}

#[tokio::test]
async fn mailbox_aspect() {
    let env = env().await;
    let mut o = Order::new();
    for (i, (at, sender, subject)) in [("2026-11-24T08:15:00+01:00", "anna@example.org", "Angebot"), ("2026-11-24T17:00:00+01:00", "ben@example.org", "Rechnung"), ("2026-11-25T09:00:00+01:00", "carla@example.org", "Termin")]
        .iter()
        .enumerate()
    {
        let m = format!("m{i}");
        o.create(&m, None, json!({"sample:Mail": {"receivedAt": at, "sender": sender, "subject": subject}}));
        if i == 0 {
            for j in 0..2 {
                let a = format!("a{j}");
                o.create(&a, None, json!({"sample:MailAttachment": {"filename": format!("file{j}.pdf")}}));
                o.link("sample:MailAttachment.mail", a.as_str(), m.as_str());
            }
        }
    }
    let r = env.admin.order(&o).await.unwrap();
    let m0 = r.created["m0"];
    env.admin.put_content(m0, b"From: anna\r\n\r\nHallo".to_vec(), "message/rfc822", None).await.unwrap();
    env.admin.put_content(r.created["a0"], b"%PDF-1".to_vec(), "application/pdf", Some("file0.pdf")).await.unwrap();
    env.admin.put_text(m0, "sample:mail.md", "# Hallo").await.unwrap();

    let de = env.admin.clone().with_lang("de");
    let days = de.aspect_children("sample:mailbox", "").await.unwrap();
    assert_eq!(days.iter().map(|d| (d.display_name.as_str(), d.count)).collect::<Vec<_>>(), vec![("2026-11-24", Some(2)), ("2026-11-25", Some(1))]);
    let mut mails = de.aspect_children("sample:mailbox", &days[0].node).await.unwrap();
    mails.sort_by(|a, b| a.display_name.cmp(&b.display_name)); // compared as a set
    assert_eq!(mails.iter().map(|m| m.display_name.as_str()).collect::<Vec<_>>(), vec!["anna@example.org - Angebot", "ben@example.org - Rechnung"]);
    let inside = de.aspect_children("sample:mailbox", &mails[0].node).await.unwrap();
    // order of the definition: the sub-node block comes before the files block in sample:mailbox
    let names: Vec<(bool, &str)> = inside.iter().map(|n| (n.file, n.display_name.as_str())).collect();
    assert_eq!(names, vec![(false, "Anhänge"), (true, "mail.eml"), (true, "mail.md")]);
    let atts = de.aspect_children("sample:mailbox", &inside[0].node).await.unwrap();
    assert_eq!(atts.len(), 2);
    let att_files = de.aspect_children("sample:mailbox", &atts.iter().find(|a| a.display_name == "file0.pdf").unwrap().node).await.unwrap();
    assert_eq!(att_files[0].display_name, "file0.pdf");
    assert_eq!(de.aspect_file("sample:mailbox", &mails[0].node, "mail.md").await.unwrap(), b"# Hallo");
    assert!(de.aspect_file("sample:mailbox", &mails[0].node, "mail.eml").await.unwrap().ends_with(b"Hallo"));

    let detail = de.aspect_node("sample:mailbox", &mails[0].node).await.unwrap();
    assert_eq!(detail["node"]["displayDescription"].as_str().unwrap(), "Eingegangen am 2026-11-24T07:15:00+00:00", "aspect text overrides the global one");
    let labels: Vec<&str> = detail["attributes"].as_array().unwrap().iter().map(|a| a["label"].as_str().unwrap()).collect();
    assert_eq!(labels, vec!["Absender", "Betreff", "Empfangen", "Anzahl Anhänge"]);
    assert_eq!(detail["attributes"][3]["values"], json!([2]));
    assert_eq!(detail["tabs"][0]["label"], json!("Anhänge"));
    assert_eq!(detail["tabs"][0]["columns"], json!(["Datei", "Hinzugefügt"]));
    assert_eq!(detail["tabs"][0]["rows"].as_array().unwrap().len(), 2);
    assert_eq!(detail["functions"][0]["label"], json!("Antworten"));

    assert_eq!(env.admin.eval(m0, "attachments.filename").await.unwrap(), vec![json!("file0.pdf"), json!("file1.pdf")]);
    assert_eq!(env.admin.eval(m0, "attachments.where(filename = 'file1.pdf').count()").await.unwrap(), vec![json!(1)]);
    // the stored upload file name (0.6): `_content.filename`
    assert_eq!(env.admin.eval(r.created["a0"], "_content.filename").await.unwrap(), vec![json!("file0.pdf")]);
    assert_eq!(env.admin.eval(m0, "attachments._content.mediaType").await.unwrap(), vec![json!("application/pdf")], "only a0 has content");
    assert_eq!(env.admin.eval(m0, "_createdBy = %x").await.unwrap_err().error_id(), Some("path.syntax"));
    assert_eq!(env.admin.eval(m0, "_createdBy._id").await.unwrap(), vec![json!(env.init.admin_account.to_string())]);
    assert_eq!(env.admin.eval(m0, "_changedBy._id").await.unwrap(), vec![json!(env.init.admin_account.to_string())]);
    let created = env.admin.eval(m0, "_createdAt").await.unwrap();
    let changed = env.admin.eval(m0, "_changedAt").await.unwrap();
    assert_eq!(changed.len(), 1);
    assert!(changed[0].as_str().unwrap() >= created[0].as_str().unwrap(), "{changed:?} >= {created:?}");

    // without read rights the mailbox is empty
    let (_, lea) = env.person("lea", &[]).await;
    assert!(lea.aspect_children("sample:mailbox", "").await.unwrap().is_empty());
}

#[tokio::test]
async fn builtin_views() {
    let env = env().await;
    let mut o = Order::new();
    o.create("m", None, json!({"sample:Mail": {"receivedAt": "2026-11-24T08:15:00+01:00", "sender": "anna@example.org", "subject": "Angebot"}}));
    for j in 0..2 {
        let a = format!("a{j}");
        o.create(&a, None, json!({"sample:MailAttachment": {"filename": format!("file{j}.pdf")}}));
        o.link("sample:MailAttachment.mail", a.as_str(), "m");
    }
    let r = env.admin.order(&o).await.unwrap();
    let m = r.created["m"];
    env.admin.put_content(m, b"From: anna\r\n\r\nHallo".to_vec(), "message/rfc822", None).await.unwrap();
    env.admin.put_text(m, "sample:mail.md", "# Hallo").await.unwrap();

    let de = env.admin.clone().with_lang("de");
    let list = de.aspects().await.unwrap();
    assert_eq!(list[0]["name"], json!("_id"));
    assert_eq!(list[1]["displayName"], json!("Nach Typ"));

    // _type: types with counts, then objects, then files and filled ends
    let types = de.aspect_children("_type", "").await.unwrap();
    let mail = types.iter().find(|t| t.display_name == "sample:Mail").expect("type folder");
    assert_eq!(mail.count, Some(1));
    assert_eq!(types.iter().find(|t| t.display_name == "sample:MailAttachment").unwrap().count, Some(2));
    let mails = de.aspect_children("_type", &mail.node).await.unwrap();
    assert_eq!(mails.len(), 1);
    let inside = de.aspect_children("_type", &mails[0].node).await.unwrap();
    let names: Vec<(bool, &str)> = inside.iter().map(|n| (n.file, n.display_name.as_str())).collect();
    assert_eq!(names, vec![(true, "content"), (true, "mail.md"), (false, "attachments")]);
    assert_eq!(de.aspect_children("_type", &inside[2].node).await.unwrap().len(), 2);
    let page = de.aspect_children_page("_type", &inside[2].node, 1, 1).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].display_name, "file1.pdf");
    assert_eq!(de.aspect_file("_type", &mails[0].node, "mail.md").await.unwrap(), b"# Hallo");

    // _id: the root lists nothing, every object is reachable by its id
    assert!(de.aspect_children("_id", "").await.unwrap().is_empty());
    let node = format!("o:{m}");
    assert_eq!(de.aspect_children("_id", &node).await.unwrap().len(), 3);
    let detail = de.aspect_node("_id", &node).await.unwrap();
    assert_eq!(detail["attributes"][0]["values"], json!([m.to_string()]));
    assert!(detail["attributes"].as_array().unwrap().iter().any(|a| a["key"] == json!("subject")));

    // without read rights nothing is visible
    let (_, lea) = env.person("lea", &[]).await;
    assert!(lea.aspect_children("_type", "").await.unwrap().iter().all(|t| t.display_name != "sample:Mail"));
    assert!(lea.aspect_node("_id", &node).await.is_err());
}

#[tokio::test]
async fn multi_valued_group_by() {
    let env = env().await;
    let mut o = Order::new();
    o.create("m1", None, json!({"sample:Mail": {"receivedAt": "2026-11-24T08:00:00Z", "sender": "anna@example.org", "subject": "Angebot", "recipients": ["ben@example.org", "carla@example.org", "ben@example.org"]}}));
    o.create("m2", None, json!({"sample:Mail": {"receivedAt": "2026-11-24T09:00:00Z", "sender": "ben@example.org", "subject": "Frage", "recipients": ["carla@example.org"]}}));
    o.create("m3", None, json!({"sample:Mail": {"receivedAt": "2026-11-24T10:00:00Z", "sender": "carla@example.org", "subject": "Ohne"}}));
    env.admin.order(&o).await.unwrap();

    let de = env.admin.clone().with_lang("de");
    let groups = de.aspect_children("sample:recipients", "").await.unwrap();
    // the mail exists once but appears under each of its (distinct) recipients; counts are occurrences
    // the mail without recipients forms one group with the empty key, labelled "—" (0.6)
    assert_eq!(
        groups.iter().map(|g| (g.display_name.as_str(), g.count)).collect::<Vec<_>>(),
        vec![("—", Some(1)), ("ben@example.org", Some(1)), ("carla@example.org", Some(2))]
    );
    let none = de.aspect_children("sample:recipients", &groups[0].node).await.unwrap();
    assert_eq!(none.iter().map(|m| m.display_name.as_str()).collect::<Vec<_>>(), vec!["carla@example.org - Ohne"]);
    // grouping is paged like every listing
    let second = de.aspect_children_page("sample:recipients", "", 2, 1).await.unwrap();
    assert_eq!(second.iter().map(|g| g.display_name.as_str()).collect::<Vec<_>>(), vec!["carla@example.org"]);
    let groups: Vec<_> = groups.into_iter().skip(1).collect();
    let mut carla = de.aspect_children("sample:recipients", &groups[1].node).await.unwrap();
    carla.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    assert_eq!(carla.iter().map(|m| m.display_name.as_str()).collect::<Vec<_>>(), vec!["anna@example.org - Angebot", "ben@example.org - Frage"]);
    let ben = de.aspect_children("sample:recipients", &groups[0].node).await.unwrap();
    assert_eq!(ben[0].id, carla[0].id, "same object under both groups");
}

#[tokio::test]
async fn tokens_expire_and_are_revoked() {
    let env = env().await;
    let (lea_id, lea) = env.person("lea", &[]).await;
    let t = lea.issue_token(None, Some("agent"), Some(7)).await.unwrap();
    let agent = lea.with_token(&t.token);
    assert_eq!(agent.me().await.unwrap().id, lea_id);
    let list = lea.tokens().await.unwrap();
    assert!(list.iter().any(|x| x.id == t.id && x.label.as_deref() == Some("agent")));
    assert!(list.iter().all(|x| x.account == lea_id), "without grant only own tokens");
    assert!(env.admin.tokens().await.unwrap().iter().any(|x| x.account == lea_id), "with grant all tokens");
    assert_eq!(lea.issue_token(Some(env.init.admin_account), None, None).await.unwrap_err().status(), Some(404));
    assert_eq!(lea.issue_token(None, None, Some(400)).await.unwrap_err().error_id(), Some("token.bad_ttl"));
    // a self-issued token never outlives the token it was issued with (lea's: 90 days)
    let long = lea.issue_token(None, None, Some(365)).await.unwrap();
    let exp = chrono::DateTime::parse_from_rfc3339(long.expires_at.as_deref().unwrap()).unwrap();
    assert!(exp < chrono::Utc::now() + chrono::Duration::days(91));

    env.store.client().await.unwrap().execute("UPDATE token SET expires_at = now() - interval '1 second' WHERE id=$1", &[&t.id]).await.unwrap();
    let e = agent.me().await.unwrap_err();
    assert_eq!((e.status(), e.error_id()), (Some(401), Some("auth.expired_token")));

    let t2 = lea.issue_token(None, None, None).await.unwrap();
    let a2 = lea.with_token(&t2.token);
    a2.me().await.unwrap();
    a2.revoke_token(t2.id).await.unwrap();
    let e = a2.me().await.unwrap_err();
    assert_eq!((e.status(), e.error_id()), (Some(401), Some("auth.revoked_token")));
    assert!(lea.tokens().await.unwrap().iter().any(|x| x.id == t2.id && x.revoked_at.is_some()));
    assert_eq!(lea.revoke_token(env.admin.tokens().await.unwrap().iter().find(|x| x.account == env.init.admin_account).unwrap().id).await.unwrap_err().status(), Some(404));
}

#[tokio::test]
async fn command_line_tokens_expire() {
    let env = env().await;
    let days = |exp: chrono::DateTime<chrono::Utc>| (exp - chrono::Utc::now()).num_hours() as f64 / 24.0;
    // the first token of a tenant is a start key for one day
    let first = env.admin.tokens().await.unwrap().into_iter().find(|x| x.account == env.init.admin_account).unwrap();
    let exp = chrono::DateTime::parse_from_rfc3339(first.expires_at.as_deref().unwrap()).unwrap().with_timezone(&chrono::Utc);
    assert!((0.9..=1.0).contains(&days(exp)), "start key: {exp}");
    let t = env.store.create_token_unchecked(env.init.tenant, env.init.admin_account, None).await.unwrap();
    assert!((89.9..=90.0).contains(&days(t.expires_at.unwrap())));
    let t = env.store.create_token_unchecked(env.init.tenant, env.init.admin_account, Some(365)).await.unwrap();
    assert!((364.9..=365.0).contains(&days(t.expires_at.unwrap())));
    for bad in [0, 366] {
        assert_eq!(env.store.create_token_unchecked(env.init.tenant, env.init.admin_account, Some(bad)).await.unwrap_err().error_id, "token.bad_ttl");
    }
}

/// Mail with two attachments, content and a text (as in the mailbox test).
async fn mail_fixture(env: &Env) -> Uuid {
    let mut o = Order::new();
    o.create("m", None, json!({"sample:Mail": {"receivedAt": "2026-11-24T08:15:00+01:00", "sender": "anna@example.org", "subject": "Angebot"}}));
    o.create("n", None, json!({"sample:Mail": {"receivedAt": "2026-11-25T09:00:00+01:00", "sender": "carla@example.org", "subject": "Termin"}}));
    for j in 0..2 {
        let a = format!("a{j}");
        o.create(&a, None, json!({"sample:MailAttachment": {"filename": format!("file{j}.pdf")}}));
        o.link("sample:MailAttachment.mail", a.as_str(), "m");
    }
    let r = env.admin.order(&o).await.unwrap();
    let m = r.created["m"];
    env.admin.put_content(m, b"From: anna\r\n\r\nHallo".to_vec(), "message/rfc822", None).await.unwrap();
    env.admin.put_content(r.created["a0"], b"%PDF-1".to_vec(), "application/pdf", Some("file0.pdf")).await.unwrap();
    env.admin.put_text(m, "sample:mail.md", "# Hallo").await.unwrap();
    m
}

#[tokio::test]
async fn index_and_file_metadata() {
    let env = env().await;
    let m = mail_fixture(&env).await;
    let de = env.admin.clone().with_lang("de");
    let days = de.aspect_children("sample:mailbox", "").await.unwrap();
    assert_eq!(days[0].name, "2026-11-24");
    assert_eq!(days[0].size, Some("# 2026-11-24\n".len() as u64));
    let mail = de.aspect_children("sample:mailbox", &days[0].node).await.unwrap().remove(0);
    let idx = de.aspect_index("sample:mailbox", &mail.node, None).await.unwrap();
    assert!(idx.starts_with(&format!("---\n_id: \"{m}\"\n_type: [\"sample:Mail\"]\n")), "{idx}");
    assert!(idx.contains("sender: \"anna@example.org\"\n"), "{idx}");
    assert!(idx.contains("---\n# anna@example.org - Angebot\n"), "{idx}");
    assert!(idx.contains("## Anhänge\n\n- [[anna@example.org - Angebot/Anhänge/file0.pdf|file0.pdf]]\n"), "{idx}");
    assert_eq!(mail.size, Some(idx.len() as u64), "a node's size is the size of its index");
    let entry = de.aspect_entry("sample:mailbox", &mail.node, None).await.unwrap();
    assert_eq!((entry.size, entry.ino, entry.name.as_str()), (mail.size, mail.ino, "anna@example.org - Angebot"));
    assert!(mail.changed_at.is_some());

    let inside = de.aspect_children("sample:mailbox", &mail.node).await.unwrap();
    let md = inside.iter().find(|n| n.name == "mail.md").unwrap();
    assert_eq!(md.size, Some(7));
    assert_eq!(inside.iter().find(|n| n.name == "mail.eml").unwrap().size, Some(19));
    // the same text has the same inode in every view
    let by_id = de.aspect_children("_id", &format!("o:{m}")).await.unwrap();
    assert_eq!(by_id.iter().find(|n| n.name == "mail.md").unwrap().ino, md.ino);
    // in the built-in views the index links the filled ends below the object's own directory
    let idx = de.aspect_index("_id", &format!("o:{m}"), Some(&m.to_string())).await.unwrap();
    assert!(idx.contains(&format!("## attachments\n\n- [[{m}/attachments/")), "{idx}");
}

#[tokio::test]
async fn tools_over_the_api() {
    let env = env().await;
    let m = mail_fixture(&env).await;
    let de = env.admin.clone().with_lang("de");
    let sh = |cwd: &'static str, argv: Vec<&'static str>| {
        let de = de.clone();
        async move {
            let v = de.tool_exec(cwd, &argv).await.unwrap();
            (v["stdout"].as_str().unwrap().to_string(), v["stderr"].as_str().unwrap().to_string(), v["status"].as_i64().unwrap())
        }
    };
    let mail = "/sample:mailbox/2026-11-24/anna@example.org - Angebot";
    let root = sh("/", vec!["ls", "/"]).await.0;
    assert!(root.contains("_id\n_id.md\n_type\n_type.md\n") && root.contains("sample:mailbox\nsample:mailbox.md\n"), "{root}");
    assert_eq!(sh("/", vec!["ls", "/sample:mailbox"]).await.0, "2026-11-24\n2026-11-24.md\n2026-11-25\n2026-11-25.md\n");
    assert_eq!(sh("/sample:mailbox/2026-11-24", vec!["cat", "anna@example.org - Angebot/mail.md"]).await.0, "# Hallo");
    // grep -r searches texts and indexes, not binary content (mail.eml also contains "Hallo")
    assert_eq!(sh("/", vec!["grep", "-r", "Hallo", "/sample:mailbox"]).await.0, format!("{mail}/mail.md:# Hallo\n"));
    assert_eq!(sh("/", vec!["grep", "-rlF", "subject: \"Angebot", "/sample:mailbox"]).await.0, format!("{mail}.md\n"));
    // attachments link back to their mail by id
    assert_eq!(sh("/", vec!["grep", "-rlF", "Angebot]]", "/sample:mailbox"]).await.0.lines().count(), 2);
    assert_eq!(sh("/", vec!["grep", "-c", "Hallo", &*Box::leak(format!("{mail}/mail.eml").into_boxed_str())]).await.0, "1\n", "named files are searched");
    let pdfs = sh("/sample:mailbox", vec!["find", ".", "-name", "*.pdf", "-type", "f"]).await.0;
    assert_eq!(pdfs, "./2026-11-24/anna@example.org - Angebot/Anhänge/file0.pdf/file0.pdf\n");
    assert_eq!(sh("/sample:mailbox", vec!["find", ".", "-name", "mail.md", "-exec", "grep", "-l", "Hallo", "{}", "+"]).await.0, "./2026-11-24/anna@example.org - Angebot/mail.md\n");
    assert_eq!(sh(mail_static(), vec!["wc", "-c", "mail.md"]).await.0, "7 mail.md\n");
    assert_eq!(sh(mail_static(), vec!["head", "-n", "1", "../anna@example.org - Angebot.md"]).await.0, "---\n");
    assert!(sh(mail_static(), vec!["stat", "mail.md"]).await.0.contains("Links: 2"));
    assert!(sh(mail_static(), vec!["ls", "-l"]).await.0.contains("-r--r--r-- 2 base base"));
    assert_eq!(sh("/", vec!["cat", &*Box::leak(format!("/_id/{m}/mail.md").into_boxed_str())]).await.0, "# Hallo");
    assert_eq!(sh("/", vec!["cat", "/nope"]).await, (String::new(), "cat: /nope: No such file or directory\n".into(), 1));

    let e = de.tool_exec("/", &["grep", "-P", "x", "/"]).await.unwrap_err();
    assert_eq!((e.status(), e.error_id()), (Some(400), Some("tool.unknown_option")));
    assert!(e.to_string().contains("\"-r\""), "the error lists the supported options: {e}");

    let v = de.tool("grep", &json!({"cwd": "/sample:mailbox", "paths": ["."], "options": {"recursive": true, "patterns": ["Hallo"], "lineNumber": true}})).await.unwrap();
    assert_eq!(v["records"], json!([{"kind": "match", "path": "./2026-11-24/anna@example.org - Angebot/mail.md", "line": 1, "text": "# Hallo"}]));
    assert_eq!(de.tool("grep", &json!({"paths": ["/"], "options": {"long": true, "patterns": ["x"]}})).await.unwrap_err().error_id(), Some("tool.unknown_option"));

    // every request counts once
    let before = env.admin.metrics().await.unwrap()["requests"].as_u64().unwrap();
    de.tool_exec("/", &["ls", "/"]).await.unwrap();
    assert_eq!(env.admin.metrics().await.unwrap()["requests"].as_u64().unwrap(), before + 1);

    // without read rights there is nothing to find
    let (_, lea) = env.person("lea", &[]).await;
    let v = lea.tool_exec("/", &["grep", "-r", "Hallo", "/sample:mailbox"]).await.unwrap();
    assert_eq!((v["stdout"].as_str().unwrap(), v["status"].as_i64().unwrap()), ("", 1));
}

#[tokio::test]
async fn equal_names_use_the_fallback_then_the_id() {
    let env = env().await;
    let de = env.admin.clone().with_lang("de");
    let mut o = Order::new();
    // two mails with equal sender and subject at different times, two more at the same time
    for (k, at) in [("a", "08:15"), ("b", "09:30"), ("c", "10:00"), ("d", "10:00")] {
        o.create(k, None, json!({"sample:Mail": {"receivedAt": format!("2026-11-24T{at}:00+01:00"), "sender": "anna@example.org", "subject": "Angebot"}}));
    }
    o.create("x", None, json!({"sample:MailAttachment": {"filename": "file.pdf"}}));
    o.link("sample:MailAttachment.mail", "x", "d");
    let r = env.admin.order(&o).await.unwrap();
    let ls = |cwd: &'static str| {
        let de = de.clone();
        async move { de.tool_exec(cwd, &["ls", "."]).await.unwrap()["stdout"].as_str().unwrap().to_string() }
    };
    let day = ls("/sample:mailbox/2026-11-24").await;
    let dirs: Vec<&str> = day.lines().filter(|l| !l.ends_with(".md")).collect();
    assert_eq!(dirs.len(), 4, "{day}");
    assert!(dirs.iter().any(|d| d.starts_with("anna@example.org - Angebot (07:15)")), "fallback with the time: {day}");
    let at10: Vec<&&str> = dirs.iter().filter(|d| d.contains("(09:00)")).collect();
    assert_eq!(at10.len(), 2, "{day}");
    assert!(at10.iter().any(|d| d.ends_with(" ~2")), "same time: numbered: {day}");
    // the numbering follows the id, so a name always means the same mail
    let (c, d) = (r.created["c"], r.created["d"]);
    let (first, second) = if c < d { (c, d) } else { (d, c) };
    let plain = at10.iter().find(|x| !x.ends_with(" ~2")).unwrap().to_string();
    let numbered = format!("{plain} ~2");
    let id_of = |name: String| {
        let de = de.clone();
        async move {
            let p = format!("/sample:mailbox/2026-11-24/{name}.md");
            let v = de.tool_exec("/", &["head", "-n", "2", &p]).await.unwrap();
            v["stdout"].as_str().unwrap().lines().nth(1).unwrap().trim_start_matches("_id: ").trim_matches('"').parse::<Uuid>().unwrap()
        }
    };
    assert_eq!(id_of(plain.clone()).await, first);
    assert_eq!(id_of(numbered.clone()).await, second);
    // the index of the attachment links to exactly its mail's entry
    let att = de.aspect_children("sample:mailbox", &format!("f:{d}:attachments")).await.unwrap();
    let idx = de.aspect_index("sample:mailbox", &att[0].node, Some("file.pdf")).await.unwrap();
    let mail_name = if d == first { &plain } else { &numbered };
    assert!(!idx.is_empty());
    let mail_idx = de.aspect_index("sample:mailbox", &format!("o:{d}"), Some(mail_name)).await.unwrap();
    assert!(mail_idx.contains(&format!("[[{mail_name}/Anhänge/file.pdf|")), "{mail_idx}");
    // without a collision the plain name stays
    assert!(ls("/sample:mailbox").await.contains("2026-11-24\n"));
}

fn mail_static() -> &'static str {
    "/sample:mailbox/2026-11-24/anna@example.org - Angebot"
}

#[tokio::test]
async fn tab_paging_sorting_and_export() {
    let env = env().await;
    let mut o = Order::new();
    o.create("m", None, json!({"sample:Mail": {"receivedAt": "2026-11-24T08:15:00+01:00", "sender": "anna@example.org", "subject": "Angebot"}}));
    for (j, f) in ["b.pdf", "a, \"quoted\".pdf", "c.pdf"].iter().enumerate() {
        let a = format!("a{j}");
        o.create(&a, None, json!({"sample:MailAttachment": {"filename": f}}));
        o.link("sample:MailAttachment.mail", a.as_str(), "m");
    }
    let m = env.admin.order(&o).await.unwrap().created["m"];
    let node = format!("o:{m}");
    let page = env.admin.aspect_tab("sample:mailbox", &node, "attachments", &[("sort", "0"), ("limit", "2")]).await.unwrap();
    assert_eq!(page["total"], 3);
    let names: Vec<_> = page["rows"].as_array().unwrap().iter().map(|r| r["cells"][0][0].as_str().unwrap().to_string()).collect();
    assert_eq!(names, vec!["a, \"quoted\".pdf", "b.pdf"]);
    assert_eq!(page["rows"][0]["type"], "sample:MailAttachment");
    let last = env.admin.aspect_tab("sample:mailbox", &node, "attachments", &[("sort", "0"), ("desc", "true"), ("offset", "2")]).await.unwrap();
    assert_eq!(last["rows"][0]["cells"][0][0], "a, \"quoted\".pdf");
    // export: every row, not only a page
    let (csv, ct, cd) = env.admin.aspect_tab_export("sample:mailbox", &node, "attachments", "csv").await.unwrap();
    assert!(ct.starts_with("text/csv") && cd.contains(".csv"));
    let csv = String::from_utf8(csv).unwrap();
    let lines: Vec<_> = csv.trim_start_matches('\u{feff}').lines().collect();
    assert_eq!(lines.len(), 4, "{csv}");
    assert_eq!(lines[0], "File,Added");
    assert!(lines.iter().any(|l| l.starts_with("\"a, \"\"quoted\"\".pdf\",")), "{csv}");
    let (xlsx, ct, _) = env.admin.aspect_tab_export("sample:mailbox", &node, "attachments", "xlsx").await.unwrap();
    assert!(ct.contains("spreadsheetml") && xlsx.starts_with(b"PK"));
    assert_eq!(env.admin.aspect_tab_export("sample:mailbox", &node, "attachments", "pdf").await.unwrap_err().status(), Some(400));
    assert_eq!(env.admin.aspect_tab("sample:mailbox", &node, "nope", &[]).await.unwrap_err().status(), Some(404));
}


#[tokio::test]
async fn rights_path_team_leader_sees_team_requests() {
    let env = env().await;
    // two teams with leaders and one member each, one request per member
    let mut o = Order::new();
    for t in ["a", "b"] {
        o.create(&format!("team-{t}"), None, json!({"t:Team": {"name": t}}));
        o.create(&format!("m-{t}"), None, json!({"base:Person": {"firstName": "M", "lastName": t}, "t:Staff": {}}));
        o.link("t:Staff.team", format!("m-{t}").as_str(), format!("team-{t}").as_str());
        o.create(&format!("lr-{t}"), None, json!({"t:LeaveRequest": {"days": 3}}));
        o.link("t:LeaveRequest.applicant", format!("lr-{t}").as_str(), format!("m-{t}").as_str());
    }
    let r = env.admin.order(&o).await.unwrap();
    let (lead_a, ca) = env.person("lead-a", &[]).await;
    let (_lead_b, cb) = env.person("lead-b", &[]).await;
    for (p, team) in [(lead_a, r.created["team-a"])] {
        let mut o = Order::new();
        o.link("t:Team.leader", team, p);
        env.admin.order(&o).await.unwrap();
    }
    let mut o = Order::new();
    o.link("t:Team.leader", r.created["team-b"], _lead_b);
    env.admin.order(&o).await.unwrap();
    for p in [lead_a, _lead_b] {
        env.admin.assign("t:lead-leave", p).await.unwrap();
        env.admin.assign("t:read-persons", p).await.unwrap();
    }
    let q = i3cm_client::ListQuery::default();
    let la: Vec<Uuid> = ca.list("t:LeaveRequest", &q).await.unwrap().iter().map(|s| s.id).collect();
    let lb: Vec<Uuid> = cb.list("t:LeaveRequest", &q).await.unwrap().iter().map(|s| s.id).collect();
    assert_eq!(la, vec![r.created["lr-a"]]);
    assert_eq!(lb, vec![r.created["lr-b"]]);
    // single access: own team yes, other team not found
    assert!(ca.get_object(r.created["lr-a"], None).await.is_ok());
    assert!(ca.get_object(r.created["lr-b"], None).await.is_err());
    // write through the rights path
    let mut o = Order::new();
    o.update(r.created["lr-a"], json!({"t:LeaveRequest": {"days": 5}}));
    assert!(ca.order(&o).await.is_ok());
    let mut o = Order::new();
    o.update(r.created["lr-b"], json!({"t:LeaveRequest": {"days": 5}}));
    assert!(ca.order(&o).await.is_err());
    // a person without the profile sees nothing
    let (_x, cx) = env.person("nobody", &[]).await;
    assert!(cx.list("t:LeaveRequest", &q).await.unwrap().is_empty());
}

#[tokio::test]
async fn rights_relevant_links_need_only_write_and_references_have_no_direction() {
    let env = env().await;
    let mut o = Order::new();
    o.create("team", None, json!({"t:Team": {"name": "x"}}));
    let team = env.admin.order(&o).await.unwrap().created["team"];
    // t:Team.leader is rights-relevant (the path applicant.team.leader runs over it). Writing data
    // never grants rights: write on the team and read on the person suffice to set the leader,
    // and to remove it again
    let (w, cw) = env.person("writer", &[]).await;
    env.admin.assign("t:team-writer", w).await.unwrap();
    env.admin.assign("t:read-persons", w).await.unwrap();
    let mut o = Order::new();
    o.link("t:Team.leader", team, w);
    let link = cw.order(&o).await.expect("write on the team is enough").links[0];
    let mut o = Order::new();
    o.unlink("t:Team.leader", link);
    assert!(cw.order(&o).await.is_ok());
    // without write on the team it is refused (read on the person alone is not enough)
    let (n, cn) = env.person("none", &[]).await;
    env.admin.assign("t:read-persons", n).await.unwrap();
    let mut o = Order::new();
    o.link("t:Team.leader", team, n);
    assert!(cn.order(&o).await.is_err(), "read on both sides is not enough");
    // an ordinary reference: write on either object and read on the other, from either side
    let (pw, cpw) = env.person("pw", &[]).await;
    env.admin.assign("t:person-writer", pw).await.unwrap();
    let mut o = Order::new();
    o.link("t:Team.mascot", team, pw);
    let link = cpw.order(&o).await.expect("write on the person, read on the team").links[0];
    let mut o = Order::new();
    o.unlink("t:Team.mascot", link);
    assert!(cpw.order(&o).await.is_ok());
    let mut o = Order::new();
    o.link("t:Team.mascot", team, n);
    assert!(cn.order(&o).await.is_err(), "read on both sides is not enough");
    // the other orientation of a rights-relevant reference: write on the person, read on the team
    let mut o = Order::new();
    o.link("t:Team.leader", team, pw);
    let link = cpw.order(&o).await.expect("write on the person end, read on the team").links[0];
    let mut o = Order::new();
    o.unlink("t:Team.leader", link);
    assert!(cpw.order(&o).await.is_ok());
    // group memberships still need the right to grant rights
    let g = env.group("club", None).await;
    let (mw, cmw) = env.person("mw", &[]).await;
    env.admin.assign("t:member-writer", mw).await.unwrap();
    let mut o = Order::new();
    o.create("m", None, json!({"base:GroupMembership": {}}));
    o.link("base:GroupMembership.group", "m", g).link("base:GroupMembership.member", "m", mw);
    let e = cmw.order(&o).await.unwrap_err();
    assert!(format!("{e}").contains("grant"), "{e}");
}

#[tokio::test]
async fn an_assignee_assigns_others_with_write_only() {
    let env = env().await;
    let (w, cw) = env.person("worker", &[]).await;
    let (other, _) = env.person("other", &[]).await;
    env.admin.assign("t:task-worker", w).await.unwrap();
    let mut o = Order::new();
    o.create("mine", None, json!({"sample:Task": {"subject": "mine", "status": "open"}}));
    o.link("sample:Task.assignees", "mine", w);
    o.create("foreign", None, json!({"sample:Task": {"subject": "foreign", "status": "open"}}));
    let r = env.admin.order(&o).await.unwrap();
    let (mine, foreign) = (r.created["mine"], r.created["foreign"]);
    // write through the rights path on the task and read on the person suffice
    let mut o = Order::new();
    o.link("sample:Task.assignees", mine, other);
    cw.order(&o).await.expect("an assignee adds another assignee");
    // a task the worker cannot write stays out of reach: no write, no link
    let mut o = Order::new();
    o.link("sample:Task.assignees", foreign, w);
    assert!(cw.order(&o).await.is_err(), "no write on a foreign task");
}

#[tokio::test]
async fn deleting_ends_references_and_takes_parts_along() {
    let env = env().await;
    let acc = env.accounts().await;
    let mut o = Order::new();
    env.journal(&mut o, 7, "2026-02-03", acc);
    let r = env.admin.order(&o).await.unwrap();
    let (e, line) = (r.created["e7"], r.created["e7-debit"]);
    // deleting an account that a line still needs (min 1) is refused
    let mut o = Order::new();
    o.delete(acc.0);
    assert!(env.admin.order(&o).await.is_err());
    // deleting the whole deletes its parts and ends their references at both ends
    let mut o = Order::new();
    o.delete(e);
    env.admin.order(&o).await.unwrap();
    assert_eq!(env.admin.get_object(line, None).await.unwrap_err().status(), Some(404));
    let open: i64 = {
        let c = env.store.client().await.unwrap();
        let t = env.store.model.ref_def("fibu:JournalLine.account").unwrap().clone();
        c.query_one(&format!("SELECT count(*) FROM {} WHERE a_id=$1 AND valid_to IS NULL", i3cm_store::schema::ref_table(&t)), &[&line]).await.unwrap().get(0)
    };
    assert_eq!(open, 0, "references of deleted objects end");
    // now the account can go
    let mut o = Order::new();
    o.delete(acc.0);
    env.admin.order(&o).await.unwrap();
}

#[test]
fn rights_path_checker_limits() {
    let json = std::fs::read_to_string(i3cm_server::test_catalog()).unwrap();
    let base: Catalog = serde_json::from_str(&json).unwrap();
    let with = |when: &str, sign: &str, types: &str| {
        let mut m = test_model().catalog.clone();
        let _ = &base;
        m.modules.last_mut().unwrap().profiles.push(serde_json::from_value(json!({"name": "t:x", "rights": [{"sign": sign, "art": ["read"], "types": [types], "room": "root", "when": when}]})).unwrap());
        Model::new(m).err().map(|e| e.error_id)
    };
    assert_eq!(with("applicant.team.leader = %me", "+", "t:LeaveRequest"), None);
    assert_eq!(with("applicant.team.leader", "+", "t:LeaveRequest").as_deref(), Some("path.rights_form"));
    assert_eq!(with("applicant.team.leader.leads.team = %me", "+", "t:LeaveRequest").as_deref(), Some("path.too_long"));
    assert_eq!(with("applicant.team.leader = %me", "-", "t:LeaveRequest").as_deref(), Some("profile.conditional_sign"));
    assert_eq!(with("applicant.team.leader = %me", "+", "*").as_deref(), Some("profile.conditional_any"));
    assert_eq!(with("nothere = %me", "+", "t:LeaveRequest").as_deref(), Some("path.unknown_step"));
    // an end of an unrelated type is not resolved: Team.leader from a request
    assert_eq!(with("leader = %me", "+", "t:LeaveRequest").as_deref(), Some("path.unknown_step"));
}


#[tokio::test]
async fn view_filter_due_and_mine() {
    let env = env().await;
    let (me, cme) = env.person("worker", &[]).await;
    let team = env.group("team", None).await;
    env.add_member(team, me).await;
    let mut o = Order::new();
    let tasks = [("past-mine", "2020-01-01T00:00:00Z", "open", true), ("future-mine", "2099-01-01T00:00:00Z", "open", true),
                 ("past-done", "2020-01-01T00:00:00Z", "done", true), ("past-other", "2020-01-01T00:00:00Z", "open", false)];
    for (k, due, st, mine) in tasks {
        o.create(k, None, json!({"sample:Task": {"subject": k, "due": due, "status": st}}));
        if mine {
            o.link("sample:Task.assignees", k, me);
        }
    }
    o.create("via-group", None, json!({"sample:Task": {"subject": "via-group", "due": "2020-01-02T00:00:00Z", "status": "open"}}));
    o.link("sample:Task.assignees", "via-group", team);
    env.admin.order(&o).await.unwrap();
    env.admin.assign("base:reader", me).await.unwrap();
    let kids = cme.aspect_children("t:due-mine", "").await.unwrap();
    let mut names: Vec<String> = kids.iter().map(|n| n.display_name.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["past-mine".to_string(), "via-group".to_string()]);
    // bad filters are rejected when the catalog is loaded
    let mut c = test_model().catalog.clone();
    c.modules.last_mut().unwrap().aspects[1].entry.filter = Some("subject.length() > 3".into());
    assert_eq!(Model::new(c).err().map(|e| e.error_id).as_deref(), Some("path.bad_filter"));
}


#[tokio::test]
async fn write_through_views() {
    let env = env().await;
    let mut o = Order::new();
    o.create("m", None, json!({"sample:Mail": {"receivedAt": "2026-11-24T08:15:00Z", "sender": "anna@example.org", "subject": "Angebot"}}));
    let m = env.admin.order(&o).await.unwrap().created["m"];
    env.admin.put_text(m, "sample:mail.md", "# Hallo").await.unwrap();
    env.admin.put_content(m, b"From: anna".to_vec(), "message/rfc822", None).await.unwrap();
    let node = format!("o:{m}");
    // text and content
    env.admin.put_aspect_file("t:mail-drop", &node, "mail.md", b"# Neu".to_vec()).await.unwrap();
    assert_eq!(env.admin.aspect_file("t:mail-drop", &node, "mail.md").await.unwrap(), b"# Neu");
    env.admin.put_aspect_file("t:mail-drop", &node, "mail.eml", b"From: ben".to_vec()).await.unwrap();
    assert_eq!(env.admin.get_content(m).await.unwrap(), b"From: ben");
    // attributes in the front matter of the index
    let idx = env.admin.aspect_index("t:mail-drop", &node, None).await.unwrap();
    let changed = idx.replace("\"subject\": \"Angebot\"", "x").replace("subject: \"Angebot\"", "subject: \"Auftrag\"");
    assert_ne!(idx, changed);
    env.admin.put_aspect_index("t:mail-drop", &node, &changed).await.unwrap();
    assert_eq!(env.admin.get_object(m, None).await.unwrap().parts.iter().find(|t| t.type_name == "sample:Mail").unwrap().attributes["subject"], json!("Auftrag"));
    // an invalid value fails as a whole, nothing is stored
    let bad = changed.replace("receivedAt: \"", "receivedAt: \"x");
    assert!(env.admin.put_aspect_index("t:mail-drop", &node, &bad).await.is_err());
    // the body (references) is not writable
    assert_eq!(env.admin.put_aspect_index("t:mail-drop", &node, &format!("{changed}\n- [[/x|y]]\n")).await.unwrap_err().error_id(), Some("index.body_readonly"));
    // a new file in the folder becomes an attachment
    env.admin.drop_file("t:mail-drop", &format!("f:{m}:attachments"), "neu.pdf", b"%PDF-9".to_vec()).await.unwrap();
    let atts = env.admin.aspect_children("t:mail-drop", &format!("f:{m}:attachments")).await.unwrap();
    assert_eq!(atts.len(), 1);
    // without write right: denied
    let (_p, cp) = env.person("reader", &[]).await;
    env.admin.assign("base:reader", _p).await.unwrap();
    assert!(cp.put_aspect_file("t:mail-drop", &node, "mail.md", b"x".to_vec()).await.is_err());
}

#[tokio::test]
async fn sed_in_place_is_one_order() {
    let env = env().await;
    let mut o = Order::new();
    o.create("m1", None, json!({"sample:Mail": {"receivedAt": "2026-11-24T08:15:00Z", "sender": "anna@example.org", "subject": "Angebot"}}));
    o.create("m2", None, json!({"sample:Mail": {"receivedAt": "2026-11-24T09:15:00Z", "sender": "ben@example.org", "subject": "Hi there"}}));
    let r = env.admin.order(&o).await.unwrap();
    let (m1, m2) = (r.created["m1"], r.created["m2"]);
    for m in [m1, m2] {
        env.admin.put_text(m, "sample:mail.md", "# Hallo Welt").await.unwrap();
    }
    let ls = env.admin.tool_exec("/", &["ls", &format!("/_id/{m1}")]).await.unwrap();
    assert!(ls["stdout"].as_str().unwrap().contains("mail.md"), "{ls}");
    // two text files: one order, both changed
    let res = env.admin.tool_exec("/", &["sed", "-i", "s/Hallo/Hi/", &format!("/_id/{m1}/mail.md"), &format!("/_id/{m2}/mail.md")]).await.unwrap();
    assert_eq!(res["status"], json!(0), "{res}");
    for m in [m1, m2] {
        assert_eq!(env.admin.get_text(m, "sample:mail.md").await.unwrap(), "# Hi Welt");
    }
    // a text plus an index whose body would change: rejected as a whole, nothing written
    let res = env.admin.tool_exec("/", &["sed", "-i", "s/Hi/Ho/", &format!("/_id/{m1}/mail.md"), &format!("/_id/{m2}.md")]).await.unwrap();
    assert_ne!(res["status"], json!(0), "{res}");
    assert_eq!(env.admin.get_text(m1, "sample:mail.md").await.unwrap(), "# Hi Welt");
    // without -i: output only
    let res = env.admin.tool_exec("/", &["sed", "s/Hi/Ho/", &format!("/_id/{m1}/mail.md")]).await.unwrap();
    assert_eq!(res["stdout"], json!("# Ho Welt"));
}

#[tokio::test]
async fn self_description_version_and_paths() {
    let env = env().await;
    let base = env.admin.base_url().to_string();
    let http = reqwest::Client::new();
    for p in ["/", "/guide/agents", "/openapi.json", "/skill.zip", "/v1/version"] {
        let r = http.get(format!("{base}{p}")).send().await.unwrap();
        assert!(r.status().is_success(), "{p}: {}", r.status());
    }
    let api: Value = http.get(format!("{base}/openapi.json")).send().await.unwrap().json().await.unwrap();
    assert!(api["paths"]["/v1/aspects/{a}/path"]["get"].is_object());
    // path of a mail in the mailbox: day group, then the mail
    let mut o = Order::new();
    o.create("m", None, json!({"sample:Mail": {"receivedAt": "2026-11-24T08:15:00Z", "sender": "anna@example.org", "subject": "Angebot"}}));
    let m = env.admin.order(&o).await.unwrap().created["m"];
    let chain: Value = env.admin.get_q_value("/aspects/sample:mailbox/path", &[("node", &format!("o:{m}"))]).await.unwrap();
    let nodes: Vec<&str> = chain.as_array().unwrap().iter().map(|n| n["node"].as_str().unwrap()).collect();
    assert_eq!(nodes, vec!["g:2026-11-24".to_string(), format!("o:{m}")]);
}

#[tokio::test]
async fn unused_tokens_lapse() {
    let env = env().await;
    let (p, cp) = env.person("idle", &[]).await;
    assert!(cp.me().await.is_ok());
    let c = env.store.client().await.unwrap();
    c.execute("UPDATE token SET last_used = now() - interval '91 days' WHERE account=$1", &[&p]).await.unwrap();
    assert_eq!(cp.me().await.unwrap_err().error_id(), Some("auth.unused_token"));
}

#[tokio::test]
async fn structure_content_and_texts_in_one_order() {
    let env = env().await;
    let mut o = Order::new();
    o.create("d", None, json!({"sample:Document": {"title": "Notes"}}));
    o.content("d", b"hello world", "text/plain", Some("notes.txt"));
    o.text("d", "sample:plain", "hello world");
    let d = env.admin.order(&o).await.unwrap().created["d"];
    assert_eq!(env.admin.get_content(d).await.unwrap(), b"hello world".to_vec());
    assert_eq!(env.admin.get_text(d, "sample:plain").await.unwrap(), "hello world");

    // all or nothing: a bad text type rolls back the object and its content
    let before = env.admin.list("sample:Document", &ListQuery::default()).await.unwrap().len();
    let mut o = Order::new();
    o.create("e", None, json!({"sample:Document": {"title": "Broken"}}));
    o.content("e", b"x", "text/plain", None);
    o.text("e", "sample:nope", "x");
    assert_eq!(env.admin.order(&o).await.unwrap_err().error_id(), Some("text.unknown_type"));
    assert_eq!(env.admin.list("sample:Document", &ListQuery::default()).await.unwrap().len(), before);

    let mut o = Order::new();
    o.ops.push(json!({"op": "content", "id": d.to_string(), "mediaType": "text/plain", "base64": "%%%"}));
    assert_eq!(env.admin.order(&o).await.unwrap_err().error_id(), Some("content.bad_base64"));
}

#[tokio::test]
async fn deactivated_accounts_cannot_act() {
    let env = env().await;
    let (lea_id, lea) = env.person("lea-off", &[]).await;
    lea.me().await.unwrap();
    let mut o = Order::new();
    o.update(lea_id, json!({"base:PersonAccount": {"deactivated": true}}));
    env.admin.order(&o).await.unwrap();
    let e = lea.me().await.unwrap_err();
    assert_eq!((e.status(), e.error_id()), (Some(401), Some("auth.account_inactive")));
    let mut o = Order::new();
    o.update(lea_id, json!({"base:PersonAccount": {"deactivated": false}}));
    env.admin.order(&o).await.unwrap();
    assert_eq!(lea.me().await.unwrap().id, lea_id, "reactivated: the same token works again");
}

#[tokio::test]
async fn the_last_admin_is_protected() {
    let env = env().await;
    let admin = env.init.admin_account;
    let admins = env.init.admin_group;
    let last = |e: i3cm_client::ClientError| assert_eq!(e.error_id(), Some("tenant.last_admin"), "{e:?}");
    // deactivating, deleting the account, deleting the group, ending the membership: all refused
    let mut o = Order::new();
    o.update(admin, json!({"base:PersonAccount": {"deactivated": true}}));
    last(env.admin.order(&o).await.unwrap_err());
    let mut o = Order::new();
    o.delete(admin);
    last(env.admin.order(&o).await.unwrap_err());
    let mut o = Order::new();
    o.delete(admins);
    last(env.admin.order(&o).await.unwrap_err());
    let membership = env.store.client().await.unwrap()
        .query_one("SELECT m.a_id FROM r_base_groupmembership_member m WHERE m.tenant_id=$1 AND m.b_id=$2 AND m.valid_to IS NULL", &[&env.init.tenant, &admin])
        .await.unwrap().get::<_, Uuid>(0);
    let mut o = Order::new();
    o.delete(membership);
    last(env.admin.order(&o).await.unwrap_err());
    // removing the guardian profile of the group: refused
    let a = env.admin.assignments(Some(admins)).await.unwrap();
    let id: Uuid = serde_json::from_value(a.iter().find(|x| x["profile"] == "base:admin").unwrap()["id"].clone()).unwrap();
    last(env.admin.unassign(id).await.unwrap_err());
    env.admin.me().await.unwrap();
    // with a second administrator the first may step down
    let (bob_id, bob) = env.person("bob-admin", &[admins]).await;
    let mut o = Order::new();
    o.update(admin, json!({"base:PersonAccount": {"deactivated": true}}));
    env.admin.order(&o).await.unwrap();
    assert_eq!(env.admin.me().await.unwrap_err().error_id(), Some("auth.account_inactive"));
    // now bob is the last one: he cannot deactivate himself
    let mut o = Order::new();
    o.update(bob_id, json!({"base:PersonAccount": {"deactivated": true}}));
    last(bob.order(&o).await.unwrap_err());
    // an order on other objects is not affected
    let g = bob.order(Order::new().create("g", None, json!({"base:Group": {"name": "plain"}}))).await.unwrap().created["g"];
    bob.order(Order::new().delete(g)).await.unwrap();
}

#[tokio::test]
async fn concurrent_orders_cannot_remove_both_last_admins() {
    let env = env().await;
    let admins = env.init.admin_group;
    let (bob_id, _bob) = env.person("bob-race", &[admins]).await;
    let off = |id: Uuid| {
        let mut o = Order::new();
        o.update(id, json!({"base:PersonAccount": {"deactivated": true}}));
        o
    };
    let (o1, o2) = (off(env.init.admin_account), off(bob_id));
    let (a, b) = tokio::join!(env.admin.order(&o1), env.admin.order(&o2));
    let ok = [a.is_ok(), b.is_ok()].iter().filter(|x| **x).count();
    assert_eq!(ok, 1, "exactly one of two concurrent removals may commit: {a:?} {b:?}");
    let err = if a.is_err() { a.unwrap_err() } else { b.unwrap_err() };
    assert!(matches!(err.error_id(), Some("tenant.last_admin") | Some("auth.account_inactive")), "{err:?}");
}

#[tokio::test]
async fn attaching_an_account_type_cannot_deactivate_the_last_admin() {
    let env = env().await;
    let mut o = Order::new();
    o.attach(env.init.admin_account, "base:AgentAccount", json!({"deactivated": true}));
    let e = env.admin.order(&o).await.unwrap_err();
    assert_eq!(e.error_id(), Some("tenant.last_admin"), "{e:?}");
    env.admin.me().await.unwrap();
}

// ---- uploads, file store, idempotency ----

fn sha_hex(b: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(b))
}

async fn http_get(env: &Env, path: &str, range: Option<&str>) -> (u16, reqwest::header::HeaderMap, Vec<u8>) {
    let mut r = reqwest::Client::new().get(format!("{}/v1{}", env.admin.base_url(), path)).bearer_auth(&env.init.token);
    if let Some(x) = range {
        r = r.header("range", x);
    }
    let resp = r.send().await.unwrap();
    let st = resp.status().as_u16();
    let h = resp.headers().clone();
    (st, h, resp.bytes().await.unwrap().to_vec())
}

#[tokio::test]
async fn upload_in_parts_with_a_repeated_part_and_range_download() {
    let env = env().await;
    // 3 parts, the last one shorter: 1 MiB + 1 MiB + 12345 bytes
    let data: Vec<u8> = (0..(2 * (1 << 20) + 12345)).map(|i: usize| (i * 31 % 251) as u8).collect();
    let sha = sha_hex(&data);
    let up = env.admin.upload_open(Some(&sha)).await.unwrap();
    let id = up["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("upl_"));
    let parts: Vec<&[u8]> = data.chunks(1 << 20).collect();
    env.admin.upload_part(&id, 2, parts[1].to_vec()).await.unwrap();
    env.admin.upload_part(&id, 1, parts[0].to_vec()).await.unwrap();
    // the same part again changes nothing; other bytes under the same number are a conflict
    env.admin.upload_part(&id, 1, parts[0].to_vec()).await.unwrap();
    assert_eq!(env.admin.upload_part(&id, 1, b"other".to_vec()).await.unwrap_err().error_id(), Some("upload.part_conflict"));
    // completing with the number of parts names the missing one
    let e = env.admin.upload_complete(&id, None, Some(3)).await.unwrap_err();
    assert_eq!(e.error_id(), Some("upload.incomplete"));
    // without it, the checksum catches the missing last part, and the upload stays open
    assert_eq!(env.admin.upload_complete(&id, None, None).await.unwrap_err().error_id(), Some("upload.checksum_mismatch"));
    env.admin.upload_part(&id, 3, parts[2].to_vec()).await.unwrap();
    let done = env.admin.upload_complete(&id, None, Some(3)).await.unwrap();
    assert_eq!(done["sha256"], sha);
    assert_eq!(done["size"], data.len());
    // completing twice gives the same answer
    assert_eq!(env.admin.upload_complete(&id, None, None).await.unwrap()["sha256"], sha);

    let mut o = Order::new();
    o.create("d", None, json!({"sample:Document": {"title": "Big"}}));
    o.content_upload("d", &id, "application/octet-stream", Some("big.bin"));
    let r = env.admin.order(&o).await.unwrap();
    assert!(r.warnings.is_empty());
    let d = r.created["d"];
    let back = env.admin.get_content(d).await.unwrap();
    assert_eq!(sha_hex(&back), sha);
    // stored as a file of the file store, not in the database
    assert!(env.store.files().unwrap().has(&sha));
    let c = env.store.client().await.unwrap();
    let in_db: i64 = c.query_one("SELECT count(*) FROM blob WHERE sha256=$1", &[&sha]).await.unwrap().get(0);
    assert_eq!(in_db, 0);

    // ranges
    let (st, h, body) = http_get(&env, &format!("/objects/{d}/content"), Some("bytes=10-19")).await;
    assert_eq!(st, 206);
    assert_eq!(body, data[10..20].to_vec());
    assert_eq!(h["content-range"], format!("bytes 10-19/{}", data.len()));
    assert_eq!(h["accept-ranges"], "bytes");
    let (st, _, body) = http_get(&env, &format!("/objects/{d}/content"), Some("bytes=-100")).await;
    assert_eq!(st, 206);
    assert_eq!(body, data[data.len() - 100..].to_vec());
    let (st, _, body) = http_get(&env, &format!("/objects/{d}/content"), Some(&format!("bytes={}-", (1 << 20) - 5))).await;
    assert_eq!(st, 206);
    assert_eq!(body, data[(1 << 20) - 5..].to_vec());
    let (st, _, _) = http_get(&env, &format!("/objects/{d}/content"), Some(&format!("bytes={}-", data.len() + 5))).await;
    assert_eq!(st, 416);

    // the handle is used up
    let mut o = Order::new();
    o.content_upload(d, &id, "application/octet-stream", None);
    assert_eq!(env.admin.order(&o).await.unwrap_err().error_id(), Some("upload.consumed"));
}

#[tokio::test]
async fn upload_rules_foreign_expired_checksum_and_sweep() {
    let store = Store::connect(&db_url(), test_model(), 16).await.unwrap().with_files(&data_dir()).unwrap().with_upload_ttl(1).unwrap();
    let env = env_with(store).await;
    let (_, lea) = env.person("lea-up", &[]).await;
    let mut o = Order::new();
    o.create("d", None, json!({"sample:Document": {"title": "Doc"}}));
    let d = env.admin.order(&o).await.unwrap().created["d"];

    // a handle of another account does not exist for the caller
    let theirs = lea.upload(b"from lea").await.unwrap();
    let mut o = Order::new();
    o.content_upload(d, &theirs, "text/plain", None);
    assert_eq!(env.admin.order(&o).await.unwrap_err().error_id(), Some("upload.not_found"));
    assert_eq!(env.admin.upload_part(&theirs, 1, b"x".to_vec()).await.unwrap_err().error_id(), Some("upload.not_found"));

    // a wrong checksum fails the upload
    let up = env.admin.upload_open(Some(&sha_hex(b"expected"))).await.unwrap();
    let id = up["id"].as_str().unwrap();
    env.admin.upload_part(id, 1, b"something else".to_vec()).await.unwrap();
    assert_eq!(env.admin.upload_complete(id, None, None).await.unwrap_err().error_id(), Some("upload.checksum_mismatch"));

    // an open upload is not a handle yet
    let open = env.admin.upload_open(None).await.unwrap()["id"].as_str().unwrap().to_string();
    env.admin.upload_part(&open, 1, b"half".to_vec()).await.unwrap();
    let mut o = Order::new();
    o.content_upload(d, &open, "text/plain", None);
    assert_eq!(env.admin.order(&o).await.unwrap_err().error_id(), Some("upload.incomplete"));

    // expiry: after the time to live a completed handle is refused, and the sweep empties staging
    let late = env.admin.upload(b"too late").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let mut o = Order::new();
    o.content_upload(d, &late, "text/plain", None);
    assert_eq!(env.admin.order(&o).await.unwrap_err().error_id(), Some("upload.expired"));
    env.store.sweep().await.unwrap();
    let fs = env.store.files().unwrap();
    for id in [&late, &open, &theirs] {
        assert!(!fs.root().join("staging").join(id.as_str()).exists(), "staging of {id} is gone");
    }
    let mut o = Order::new();
    o.content_upload(d, &late, "text/plain", None);
    assert_eq!(env.admin.order(&o).await.unwrap_err().error_id(), Some("upload.expired"));
}

#[tokio::test]
async fn idempotent_orders_write_once() {
    let env = env().await;
    let before = env.admin.list("sample:Document", &ListQuery::default()).await.unwrap().len();
    let mut o = Order::new();
    o.create("d", None, json!({"sample:Document": {"title": "Once"}}));
    let key = Uuid::new_v4().to_string();
    let a = env.admin.order_with_key(&o, &key).await.unwrap();
    let b = env.admin.order_with_key(&o, &key).await.unwrap();
    assert_eq!(a.txn, b.txn);
    assert_eq!(a.created, b.created);
    assert_eq!(env.admin.list("sample:Document", &ListQuery::default()).await.unwrap().len(), before + 1);
    // the repeat is marked
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/orders", env.admin.base_url()))
        .bearer_auth(&env.init.token)
        .header("idempotency-key", &key)
        .json(&serde_json::to_value(&o).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers()["idempotent-replay"], "true");
    // same key, other body
    let mut o2 = Order::new();
    o2.create("d", None, json!({"sample:Document": {"title": "Other"}}));
    let e = env.admin.order_with_key(&o2, &key).await.unwrap_err();
    assert_eq!(e.status(), Some(409));
    assert_eq!(e.error_id(), Some("idempotency.conflict"));
    // a failed order does not keep its key: fixing the order and retrying with the key works
    let mut bad = Order::new();
    bad.create("x", None, json!({"sample:Document": {"title": "X"}})).text("x", "sample:nope", "x");
    let k2 = Uuid::new_v4().to_string();
    assert!(env.admin.order_with_key(&bad, &k2).await.is_err());
    assert!(env.admin.order_with_key(&bad, &k2).await.is_err());
}

#[tokio::test]
async fn inline_content_is_limited_and_marked_deprecated() {
    let env = env().await;
    let mut o = Order::new();
    o.create("d", None, json!({"sample:Document": {"title": "Small"}}));
    o.content("d", b"small", "text/plain", None);
    let r = env.admin.order(&o).await.unwrap();
    assert_eq!(r.warnings[0]["warning"], "content.inline_deprecated");
    let big = vec![7u8; (1 << 20) + 1];
    let mut o = Order::new();
    o.content(r.created["d"], &big, "application/octet-stream", None);
    let e = env.admin.order(&o).await.unwrap_err();
    assert_eq!(e.error_id(), Some("content.inline_too_large"));
}

#[tokio::test]
async fn migrate_blobs_moves_database_content_to_files() {
    // content written by a server without a data directory stays in the database
    let plain = env_with(Store::connect(&db_url(), test_model(), 4).await.unwrap()).await;
    let mut o = Order::new();
    o.create("d", None, json!({"sample:Document": {"title": "Old"}}));
    o.content("d", b"old bytes in the database", "text/plain", None);
    let d = plain.admin.order(&o).await.unwrap().created["d"];
    let sha = sha_hex(b"old bytes in the database");
    let c = plain.store.client().await.unwrap();
    let n: i64 = c.query_one("SELECT count(*) FROM blob WHERE sha256=$1", &[&sha]).await.unwrap().get(0);
    assert_eq!(n, 1);
    // a server with a data directory still reads it from the database
    let dir = data_dir();
    let with = plain.store.clone().with_files(&dir).unwrap();
    let ctx = with.ctx(plain.init.tenant, plain.init.admin_account).await.unwrap();
    assert_eq!(with.get_content(&ctx, d).await.unwrap().0, b"old bytes in the database");
    // after the migration it reads the file, and the database holds no blob any more
    assert!(with.migrate_blobs().await.unwrap() >= 1);
    let n: i64 = c.query_one("SELECT count(*) FROM blob", &[]).await.unwrap().get(0);
    assert_eq!(n, 0);
    assert!(with.files().unwrap().has(&sha));
    assert_eq!(with.get_content(&ctx, d).await.unwrap().0, b"old bytes in the database");
}

#[tokio::test]
async fn upload_limits_hold_under_concurrent_requests() {
    let env = env().await;
    let (_, lea) = env.person("lea-many", &[]).await;
    // 120 opens at once: exactly the limit of open uploads succeeds
    let tasks: Vec<_> = (0..120).map(|_| { let c = lea.clone(); tokio::spawn(async move { c.upload_open(None).await }) }).collect();
    let mut ok = 0;
    let mut refused = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(e) => {
                assert_eq!(e.error_id(), Some("upload.too_many_open"));
                refused += 1;
            }
        }
    }
    assert_eq!((ok, refused), (100, 20));
}

#[tokio::test]
async fn idempotent_upload_open_counts_the_query() {
    let env = env().await;
    let key = Uuid::new_v4().to_string();
    let post = |q: &str| {
        reqwest::Client::new()
            .post(format!("{}/v1/uploads{q}", env.admin.base_url()))
            .bearer_auth(&env.init.token)
            .header("idempotency-key", &key)
            .send()
    };
    let a = post("").await.unwrap();
    assert_eq!(a.status(), 200);
    let b = post("").await.unwrap();
    assert_eq!(b.headers()["idempotent-replay"], "true");
    let c = post(&format!("?sha256={}", sha_hex(b"x"))).await.unwrap();
    assert_eq!(c.status(), 409);
}

#[tokio::test]
async fn upload_assignment_needs_rights_and_a_commit() {
    let env = env().await;
    let mut o = Order::new();
    o.create("d", None, json!({"sample:Document": {"title": "Theirs"}}));
    let d = env.admin.order(&o).await.unwrap().created["d"];
    let data = b"assigned only by a committed order that may write".to_vec();
    let sha = sha_hex(&data);

    // a dry run assigns nothing: no blob, the handle stays usable
    let id = env.admin.upload(&data).await.unwrap();
    let mut o = Order::new().dry_run();
    o.content_upload(d, &id, "text/plain", None);
    env.admin.order(&o).await.unwrap();
    assert!(!env.store.files().unwrap().has(&sha), "no blob after a dry run");

    // an account without write right on the object cannot use its upload there
    let (_, lea) = env.person("lea-norights", &[]).await;
    let theirs = lea.upload(b"lea's bytes").await.unwrap();
    let mut o = Order::new();
    o.content_upload(d, &theirs, "text/plain", None);
    assert_eq!(lea.order(&o).await.unwrap_err().error_id(), Some("rights.denied"));
    assert!(!env.store.files().unwrap().has(&sha_hex(b"lea's bytes")));

    // the real order commits and stores the blob
    let mut o = Order::new();
    o.content_upload(d, &id, "text/plain", None);
    env.admin.order(&o).await.unwrap();
    assert!(env.store.files().unwrap().has(&sha));
    assert_eq!(env.admin.get_content(d).await.unwrap(), data);

    // bad completions
    let up = env.admin.upload_open(Some(&sha_hex(b"a"))).await.unwrap();
    let uid = up["id"].as_str().unwrap();
    env.admin.upload_part(uid, 1, b"a".to_vec()).await.unwrap();
    assert_eq!(env.admin.upload_complete(uid, None, Some(i32::MAX)).await.unwrap_err().error_id(), Some("upload.bad_part"));
    assert_eq!(env.admin.upload_complete(uid, Some(&sha_hex(b"b")), None).await.unwrap_err().error_id(), Some("upload.checksum_conflict"));
    assert_eq!(env.admin.upload_complete(uid, None, Some(1)).await.unwrap()["state"], "complete");
}

/// Versions stay consistent when orders overlap: an order that began earlier but writes later must
/// not close a version that a later order already committed (no row with `valid_to < valid_from`,
/// exactly one open row per part).
#[tokio::test]
async fn overlapping_orders_keep_versions_consistent() {
    let env = env().await;
    let (id, _) = env.person("olga", &[]).await;
    let ctx = env.store.ctx(env.init.tenant, env.init.admin_account).await.unwrap();
    let audit = i3cm_store::session::Audit::default();
    let upd = |name: &str| -> i3cm_store::Op {
        serde_json::from_value(json!({"op": "update", "id": id.to_string(), "types": {"base:Person": {"lastName": name}}})).unwrap()
    };
    let attach: i3cm_store::Op = serde_json::from_value(json!({"op": "detach", "id": id.to_string(), "type": "base:PersonAccount"})).unwrap();
    // A begins first (smaller transaction number) ...
    let mut a = env.store.begin(&ctx, &audit).await.unwrap();
    // ... B begins later, writes and commits while A is still open
    let (store, ctx_b, upd_b) = (env.store.clone(), ctx.clone(), upd("from-b"));
    let b = tokio::spawn(async move {
        let mut ctx_b = ctx_b;
        let mut s = store.begin(&ctx_b, &i3cm_store::session::Audit::default()).await.unwrap();
        s.apply(&store, &mut ctx_b, &upd_b).await.unwrap();
        s.finish(&store, &mut ctx_b, true).await.unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    // ... then A writes the same object and commits
    let mut ctx_a = ctx.clone();
    a.apply(&env.store, &mut ctx_a, &upd("from-a")).await.unwrap();
    a.apply(&env.store, &mut ctx_a, &attach).await.unwrap();
    assert!(!b.is_finished(), "B waits for A's write turn");
    a.finish(&env.store, &mut ctx_a, true).await.unwrap();
    b.await.unwrap();
    // B wrote after A: its value is the current one
    let cur = env.admin.get_object(id, None).await.unwrap();
    assert_eq!(cur.attr("lastName"), Some(&json!("from-b")));
    let c = env.store.client().await.unwrap();
    for t in ["t_base_person", "object_state", "object_type"] {
        let broken: i64 = c
            .query_one(&format!("SELECT count(*) FROM {t} WHERE object_id=$1 AND valid_to IS NOT NULL AND valid_to < valid_from"), &[&id])
            .await
            .unwrap()
            .get(0);
        assert_eq!(broken, 0, "{t}: a version closed before it began");
    }
    let open: i64 = c.query_one("SELECT count(*) FROM t_base_person WHERE object_id=$1 AND valid_to IS NULL", &[&id]).await.unwrap().get(0);
    assert_eq!(open, 1, "exactly one open person row");
    let dup: i64 = c
        .query_one("SELECT count(*) FROM (SELECT type_name FROM object_type WHERE object_id=$1 AND valid_to IS NULL GROUP BY type_name HAVING count(*) > 1) d", &[&id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(dup, 0, "at most one open entry per type");
    // the version history is ordered: each closed row ends where a later one begins
    let rows = c.query("SELECT valid_from, valid_to FROM t_base_person WHERE object_id=$1 ORDER BY valid_from", &[&id]).await.unwrap();
    for w in rows.windows(2) {
        assert_eq!(w[0].get::<_, Option<i64>>(1), Some(w[1].get::<_, i64>(0)), "person versions form a chain");
    }
}

/// Each tenant counts its committed writing transactions without gaps, in commit order; refused
/// orders and dry runs leave no number behind.
#[tokio::test]
async fn tenant_sequence_is_gap_free_in_commit_order() {
    let env = env().await;
    let (id, _) = env.person("sam", &[]).await;
    let mut dry = Order::new();
    dry.update(id, json!({"base:Person": {"lastName": "dry"}}));
    dry.dry_run = true;
    env.admin.order(&dry).await.unwrap();
    let mut bad = Order::new();
    bad.update(id, json!({"base:Person": {"noSuchAttribute": 1}}));
    assert!(env.admin.order(&bad).await.is_err());
    let mut o = Order::new();
    o.update(id, json!({"base:Person": {"lastName": "sam-2"}}));
    let (r1, r2) = tokio::join!(env.admin.order(&o), env.admin.order(&o));
    r1.unwrap();
    r2.unwrap();
    let c = env.store.client().await.unwrap();
    let missing: i64 = c.query_one("SELECT count(*) FROM txn WHERE tenant_id=$1 AND seq IS NULL", &[&env.init.tenant]).await.unwrap().get(0);
    assert_eq!(missing, 0, "every writing transaction of a new tenant has a sequence number");
    let rows = c.query("SELECT seq, id FROM txn WHERE tenant_id=$1 ORDER BY seq", &[&env.init.tenant]).await.unwrap();
    assert!(rows.len() >= 3);
    for (i, r) in rows.iter().enumerate() {
        assert_eq!(r.get::<_, i64>(0), i as i64 + 1, "gap-free sequence");
    }
    for w in rows.windows(2) {
        assert!(w[0].get::<_, i64>(1) < w[1].get::<_, i64>(1), "transaction numbers follow the sequence");
    }
}

/// An open writing order of one tenant does not hold up writers of another tenant.
#[tokio::test]
async fn write_turns_are_per_tenant() {
    let env = env().await;
    let ctx = env.store.ctx(env.init.tenant, env.init.admin_account).await.unwrap();
    let held = env.store.begin(&ctx, &i3cm_store::session::Audit::default()).await.unwrap();
    let other = env.store.init_tenant(&format!("t-{}", Uuid::new_v4()), "admin").await.unwrap();
    let octx = env.store.ctx(other.tenant, other.admin_account).await.unwrap();
    let mut o = Order::new();
    o.create("g", None, json!({"base:Group": {"name": "elsewhere"}}));
    let mut octx2 = octx.clone();
    let order: i3cm_store::Order = serde_json::from_value(serde_json::to_value(&o).unwrap()).unwrap();
    let done = tokio::time::timeout(std::time::Duration::from_secs(5), env.store.execute(&mut octx2, &order)).await;
    assert!(matches!(done, Ok(Ok(_))), "another tenant writes while the first tenant's turn is held");
    held.abort().await;
}
