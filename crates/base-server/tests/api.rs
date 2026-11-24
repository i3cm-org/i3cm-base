//! Integration tests: real PostgreSQL (embedded), real HTTP server, Rust client.
//! The acceptance criteria of milestone 0.2 are covered here.

use base_client::{Client, ListQuery, Order};
use base_core::catalog::Catalog;
use base_core::Model;
use base_server::{serve, EmbeddedDb};
use base_store::tenant::TenantInit;
use base_store::Store;
use serde_json::json;
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

/// The shipped catalog plus a test module with a sub room and minus/block roles.
fn test_model() -> Model {
    let json = std::fs::read_to_string(base_server::default_catalog()).expect("run typecat first: catalog.json missing");
    let mut c: Catalog = serde_json::from_str(&json).unwrap();
    c.modules.push(
        serde_json::from_value(json!({
            "name": "t", "requires": ["base"], "rooms": ["tsub"], "i18n": {},
            "roles": [
                {"name": "t:no-person-sub", "rights": [{"sign": "-", "art": ["read"], "types": ["base:Person"], "room": "tsub"}]},
                {"name": "t:block-person", "rights": [{"sign": "block", "art": ["read"], "types": ["base:Person"], "room": "root"}]}
            ]
        }))
        .unwrap(),
    );
    Model::new(c).unwrap()
}

struct Env {
    store: Store,
    admin: Client,
    init: TenantInit,
    rooms: std::collections::BTreeMap<String, Uuid>,
}

async fn env() -> Env {
    let store = Store::connect(&db_url(), test_model(), 16).await.unwrap();
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
    let json = std::fs::read_to_string(base_server::default_catalog()).unwrap();
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
    // without a role Lea sees nothing else
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
    o.create("m", None, json!({"base:Mail": {"receivedAt": "2026-11-24T08:15:00+01:00", "sender": "anna@example.org", "subject": "Angebot"}}));
    for i in 0..2 {
        let a = format!("a{i}");
        o.create(&a, None, json!({"base:MailAttachment": {"filename": format!("file{i}.pdf")}}));
        o.link("base:MailAttachment.mail", a.as_str(), "m");
    }
    let r = env.admin.order(&o).await.unwrap();
    let m = r.created["m"];
    env.admin.put_content(m, b"From: anna@example.org\r\n\r\nHallo".to_vec(), "message/rfc822", Some("mail.eml")).await.unwrap();
    assert_eq!(env.admin.ends(m, "attachments", 10, 0).await.unwrap().len(), 2);
    assert!(env.admin.get_content(m).await.unwrap().ends_with(b"Hallo"));
    env.admin.put_text(m, "base:mail.md", "Hallo").await.unwrap();
    assert_eq!(env.admin.get_text(m, "base:mail.md").await.unwrap(), "Hallo");
    let view = env.admin.get_object(m, None).await.unwrap();
    assert_eq!(view.texts, vec!["base:mail.md".to_string()]);
    assert_eq!(view.display_name("base:Mail"), Some("anna@example.org - Angebot"));
    let en = env.admin.clone().with_lang("en").get_object(m, None).await.unwrap();
    let de = env.admin.clone().with_lang("de-DE").get_object(m, None).await.unwrap();
    assert!(en.types[0].display_description.starts_with("Mail received"));
    assert!(de.types[0].display_description.starts_with("Mail vom"));
    assert_eq!(env.admin.put_text(m, "base:body.html", "<p>x</p>").await.unwrap() > 0, true);
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
        o.create(&m, None, json!({"base:Mail": {"receivedAt": at, "sender": sender, "subject": subject}}));
        if i == 0 {
            for j in 0..2 {
                let a = format!("a{j}");
                o.create(&a, None, json!({"base:MailAttachment": {"filename": format!("file{j}.pdf")}}));
                o.link("base:MailAttachment.mail", a.as_str(), m.as_str());
            }
        }
    }
    let r = env.admin.order(&o).await.unwrap();
    let m0 = r.created["m0"];
    env.admin.put_content(m0, b"From: anna\r\n\r\nHallo".to_vec(), "message/rfc822", None).await.unwrap();
    env.admin.put_content(r.created["a0"], b"%PDF-1".to_vec(), "application/pdf", Some("file0.pdf")).await.unwrap();
    env.admin.put_text(m0, "base:mail.md", "# Hallo").await.unwrap();

    let de = env.admin.clone().with_lang("de");
    let days = de.aspect_children("base:mailbox", "").await.unwrap();
    assert_eq!(days.iter().map(|d| (d.display_name.as_str(), d.count)).collect::<Vec<_>>(), vec![("2026-11-24", Some(2)), ("2026-11-25", Some(1))]);
    let mails = de.aspect_children("base:mailbox", &days[0].node).await.unwrap();
    assert_eq!(mails.iter().map(|m| m.display_name.as_str()).collect::<Vec<_>>(), vec!["anna@example.org - Angebot", "ben@example.org - Rechnung"]);
    let inside = de.aspect_children("base:mailbox", &mails[0].node).await.unwrap();
    let names: Vec<(&str, &str)> = inside.iter().map(|n| (n.kind.as_str(), n.display_name.as_str())).collect();
    assert_eq!(names, vec![("file", "mail.eml"), ("file", "mail.md"), ("folder", "Anhänge")]);
    let atts = de.aspect_children("base:mailbox", &inside[2].node).await.unwrap();
    assert_eq!(atts.len(), 2);
    let att_files = de.aspect_children("base:mailbox", &atts.iter().find(|a| a.display_name == "file0.pdf").unwrap().node).await.unwrap();
    assert_eq!(att_files[0].display_name, "file0.pdf");
    assert_eq!(de.aspect_file("base:mailbox", &mails[0].node, "mail.md").await.unwrap(), b"# Hallo");
    assert!(de.aspect_file("base:mailbox", &mails[0].node, "mail.eml").await.unwrap().ends_with(b"Hallo"));

    let detail = de.aspect_node("base:mailbox", &mails[0].node).await.unwrap();
    assert_eq!(detail["node"]["displayDescription"].as_str().unwrap(), "Eingegangen am 2026-11-24T07:15:00+00:00", "aspect text overrides the global one");
    let labels: Vec<&str> = detail["attributes"].as_array().unwrap().iter().map(|a| a["label"].as_str().unwrap()).collect();
    assert_eq!(labels, vec!["Absender", "Betreff", "Empfangen", "Anzahl Anhänge"]);
    assert_eq!(detail["attributes"][3]["values"], json!([2]));
    assert_eq!(detail["tabs"][0]["label"], json!("Anhänge"));
    assert_eq!(detail["tabs"][0]["columns"], json!(["Datei"]));
    assert_eq!(detail["tabs"][0]["rows"].as_array().unwrap().len(), 2);
    assert_eq!(detail["functions"][0]["label"], json!("Antworten"));

    assert_eq!(env.admin.eval(m0, "attachments.filename").await.unwrap(), vec![json!("file0.pdf"), json!("file1.pdf")]);
    assert_eq!(env.admin.eval(m0, "attachments.where(filename = 'file1.pdf').count()").await.unwrap(), vec![json!(1)]);
    assert_eq!(env.admin.eval(m0, "_createdBy = %x").await.unwrap_err().error_id(), Some("path.syntax"));
    assert_eq!(env.admin.eval(m0, "_createdBy._id").await.unwrap(), vec![json!(env.init.admin_account.to_string())]);

    // without read rights the mailbox is empty
    let (_, lea) = env.person("lea", &[]).await;
    assert!(lea.aspect_children("base:mailbox", "").await.unwrap().is_empty());
}
