//! i3cm-core: catalog model, type checker, value types, rights evaluation and display
//! names. No I/O.

pub mod catalog;
pub mod checker;
pub mod error;
pub mod fhirpath;
pub mod i18n;
pub mod paths;
pub mod rights;
pub mod values;

pub use catalog::{Art, Catalog, Model, Sign, ANY};
pub use error::{BaseError, ErrorKind, Result};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> serde_json::Value {
        json!({
          "format": 1,
          "modules": [{"name": "m", "requires": [], "rooms": [], "profiles": [], "i18n": {"en": "m-A-name = A { $x }\n"}}],
          "types": [
            {"name":"m:A","ns":"m","local":"A","abstract":false,"extends":null,"attributes":[{"name":"x","vt":{"kind":"string"},"required":true,"list":false,"indexed":false,"unique":false}],
             "requires":{"mode":"any","types":[]},"imprintsOn":[],"brings":["m:B"],"content":null,"textTypes":[]},
            {"name":"m:B","ns":"m","local":"B","abstract":false,"extends":null,"attributes":[],
             "requires":{"mode":"any","types":[]},"imprintsOn":[],"brings":[],"content":null,"textTypes":[]},
            {"name":"m:C","ns":"m","local":"C","abstract":false,"extends":"m:B","attributes":[],
             "requires":{"mode":"any","types":[]},"imprintsOn":["m:A"],"brings":[],"content":null,"textTypes":[]}
          ],
          "references": [],
          "textTypes": []
        })
    }

    #[test]
    fn loads_and_closes_implications() {
        let m = Model::new(serde_json::from_value(catalog()).unwrap()).unwrap();
        let set = m.implied_closure(&["m:A".to_string()].into());
        assert_eq!(set, ["m:A", "m:B", "m:C"].iter().map(|s| s.to_string()).collect());
        assert!(m.is_a("m:C", "m:B"));
        assert!(!m.is_a("m:B", "m:C"));
        let i = i18n::I18n::new(&m).unwrap();
        let attrs = json!({"x": "one"}).as_object().unwrap().clone();
        assert_eq!(i.names("en", "m:A", &attrs).0, "A one");
        assert_eq!(i.pick(Some("de-DE,de;q=0.9")), "en");
    }

    #[test]
    fn rejects_implication_cycle() {
        let mut c = catalog();
        c["types"][1]["brings"] = json!(["m:A"]);
        let e = Model::new(serde_json::from_value(c).unwrap()).unwrap_err();
        assert_eq!(e.error_id, "type.implication_cycle");
    }

    #[test]
    fn rejects_unknown_supertype_and_reserved_names() {
        let mut c = catalog();
        c["types"][2]["extends"] = json!("m:Nope");
        assert_eq!(Model::new(serde_json::from_value(c).unwrap()).unwrap_err().error_id, "type.unknown_supertype");
        let mut c = catalog();
        c["types"][0]["attributes"][0]["name"] = json!("object_id");
        assert_eq!(Model::new(serde_json::from_value(c).unwrap()).unwrap_err().error_id, "type.reserved_attribute");
    }

    #[test]
    fn aspects_are_checked() {
        let mut c = catalog();
        c["modules"][0]["aspects"] = json!([{"name": "m:v", "entry": {"type": "m:A", "groupBy": ["x.substring(0, 1)"]},
            "nodes": {"m:A": {"attributes": [{"label": "x", "path": "x.upper()"}]}}}]);
        assert!(Model::new(serde_json::from_value(c.clone()).unwrap()).is_ok());
        let mut bad = c.clone();
        bad["modules"][0]["aspects"][0]["nodes"]["m:A"]["attributes"][0]["path"] = json!("nope.x");
        assert_eq!(Model::new(serde_json::from_value(bad).unwrap()).unwrap_err().error_id, "aspect.unknown_name");
        let mut bad = c.clone();
        bad["modules"][0]["aspects"][0]["entry"]["type"] = json!("m:Z");
        assert_eq!(Model::new(serde_json::from_value(bad).unwrap()).unwrap_err().error_id, "aspect.unknown_type");
        let mut bad = c.clone();
        bad["modules"][0]["aspects"][0]["nodes"]["m:A"]["attributes"][0]["path"] = json!("x.repeat(y)");
        assert_eq!(Model::new(serde_json::from_value(bad).unwrap()).unwrap_err().error_id, "path.function_not_allowed");
        let mut bad = c.clone();
        bad["modules"][0]["aspects"][0]["entry"]["groupBy"] = json!(["x.count()"]);
        assert_eq!(Model::new(serde_json::from_value(bad).unwrap()).unwrap_err().error_id, "aspect.bad_group");
        let mut ok = c.clone();
        ok["modules"][0]["aspects"][0]["entry"]["groupBy"] = json!(["x.toString().substring(0, 2).upper()"]);
        assert!(Model::new(serde_json::from_value(ok).unwrap()).is_ok());
        let mut bad = c;
        bad["modules"][0]["aspects"][0]["nodes"]["m:A"]["files"] = json!([{"name": "f", "path": "_content"}]);
        assert_eq!(Model::new(serde_json::from_value(bad).unwrap()).unwrap_err().error_id, "aspect.bad_file");
    }

    #[test]
    fn shipped_catalog_is_valid() {
        // catalog.json is produced by typecat in CI before the tests run; skip if absent.
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../catalog.json");
        if let Ok(s) = std::fs::read_to_string(p) {
            let m = Model::from_json(&s).unwrap();
            assert!(m.type_def("base:PersonAccount").is_some());
            assert!(m.is_a("base:PersonAccount", "base:Carrier"));
            // the kernel carries no business types since 0.6 (DEC-138): they live in modules
            assert!(m.type_def("base:Mail").is_none() && m.type_def("base:Task").is_none());
        }
    }
}
