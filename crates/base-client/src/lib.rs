//! base-client: Rust client for the base-erp REST API.
//!
//! ```no_run
//! # async fn demo() -> Result<(), base_client::ClientError> {
//! use base_client::{Client, Order};
//! use serde_json::json;
//! let c = Client::new("http://localhost:8480", "bt_...");
//! let rooms = c.rooms().await?;
//! let mut o = Order::new();
//! o.create("p", Some(rooms["root"]), json!({"base:Person": {"lastName": "Muster"}}));
//! let res = c.order(&o).await?;
//! let person = c.get_object(res.created["p"], None).await?;
//! # Ok(()) }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{status} {error_id}: {description} {params}")]
    Api { status: u16, error_id: String, description: String, params: Value },
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}

impl ClientError {
    pub fn error_id(&self) -> Option<&str> {
        match self {
            ClientError::Api { error_id, .. } => Some(error_id),
            _ => None,
        }
    }
    pub fn status(&self) -> Option<u16> {
        match self {
            ClientError::Api { status, .. } => Some(*status),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// An order: operations executed together in one transaction.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Order {
    pub dry_run: bool,
    pub ops: Vec<Value>,
}

/// Turn an object id or local reference into the string form used in orders.
pub trait IdRef {
    fn id_ref(&self) -> String;
}
impl IdRef for Uuid {
    fn id_ref(&self) -> String {
        self.to_string()
    }
}
impl IdRef for &str {
    fn id_ref(&self) -> String {
        self.to_string()
    }
}
impl IdRef for String {
    fn id_ref(&self) -> String {
        self.clone()
    }
}

impl Order {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }
    /// `types`: `{"ns:Type": {attributes...}, ...}`
    pub fn create(&mut self, local: &str, room: Option<Uuid>, types: Value) -> &mut Self {
        self.ops.push(json!({"op": "create", "ref": local, "room": room.map(|r| r.to_string()), "types": types}));
        self
    }
    pub fn update(&mut self, id: impl IdRef, types: Value) -> &mut Self {
        self.ops.push(json!({"op": "update", "id": id.id_ref(), "types": types}));
        self
    }
    pub fn attach(&mut self, id: impl IdRef, type_name: &str, values: Value) -> &mut Self {
        self.ops.push(json!({"op": "attach", "id": id.id_ref(), "type": type_name, "values": values}));
        self
    }
    pub fn detach(&mut self, id: impl IdRef, type_name: &str) -> &mut Self {
        self.ops.push(json!({"op": "detach", "id": id.id_ref(), "type": type_name}));
        self
    }
    pub fn delete(&mut self, id: impl IdRef) -> &mut Self {
        self.ops.push(json!({"op": "delete", "id": id.id_ref()}));
        self
    }
    pub fn move_to(&mut self, id: impl IdRef, room: impl IdRef) -> &mut Self {
        self.ops.push(json!({"op": "move", "id": id.id_ref(), "room": room.id_ref()}));
        self
    }
    pub fn link(&mut self, reference: &str, from: impl IdRef, to: impl IdRef) -> &mut Self {
        self.ops.push(json!({"op": "link", "reference": reference, "from": from.id_ref(), "to": to.id_ref()}));
        self
    }
    pub fn unlink(&mut self, reference: &str, link: Uuid) -> &mut Self {
        self.ops.push(json!({"op": "unlink", "reference": reference, "id": link}));
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderResult {
    pub txn: i64,
    pub dry_run: bool,
    pub created: BTreeMap<String, Uuid>,
    pub links: Vec<Uuid>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Shown {
    #[serde(rename = "type")]
    pub type_name: String,
    pub implied: bool,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "displayDescription")]
    pub display_description: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Part {
    #[serde(rename = "type")]
    pub type_name: String,
    pub attributes: Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ObjectView {
    pub id: Uuid,
    pub room: Uuid,
    pub deleted: bool,
    pub created_by: Option<Uuid>,
    pub created_at: String,
    pub types: Vec<Shown>,
    pub parts: Vec<Part>,
    pub content: Option<Value>,
    pub texts: Vec<String>,
}

impl ObjectView {
    /// Attribute value from any readable part.
    pub fn attr(&self, name: &str) -> Option<&Value> {
        self.parts.iter().find_map(|p| p.attributes.get(name))
    }
    pub fn has_type(&self, t: &str) -> bool {
        self.types.iter().any(|x| x.type_name == t)
    }
    pub fn display_name(&self, t: &str) -> Option<&str> {
        self.types.iter().find(|x| x.type_name == t).map(|x| x.display_name.as_str())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Summary {
    pub id: Uuid,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub types: Vec<String>,
    pub link: Option<Uuid>,
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Version {
    pub version: i64,
    pub at: String,
    pub account: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GroupCount {
    pub key: String,
    pub count: i64,
}

/// Filter for listing objects of a type by one of its own attributes.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ListQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eq: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub desc: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
}

/// A node of an aspect tree.
#[derive(Debug, Clone, Deserialize)]
pub struct AspectNode {
    pub node: String,
    /// A file (bytes) instead of a node (index and children).
    pub file: bool,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "displayDescription", default)]
    pub display_description: String,
    pub count: Option<i64>,
    #[serde(rename = "type")]
    pub type_name: Option<String>,
    pub id: Option<Uuid>,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
    lang: Option<String>,
}

impl Client {
    pub fn new(base: &str, token: &str) -> Client {
        let http = reqwest::Client::builder().pool_max_idle_per_host(64).build().expect("http client");
        Client { http, base: base.trim_end_matches('/').to_string(), token: token.to_string(), lang: None }
    }

    /// Same server, other token (shares the connection pool).
    pub fn with_token(&self, token: &str) -> Client {
        Client { token: token.to_string(), ..self.clone() }
    }

    pub fn with_lang(mut self, lang: &str) -> Client {
        self.lang = Some(lang.to_string());
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}/v1{}", self.base, path)
    }

    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self.http.request(m, self.url(path)).bearer_auth(&self.token);
        if let Some(l) = &self.lang {
            r = r.header(reqwest::header::ACCEPT_LANGUAGE, l);
        }
        r
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        Err(ClientError::Api {
            status,
            error_id: body["error_id"].as_str().unwrap_or("unknown").to_string(),
            description: body["description"].as_str().unwrap_or("").to_string(),
            params: body["params"].clone(),
        })
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        Ok(Self::check(self.req(reqwest::Method::GET, path).send().await?).await?.json().await?)
    }

    async fn get_q<T: for<'de> Deserialize<'de>, Q: Serialize>(&self, path: &str, q: &Q) -> Result<T> {
        Ok(Self::check(self.req(reqwest::Method::GET, path).query(q).send().await?).await?.json().await?)
    }

    async fn post<T: for<'de> Deserialize<'de>>(&self, path: &str, body: &Value) -> Result<T> {
        Ok(Self::check(self.req(reqwest::Method::POST, path).json(body).send().await?).await?.json().await?)
    }

    pub async fn order(&self, o: &Order) -> Result<OrderResult> {
        self.post("/orders", &serde_json::to_value(o).unwrap()).await
    }

    pub async fn get_object(&self, id: Uuid, at: Option<i64>) -> Result<ObjectView> {
        match at {
            Some(v) => self.get_q(&format!("/objects/{id}"), &[("at", v)]).await,
            None => self.get(&format!("/objects/{id}")).await,
        }
    }

    pub async fn versions(&self, id: Uuid) -> Result<Vec<Version>> {
        self.get(&format!("/objects/{id}/versions")).await
    }

    pub async fn ends(&self, id: Uuid, end: &str, limit: i64, offset: i64) -> Result<Vec<Summary>> {
        self.get_q(&format!("/objects/{id}/ends/{end}"), &[("limit", limit), ("offset", offset)]).await
    }

    pub async fn list(&self, type_name: &str, q: &ListQuery) -> Result<Vec<Summary>> {
        self.get_q(&format!("/types/{type_name}/objects"), q).await
    }

    pub async fn group_counts(&self, type_name: &str, attr: &str, unit: &str, from: Option<&str>, to: Option<&str>) -> Result<Vec<GroupCount>> {
        let mut q = vec![("attr", attr.to_string()), ("unit", unit.to_string())];
        if let Some(f) = from {
            q.push(("from", f.to_string()));
        }
        if let Some(t) = to {
            q.push(("to", t.to_string()));
        }
        self.get_q(&format!("/types/{type_name}/groups"), &q).await
    }

    pub async fn me(&self) -> Result<ObjectView> {
        self.get("/me").await
    }
    pub async fn my_groups(&self) -> Result<Vec<Summary>> {
        self.get("/me/groups").await
    }
    pub async fn members(&self, group: Uuid, transitive: bool) -> Result<Vec<Summary>> {
        self.get_q(&format!("/groups/{group}/members"), &[("transitive", transitive)]).await
    }
    /// Module rooms by name, plus "root".
    pub async fn rooms(&self) -> Result<BTreeMap<String, Uuid>> {
        self.get("/rooms").await
    }
    pub async fn room_children(&self, room: Uuid) -> Result<Vec<Summary>> {
        self.get(&format!("/rooms/{room}/children")).await
    }
    pub async fn room_objects(&self, room: Uuid, limit: i64, offset: i64) -> Result<Vec<Summary>> {
        self.get_q(&format!("/rooms/{room}/objects"), &[("limit", limit), ("offset", offset)]).await
    }
    pub async fn room_path(&self, room: Uuid) -> Result<Vec<Summary>> {
        self.get(&format!("/rooms/{room}/path")).await
    }

    pub async fn put_content(&self, id: Uuid, data: Vec<u8>, media_type: &str, filename: Option<&str>) -> Result<i64> {
        let mut r = self.req(reqwest::Method::PUT, &format!("/objects/{id}/content")).header(reqwest::header::CONTENT_TYPE, media_type).body(data);
        if let Some(f) = filename {
            r = r.header("x-filename", f);
        }
        let v: Value = Self::check(r.send().await?).await?.json().await?;
        Ok(v["txn"].as_i64().unwrap_or_default())
    }

    pub async fn get_content(&self, id: Uuid) -> Result<Vec<u8>> {
        Ok(Self::check(self.req(reqwest::Method::GET, &format!("/objects/{id}/content")).send().await?).await?.bytes().await?.to_vec())
    }

    pub async fn put_text(&self, id: Uuid, text_type: &str, content: &str) -> Result<i64> {
        let r = self.req(reqwest::Method::PUT, &format!("/objects/{id}/texts/{text_type}")).body(content.to_string());
        let v: Value = Self::check(r.send().await?).await?.json().await?;
        Ok(v["txn"].as_i64().unwrap_or_default())
    }

    pub async fn get_text(&self, id: Uuid, text_type: &str) -> Result<String> {
        Ok(Self::check(self.req(reqwest::Method::GET, &format!("/objects/{id}/texts/{text_type}")).send().await?).await?.text().await?)
    }

    /// Evaluate a path (FHIRPath, profile views) on an object.
    pub async fn eval(&self, id: Uuid, expr: &str) -> Result<Vec<Value>> {
        self.get_q(&format!("/objects/{id}/eval"), &[("expr", expr)]).await
    }
    pub async fn aspects(&self) -> Result<Vec<Value>> {
        self.get("/aspects").await
    }
    /// Children of a node of an aspect; `""` is the entry.
    pub async fn aspect_children(&self, aspect: &str, node: &str) -> Result<Vec<AspectNode>> {
        self.get_q(&format!("/aspects/{aspect}/children"), &[("node", node)]).await
    }
    /// Children of a node, one page (built-in views page their long listings).
    pub async fn aspect_children_page(&self, aspect: &str, node: &str, offset: i64, limit: i64) -> Result<Vec<AspectNode>> {
        self.get_q(&format!("/aspects/{aspect}/children"), &[("node", node), ("offset", &offset.to_string()), ("limit", &limit.to_string())]).await
    }
    pub async fn aspect_node(&self, aspect: &str, node: &str) -> Result<Value> {
        self.get_q(&format!("/aspects/{aspect}/node"), &[("node", node)]).await
    }
    pub async fn aspect_file(&self, aspect: &str, node: &str, name: &str) -> Result<Vec<u8>> {
        let r = self.req(reqwest::Method::GET, &format!("/aspects/{aspect}/file")).query(&[("node", node), ("name", name)]);
        Ok(Self::check(r.send().await?).await?.bytes().await?.to_vec())
    }

    pub async fn roles(&self) -> Result<Vec<Value>> {
        self.get("/roles").await
    }
    pub async fn assign(&self, role: &str, carrier: Uuid) -> Result<Uuid> {
        let v: Value = self.post("/assignments", &json!({"role": role, "carrier": carrier})).await?;
        Ok(serde_json::from_value(v["id"].clone()).unwrap_or_default())
    }
    pub async fn create_token(&self, account: Uuid) -> Result<String> {
        let v: Value = self.post("/tokens", &json!({"account": account})).await?;
        Ok(v["token"].as_str().unwrap_or_default().to_string())
    }
}
