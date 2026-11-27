//! `i3cm scaffold`: frames for the IDE in a module repository. Nothing is overwritten unless
//! `--force` is given; the files are written below the repository root found by `i3cm/module.ts`.
//!
//! * `ops --lang rust`: a WebAssembly component (`wasm32-wasip2`) against the kernel's WIT world
//!   `i3cm:module`, with the WIT file, a dispatcher by operation name and a unit test.
//! * `ui`: `i3cm/ui/<module>.html` with one template per aspect and node type found in
//!   `i3cm/aspects/`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The kernel's WIT world, shipped with the program so scaffolds match it.
pub const MODULE_WIT: &str = include_str!("../../../wit/module.wit");

/// Languages `scaffold ops` knows.
pub const LANGUAGES: &[&str] = &["rust"];

/// Files of a scaffold: relative path and content.
pub type Files = Vec<(String, String)>;

/// Crate name for a module name: `[a-z0-9-]`, prefixed with `i3cm-`.
pub fn crate_name(module: &str) -> String {
    let s: String = module.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    format!("i3cm-{}", s.trim_matches('-'))
}

pub fn ops_rust(module: &str) -> Files {
    let name = crate_name(module);
    let cargo = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
license = "Apache-2.0"
publish = false

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
wit-bindgen = "0.63"
serde_json = "1"

[profile.release]
opt-level = "s"
lto = true
strip = true

[workspace]
"#
    );
    let lib = format!(
        r#"//! Operations and hooks of the module `{module}`: one WebAssembly component, operations
//! dispatched by name. Declare them in `i3cm/module.ts` (`operations`, `hooks`, `schedules`).

wit_bindgen::generate!({{ path: "wit", world: "module" }});

use serde_json::{{json, Value}};

struct Module;

/// Parameters as JSON (`null` if they do not parse).
pub fn params(text: &str) -> Value {{
    serde_json::from_str(text).unwrap_or(Value::Null)
}}

/// An error the kernel shows as `{{error_id, description, params}}`.
pub fn error(id: &str, description: &str, params: Value) -> String {{
    json!({{ "error_id": id, "description": description, "params": params }}).to_string()
}}

impl Guest for Module {{
    fn run(conn: &Connection, op: String, params_text: String) -> Result<String, String> {{
        let p = params(&params_text);
        match op.as_str() {{
            "hello" => Ok(json!({{ "hello": p["name"].as_str().unwrap_or("world"), "me": conn.me() }}).to_string()),
            other => Err(error("{module}.unknown_operation", "unknown operation", json!({{ "op": other }}))),
        }}
    }}
}}

export!(Module);

#[cfg(test)]
mod tests {{
    use super::*;

    #[test]
    fn errors_are_json_objects() {{
        let e: Value = serde_json::from_str(&error("x.y", "d", json!({{ "a": 1 }}))).unwrap();
        assert_eq!(e["error_id"], "x.y");
        assert_eq!(params("{{\"name\": \"n\"}}")["name"], "n");
        assert!(params("not json").is_null());
    }}
}}
"#
    );
    let readme = format!(
        r#"# Module code of `{module}`

Build the component and run it with a local kernel:

```sh
rustup target add wasm32-wasip2          # once
cargo test                                # unit tests on the host
cargo build --release --target wasm32-wasip2
mkdir -p ../modules && cp target/wasm32-wasip2/release/{lib}.wasm ../modules/{module}.wasm
```

`wit/module.wit` is the kernel's interface; take a newer copy from the kernel when it changes
(`i3cm scaffold ops --lang rust --force` rewrites the scaffold, so commit first).
"#,
        lib = name.replace('-', "_")
    );
    vec![
        ("ops/Cargo.toml".into(), cargo),
        ("ops/src/lib.rs".into(), lib),
        ("ops/wit/module.wit".into(), MODULE_WIT.to_string()),
        ("ops/README.md".into(), readme),
        ("ops/.gitignore".into(), "/target\n".into()),
    ]
}

/// Aspect names and the node types of each, read from the sources of `i3cm/aspects/`:
/// `aspect("name", …)` and quoted keys `"ns:Type":` after it.
pub fn aspects_in(source: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = vec![];
    let mut rest = source;
    while let Some(i) = rest.find("aspect(") {
        rest = &rest[i + 7..];
        let s = rest.trim_start();
        let Some(q) = s.chars().next().filter(|c| matches!(c, '"' | '\'' | '`')) else { continue };
        let Some(end) = s[1..].find(q) else { continue };
        let name = s[1..1 + end].to_string();
        let body_end = rest.find("aspect(").unwrap_or(rest.len());
        let body = &rest[..body_end];
        let mut types = BTreeSet::new();
        let mut b = body;
        while let Some(j) = b.find('"') {
            let after = &b[j + 1..];
            let Some(k) = after.find('"') else { break };
            let key = &after[..k];
            let tail = after[k + 1..].trim_start();
            if tail.starts_with(':') && key.contains(':') && !key.contains(' ') && key.split(':').nth(1).is_some_and(|t| t.starts_with(|c: char| c.is_ascii_uppercase())) {
                types.insert(key.to_string());
            }
            b = &after[k + 1..];
        }
        out.push((name, types.into_iter().collect()));
    }
    out
}

pub fn ui(module: &str, aspects: &[(String, Vec<String>)]) -> Files {
    let mut html = String::new();
    for (aspect, types) in aspects {
        for t in types {
            let class = t.replace(':', "-").to_lowercase();
            html.push_str(&format!(
                "<template data-aspect=\"{aspect}\" data-type=\"{t}\">\n  <div class=\"{class}\">\n    <!-- <span data-label=\"key\"></span>: <span data-attr=\"key\"></span> for an attribute of the aspect -->\n    <div data-field=\"displayName\"></div>\n  </div>\n</template>\n"
            ));
        }
    }
    if html.is_empty() {
        html.push_str("<!-- one <template data-aspect=\"…\" data-type=\"…\"> per node type of an aspect; see the kernel's docs/developer/ui.md -->\n");
    }
    vec![(format!("i3cm/ui/{module}.html"), html)]
}

/// Write files below `root`; refuses existing files unless `force`. Returns the written paths.
pub fn write(root: &Path, files: &Files, force: bool) -> Result<Vec<PathBuf>, String> {
    if !force {
        let existing: Vec<String> = files.iter().filter(|(p, _)| root.join(p).exists()).map(|(p, _)| p.clone()).collect();
        if !existing.is_empty() {
            return Err(format!("exists already (use --force to overwrite): {}", existing.join(", ")));
        }
    }
    let mut out = vec![];
    for (p, content) in files {
        let target = root.join(p);
        if let Some(d) = target.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        std::fs::write(&target, content).map_err(|e| format!("{}: {e}", target.display()))?;
        out.push(target);
    }
    Ok(out)
}

/// Sources of all `.ts` files below `i3cm/aspects/`.
pub fn aspect_sources(repo: &Path) -> String {
    let mut all = String::new();
    let mut stack = vec![repo.join("i3cm").join("aspects")];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "ts") {
                all.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
                all.push('\n');
            }
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_names() {
        assert_eq!(crate_name("studio"), "i3cm-studio");
        assert_eq!(crate_name("Mein_Verein"), "i3cm-mein-verein");
    }

    #[test]
    fn rust_scaffold_uses_the_kernel_wit() {
        let files = ops_rust("demo");
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["ops/Cargo.toml", "ops/src/lib.rs", "ops/wit/module.wit", "ops/README.md", "ops/.gitignore"]);
        assert!(files[0].1.contains("name = \"i3cm-demo\""));
        assert!(files[1].1.contains("world: \"module\"") && files[1].1.contains("demo.unknown_operation"));
        assert!(files[2].1.contains("world module"));
        assert!(files[3].1.contains("i3cm_demo.wasm") && files[3].1.contains("../modules/demo.wasm"));
    }

    #[test]
    fn aspects_and_node_types_from_sources() {
        let src = r#"
import { aspect } from "@i3cm/lib";
export const loans = aspect("loans", {
  entry: { type: "lend:Loan", groupBy: ["due"] },
  nodes: {
    "lend:Loan": { attributes: { due: "due" }, subNodes: { items: { path: "item" } } },
    "lend:Item": { attributes: { title: "title" } },
  },
});
export const people = aspect('people', { entry: { type: "base:Person" }, nodes: { "base:Person": {} } });
"#;
        assert_eq!(aspects_in(src), vec![
            ("loans".to_string(), vec!["lend:Item".to_string(), "lend:Loan".to_string()]),
            ("people".to_string(), vec!["base:Person".to_string()]),
        ]);
    }

    #[test]
    fn ui_templates_per_aspect_and_type() {
        let files = ui("lend", &[("loans".into(), vec!["lend:Loan".into()])]);
        assert_eq!(files[0].0, "i3cm/ui/lend.html");
        assert!(files[0].1.contains("<template data-aspect=\"loans\" data-type=\"lend:Loan\">"));
        assert!(ui("lend", &[])[0].1.starts_with("<!--"));
    }

    #[test]
    fn write_refuses_existing_files() {
        let root = std::env::temp_dir().join(format!("i3cm-scaffold-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let files: Files = vec![("a/b.txt".into(), "1".into())];
        assert_eq!(write(&root, &files, false).unwrap().len(), 1);
        assert!(write(&root, &files, false).unwrap_err().contains("a/b.txt"));
        write(&root, &vec![("a/b.txt".into(), "2".into())], true).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a/b.txt")).unwrap(), "2");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
