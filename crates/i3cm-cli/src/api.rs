//! Blocking client for the parts of the API the program uses.

use crate::config::Config;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

pub struct Api {
    http: reqwest::blocking::Client,
    base: String,
    token: String,
    lang: String,
}

/// An entry of `GET /v1/aspects/{a}/children`.
#[derive(Debug, Clone, Deserialize)]
pub struct Child {
    pub node: String,
    pub file: bool,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub name: String,
    pub id: Option<String>,
    pub size: Option<u64>,
    #[serde(rename = "changedAt")]
    pub changed_at: Option<String>,
    pub ino: Option<u64>,
}

impl Api {
    pub fn new(c: &Config) -> Api {
        let http = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(120)).build().expect("http client");
        Api { http, base: c.server.trim_end_matches('/').to_string(), token: c.token.clone(), lang: c.lang.clone() }
    }

    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        self.http.request(m, format!("{}/v1{path}", self.base)).bearer_auth(&self.token).header(reqwest::header::ACCEPT_LANGUAGE, &self.lang)
    }

    fn send(rb: reqwest::blocking::RequestBuilder) -> Result<reqwest::blocking::Response, String> {
        let r = rb.send().map_err(|e| format!("server not reachable: {e}"))?;
        if r.status().is_success() {
            return Ok(r);
        }
        let status = r.status().as_u16();
        let v: Value = r.json().unwrap_or(Value::Null);
        let mut msg = format!("{} ({status} {})", v["description"].as_str().unwrap_or("error"), v["error_id"].as_str().unwrap_or(""));
        if let Some(s) = v["params"]["supported"].as_array() {
            msg = format!("{} (supported: {})", v["description"].as_str().unwrap_or(""), s.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "));
        }
        Err(msg)
    }

    pub fn get_json(&self, path: &str, q: &[(&str, &str)]) -> Result<Value, String> {
        Self::send(self.req(reqwest::Method::GET, path).query(q))?.json().map_err(|e| e.to_string())
    }

    pub fn post_json(&self, path: &str, body: &Value) -> Result<Value, String> {
        Self::send(self.req(reqwest::Method::POST, path).json(body))?.json().map_err(|e| e.to_string())
    }

    pub fn get_bytes(&self, path: &str, q: &[(&str, &str)]) -> Result<Vec<u8>, String> {
        Ok(Self::send(self.req(reqwest::Method::GET, path).query(q))?.bytes().map_err(|e| e.to_string())?.to_vec())
    }

    pub fn me(&self) -> Result<Value, String> {
        self.get_json("/me", &[])
    }

    pub fn aspects(&self) -> Result<Vec<Value>, String> {
        Ok(self.get_json("/aspects", &[])?.as_array().cloned().unwrap_or_default())
    }

    /// The complete listing of a node (paged internally).
    pub fn children(&self, aspect: &str, node: &str) -> Result<Vec<Child>, String> {
        let mut all = vec![];
        let mut offset = 0;
        loop {
            let o = offset.to_string();
            let v = self.get_json(&format!("/aspects/{aspect}/children"), &[("node", node), ("offset", &o), ("limit", "1000")])?;
            let page: Vec<Child> = serde_json::from_value(v).map_err(|e| e.to_string())?;
            let n = page.len();
            all.extend(page);
            if n < 1000 {
                return Ok(all);
            }
            offset += 1000;
        }
    }

    pub fn entry(&self, aspect: &str, node: &str, name: &str) -> Result<Child, String> {
        serde_json::from_value(self.get_json(&format!("/aspects/{aspect}/entry"), &[("node", node), ("name", name)])?).map_err(|e| e.to_string())
    }

    pub fn file(&self, aspect: &str, node: &str, name: &str) -> Result<Vec<u8>, String> {
        self.get_bytes(&format!("/aspects/{aspect}/file"), &[("node", node), ("name", name)])
    }

    pub fn index(&self, aspect: &str, node: &str, dir: &str) -> Result<Vec<u8>, String> {
        self.get_bytes(&format!("/aspects/{aspect}/index"), &[("node", node), ("name", dir)])
    }

    /// Several files at once: items `{"node", "name"}` or `{"node", "index": true, "name"}`.
    pub fn files(&self, aspect: &str, items: &[Value]) -> Result<Vec<Option<Vec<u8>>>, String> {
        let v = self.post_json(&format!("/aspects/{aspect}/files"), &json!({"items": items}))?;
        Ok(v.as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|x| x["data"].as_str().and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok()))
            .collect())
    }

    pub fn tool(&self, call: &i3cm_tools::Call) -> Result<i3cm_tools::Response, String> {
        let v = self.post_json(&format!("/tools/{}", call.tool.name()), &serde_json::to_value(&call.req).unwrap_or_default())?;
        serde_json::from_value(v).map_err(|e| e.to_string())
    }
}
