//! Example module code for i3cm: one component, operations dispatched by name.

wit_bindgen::generate!({ path: "../wit", world: "module" });

use serde_json::{json, Value};

struct Example;

fn p(params: &str) -> Value {
    serde_json::from_str(params).unwrap_or(Value::Null)
}

fn note(conn: &Connection, title: &str) -> Result<String, String> {
    let ops = json!([{ "op": "create", "ref": "n", "types": { "example:Note": { "title": title, "author": conn.me() } } }]);
    let created: Value = serde_json::from_str(&conn.order(&ops.to_string())?).map_err(|e| e.to_string())?;
    Ok(created["n"].as_str().unwrap_or_default().to_string())
}

impl Guest for Example {
    fn run(conn: &Connection, op: String, params: String) -> Result<String, String> {
        let args = p(&params);
        match op.as_str() {
            "greet" => Ok(json!({ "hello": args["name"].as_str().unwrap_or("world"), "me": conn.me() }).to_string()),
            "addNote" => {
                let id = note(conn, args["title"].as_str().unwrap_or("untitled"))?;
                let back: Value = serde_json::from_str(&conn.get(&id)?).map_err(|e| e.to_string())?;
                Ok(json!({ "id": id, "readBack": back["id"] }).to_string())
            }
            "stamp" => {
                let sys = conn.impersonate("example:system")?;
                let id = note(&sys, args["title"].as_str().unwrap_or("stamped"))?;
                Ok(json!({ "id": id, "as": sys.me(), "caller": conn.me() }).to_string())
            }
            "stampDenied" => {
                let sys = conn.impersonate("example:system")?;
                Ok(json!({ "as": sys.me() }).to_string())
            }
            "callGreet" => conn.call("example:greet", &json!({ "name": "nested" }).to_string()),
            "checkNote" => {
                let title = args["op"]["types"]["example:Note"]["title"].as_str().unwrap_or("");
                if title.contains("forbidden") {
                    return Err(json!({ "error_id": "example.forbidden_title", "description": "titles must not contain 'forbidden'", "params": { "title": title } }).to_string());
                }
                // hooks get read-only handles: writing must fail
                if title.contains("hookwrite") {
                    return match conn.order("[]") {
                        Ok(_) => Err("hook could write".into()),
                        Err(e) => Err(e),
                    };
                }
                Ok("{}".into())
            }
            "tick" => {
                let id = note(conn, &format!("tick {}", args["scheduled"].as_str().unwrap_or("")))?;
                Ok(json!({ "id": id }).to_string())
            }
            "crash" => {
                note(conn, "written before the crash")?;
                panic!("deliberate crash");
            }
            // after-update hook: when a note becomes done, stamp it and write a log note, in the
            // same order; titles with "loop" write on every update (ends at the depth limit),
            // titles with "fail" make the hook fail (the whole order is rolled back)
            "onUpdate" => {
                let id = args["id"].as_str().unwrap_or_default().to_string();
                let note: Value = serde_json::from_str(&conn.get(&id)?).map_err(|e| e.to_string())?;
                let part = note["parts"].as_array().and_then(|ps| ps.iter().find(|p| p["type"] == "example:Note")).cloned().unwrap_or(Value::Null);
                let attrs = &part["attributes"];
                let title = attrs["title"].as_str().unwrap_or("");
                if title.contains("fail") {
                    return Err(json!({ "error_id": "example.after_failed", "description": "after hook failed", "params": {} }).to_string());
                }
                if title.contains("loop") {
                    let ops = json!([{ "op": "update", "id": id, "types": { "example:Note": { "author": format!("{}x", attrs["author"].as_str().unwrap_or("")) } } }]);
                    conn.order(&ops.to_string())?;
                    return Ok("{}".into());
                }
                if attrs["status"] == "done" && attrs["doneAt"].is_null() {
                    let ops = json!([
                        { "op": "update", "id": id, "types": { "example:Note": { "doneAt": "2026-01-01T00:00:00Z" } } },
                        { "op": "create", "ref": "log", "types": { "example:Note": { "title": format!("log: {title} done"), "author": conn.me() } } }
                    ]);
                    conn.order(&ops.to_string())?;
                }
                Ok("{}".into())
            }
            // all open notes due before a time, read page by page through the filter expression
            "findOpen" => {
                let mut after = Value::Null;
                let mut ids = vec![];
                let mut pages = 0;
                loop {
                    let f = json!({ "where": args["where"].as_str().unwrap_or("status = 'open'"), "after": after, "limit": args["limit"].as_i64().unwrap_or(100) });
                    let page: Value = serde_json::from_str(&conn.query("example:Note", &f.to_string())?).map_err(|e| e.to_string())?;
                    pages += 1;
                    for o in page["items"].as_array().cloned().unwrap_or_default() {
                        ids.push(o["id"].clone());
                    }
                    if page["next"].is_null() {
                        break;
                    }
                    after = page["next"].clone();
                }
                Ok(json!({ "count": ids.len(), "pages": pages, "ids": ids }).to_string())
            }
            // the reference ends of a note, from get and from ends
            "refs" => {
                let id = args["id"].as_str().unwrap_or_default();
                let v: Value = serde_json::from_str(&conn.get(id)?).map_err(|e| e.to_string())?;
                let children: Value = serde_json::from_str(&conn.ends(id, "children", 0, 500)?).map_err(|e| e.to_string())?;
                Ok(json!({ "references": v["references"], "children": children }).to_string())
            }
            // filter hooks: a note entered or left `@done`; content and text hooks of documents;
            // each writes a log note naming what happened
            "onEnter" | "onLeave" | "onContent" | "onSummary" => {
                let what = match op.as_str() { "onEnter" => "enter", "onLeave" => "leave", "onContent" => "content", _ => "summary" };
                // hooks write into the order of their trigger: local names must be unique there
                let id = args["id"].as_str().unwrap_or_default();
                let ops = json!([{ "op": "create", "ref": format!("{what}-{id}"), "types": { "example:Note": { "title": format!("{what} {id}"), "author": conn.me() } } }]);
                conn.order(&ops.to_string())?;
                Ok("{}".into())
            }
            "spin" => {
                #[allow(clippy::empty_loop)]
                loop {}
            }
            other => Err(format!("unknown operation {other}")),
        }
    }
}

export!(Example);
