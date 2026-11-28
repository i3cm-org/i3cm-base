//! i3cm-client: Rust client for the i3cm REST API.
//!
//! ```no_run
//! # async fn demo() -> Result<(), i3cm_client::ClientError> {
//! use i3cm_client::{Client, Order};
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
    /// New content of an object, in the same order as structure changes.
    pub fn content(&mut self, id: impl IdRef, data: &[u8], media_type: &str, filename: Option<&str>) -> &mut Self {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        self.ops.push(json!({"op": "content", "id": id.id_ref(), "mediaType": media_type, "filename": filename, "base64": b64}));
        self
    }
    /// New content of an object from a completed upload (`Client::upload`).
    pub fn content_upload(&mut self, id: impl IdRef, upload: &str, media_type: &str, filename: Option<&str>) -> &mut Self {
        self.ops.push(json!({"op": "content", "id": id.id_ref(), "mediaType": media_type, "filename": filename, "upload": upload}));
        self
    }
    /// New version of a text of an object.
    pub fn text(&mut self, id: impl IdRef, text_type: &str, content: &str) -> &mut Self {
        self.ops.push(json!({"op": "text", "id": id.id_ref(), "textType": text_type, "content": content}));
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderResult {
    pub txn: i64,
    pub dry_run: bool,
    pub created: BTreeMap<String, Uuid>,
    pub links: Vec<Uuid>,
    #[serde(default)]
    pub warnings: Vec<Value>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Uuid>,
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
    /// Name in paths (FUSE, tools).
    #[serde(default)]
    pub name: String,
    /// Name in paths by the aspect's fallback rule, used when names collide.
    #[serde(rename = "fallbackName", default)]
    pub fallback_name: Option<String>,
    /// Bytes of the file; for a node: of its index.
    pub size: Option<u64>,
    #[serde(rename = "changedAt")]
    pub changed_at: Option<String>,
    pub ino: Option<u64>,
}

/// An issued token (the secret is shown once).
#[derive(Debug, Clone, Deserialize)]
pub struct IssuedToken {
    pub id: Uuid,
    pub token: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<String>,
}

/// A token as listed (without secret).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    pub id: Uuid,
    pub account: Uuid,
    pub label: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
    pub last_used: Option<String>,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
    lang: Option<String>,
}

impl Client {
    /// The server address this client talks to (without `/v1`).
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// GET with query parameters, answer as JSON.
    pub async fn get_q_value(&self, path: &str, q: &[(&str, &str)]) -> Result<Value> {
        self.get_q(path, &q).await
    }

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

    /// Send an order with a fresh `Idempotency-Key` (a retry after a network error does not
    /// write twice when it reuses the key: see `order_with_key`).
    pub async fn order(&self, o: &Order) -> Result<OrderResult> {
        self.order_with_key(o, &Uuid::new_v4().to_string()).await
    }

    /// Send an order with the given `Idempotency-Key`.
    pub async fn order_with_key(&self, o: &Order, key: &str) -> Result<OrderResult> {
        let r = self.req(reqwest::Method::POST, "/orders").header("idempotency-key", key).json(&serde_json::to_value(o).unwrap());
        Ok(Self::check(r.send().await?).await?.json().await?)
    }

    /// Open an upload (`sha256` is checked on completion).
    pub async fn upload_open(&self, sha256: Option<&str>) -> Result<Value> {
        self.post("/uploads", &json!({"sha256": sha256})).await
    }

    /// Send part `n` (from 1) of an upload.
    pub async fn upload_part(&self, id: &str, n: i32, data: Vec<u8>) -> Result<Value> {
        Ok(Self::check(self.req(reqwest::Method::PUT, &format!("/uploads/{id}/parts/{n}")).body(data).send().await?).await?.json().await?)
    }

    /// Complete an upload.
    pub async fn upload_complete(&self, id: &str, sha256: Option<&str>, parts: Option<i32>) -> Result<Value> {
        self.post(&format!("/uploads/{id}/complete"), &json!({"sha256": sha256, "parts": parts})).await
    }

    /// Upload bytes and return the handle: one request up to 8 MiB, parts of 8 MiB above.
    pub async fn upload(&self, data: &[u8]) -> Result<String> {
        const PART: usize = 8 << 20;
        if data.len() <= PART {
            let r = self.req(reqwest::Method::POST, "/uploads").query(&[("single", "1")]).body(data.to_vec());
            let v: Value = Self::check(r.send().await?).await?.json().await?;
            return Ok(v["id"].as_str().unwrap_or_default().to_string());
        }
        let v = self.upload_open(None).await?;
        let id = v["id"].as_str().unwrap_or_default().to_string();
        for (i, chunk) in data.chunks(PART).enumerate() {
            self.upload_part(&id, i as i32 + 1, chunk.to_vec()).await?;
        }
        self.upload_complete(&id, None, Some(data.chunks(PART).count() as i32)).await?;
        Ok(id)
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

    /// Write a file of a view node (content or text behind it); one order.
    pub async fn put_aspect_file(&self, aspect: &str, node: &str, name: &str, data: Vec<u8>) -> Result<Value> {
        let r = self.req(reqwest::Method::PUT, &format!("/aspects/{aspect}/file")).query(&[("node", node), ("name", name)]).body(data);
        Ok(Self::check(r.send().await?).await?.json().await?)
    }

    /// Write the index of an object node (attributes in the front matter); one order.
    pub async fn put_aspect_index(&self, aspect: &str, node: &str, text: &str) -> Result<Value> {
        let r = self.req(reqwest::Method::PUT, &format!("/aspects/{aspect}/index")).query(&[("node", node)]).body(text.to_string());
        Ok(Self::check(r.send().await?).await?.json().await?)
    }

    /// Copy a new file into a folder with `dropType`: creates an object.
    pub async fn drop_file(&self, aspect: &str, folder: &str, name: &str, data: Vec<u8>) -> Result<Value> {
        let r = self.req(reqwest::Method::POST, &format!("/aspects/{aspect}/drop")).query(&[("node", folder), ("name", name)]).body(data);
        Ok(Self::check(r.send().await?).await?.json().await?)
    }

    /// One tab of an object node; `query` holds `offset`, `limit`, `sort`, `desc` as needed.
    pub async fn aspect_tab(&self, aspect: &str, node: &str, tab: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut q = vec![("node", node), ("tab", tab)];
        q.extend_from_slice(query);
        self.get_q(&format!("/aspects/{aspect}/tab"), &q).await
    }
    /// All rows of a tab as `csv` or `xlsx`: (bytes, content type, content disposition).
    pub async fn aspect_tab_export(&self, aspect: &str, node: &str, tab: &str, format: &str) -> Result<(Vec<u8>, String, String)> {
        let r = self.req(reqwest::Method::GET, &format!("/aspects/{aspect}/tab")).query(&[("node", node), ("tab", tab), ("format", format)]);
        let r = Self::check(r.send().await?).await?;
        let h = |k: reqwest::header::HeaderName| r.headers().get(k).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        let (ct, cd) = (h(reqwest::header::CONTENT_TYPE), h(reqwest::header::CONTENT_DISPOSITION));
        Ok((r.bytes().await?.to_vec(), ct, cd))
    }

    pub async fn profiles(&self) -> Result<Vec<Value>> {
        self.get("/profiles").await
    }
    pub async fn assign(&self, profile: &str, carrier: Uuid) -> Result<Uuid> {
        let v: Value = self.post("/assignments", &json!({"profile": profile, "carrier": carrier})).await?;
        Ok(serde_json::from_value(v["id"].clone()).unwrap_or_default())
    }
    /// Current profile assignments (`carrier` filters).
    pub async fn assignments(&self, carrier: Option<Uuid>) -> Result<Vec<Value>> {
        match carrier {
            Some(c) => self.get_q("/assignments", &[("carrier", c.to_string())]).await,
            None => self.get("/assignments").await,
        }
    }
    /// End a profile assignment.
    pub async fn unassign(&self, id: Uuid) -> Result<()> {
        Self::check(self.req(reqwest::Method::DELETE, &format!("/assignments/{id}")).send().await?).await?;
        Ok(())
    }
    pub async fn create_token(&self, account: Uuid) -> Result<String> {
        let v: Value = self.post("/tokens", &json!({"account": account})).await?;
        Ok(v["token"].as_str().unwrap_or_default().to_string())
    }
    /// Issue a token; `account: None` issues one for the caller. `ttl` in days.
    pub async fn issue_token(&self, account: Option<Uuid>, label: Option<&str>, ttl: Option<i64>) -> Result<IssuedToken> {
        self.post("/tokens", &json!({"account": account, "label": label, "ttl": ttl})).await
    }
    pub async fn tokens(&self) -> Result<Vec<TokenInfo>> {
        self.get("/tokens").await
    }
    pub async fn revoke_token(&self, id: Uuid) -> Result<()> {
        Self::check(self.req(reqwest::Method::DELETE, &format!("/tokens/{id}")).send().await?).await?;
        Ok(())
    }
    /// Requests served so far by this server process.
    pub async fn metrics(&self) -> Result<Value> {
        self.get("/metrics").await
    }
    /// Index of a node as Markdown; `name` is the directory name relative links start from.
    pub async fn aspect_index(&self, aspect: &str, node: &str, name: Option<&str>) -> Result<String> {
        let mut q = vec![("node", node)];
        if let Some(n) = name {
            q.push(("name", n));
        }
        Ok(Self::check(self.req(reqwest::Method::GET, &format!("/aspects/{aspect}/index")).query(&q).send().await?).await?.text().await?)
    }
    /// One node with metadata as in a listing.
    pub async fn aspect_entry(&self, aspect: &str, node: &str, name: Option<&str>) -> Result<AspectNode> {
        let mut q = vec![("node", node)];
        if let Some(n) = name {
            q.push(("name", n));
        }
        self.get_q(&format!("/aspects/{aspect}/entry"), &q).await
    }
    /// A tool in shell form: `{"status", "records", "stdout", "stderr"}`.
    pub async fn tool_exec(&self, cwd: &str, argv: &[&str]) -> Result<Value> {
        self.post("/tools/exec", &json!({"cwd": cwd, "argv": argv})).await
    }
    /// A tool in typed form: `{"status", "records"}`.
    pub async fn tool(&self, tool: &str, req: &Value) -> Result<Value> {
        self.post(&format!("/tools/{tool}"), req).await
    }
}
