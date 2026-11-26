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
            "spin" => {
                #[allow(clippy::empty_loop)]
                loop {}
            }
            other => Err(format!("unknown operation {other}")),
        }
    }
}

export!(Example);
