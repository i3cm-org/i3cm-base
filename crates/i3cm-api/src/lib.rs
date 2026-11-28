//! i3cm-api: REST API (`/v1`, JSON). Every request carries `Authorization: Bearer <token>`;
//! the token determines tenant and `me`. Display names follow `Accept-Language`.

/// Source revision the binaries were built from (`I3CM_REV` or git).
pub const REVISION: &str = env!("I3CM_REV");

pub mod describe;
pub mod dev;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use base64::Engine;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use axum::{Json, Router};
use i3cm_core::{BaseError, ErrorKind};
use i3cm_store::read::ListQuery;
use i3cm_store::fs::FsTree;
use i3cm_store::{Auth, Ctx, Op, Order, Store};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

pub struct ApiError(BaseError);

impl From<BaseError> for ApiError {
    fn from(e: BaseError) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.kind {
            ErrorKind::Invalid => StatusCode::BAD_REQUEST,
            ErrorKind::Unauthenticated => StatusCode::UNAUTHORIZED,
            ErrorKind::NotFound => StatusCode::NOT_FOUND,
            ErrorKind::Conflict => StatusCode::CONFLICT,
            ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error = %self.0, "internal error");
        }
        (status, Json(self.0)).into_response()
    }
}

type R<T> = Result<T, ApiError>;

struct Req {
    ctx: Ctx,
    lang: String,
    auth: Auth,
}

async fn auth(s: &Store, h: &HeaderMap) -> R<Req> {
    let token = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| BaseError::new(ErrorKind::Unauthenticated, "auth.missing_token", "bearer token required"))?;
    let auth = s.authenticate(token.trim()).await?;
    let ctx = s.ctx(auth.tenant, auth.account).await?;
    let lang = s.i18n.pick(h.get(header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()));
    Ok(Req { ctx, lang, auth })
}

#[derive(Deserialize, Default)]
struct DryRun {
    #[serde(default)]
    dry_run: bool,
}

#[derive(Deserialize, Default)]
struct At {
    at: Option<i64>,
}

#[derive(Deserialize, Default)]
struct Page {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
struct Transitive {
    #[serde(default)]
    transitive: bool,
}

#[derive(Deserialize)]
struct GroupQuery {
    attr: String,
    unit: String,
    from: Option<String>,
    to: Option<String>,
}

#[derive(Deserialize)]
struct CreateBody {
    room: Option<String>,
    types: BTreeMap<String, Map<String, Value>>,
}

#[derive(Deserialize)]
struct UpdateBody {
    types: BTreeMap<String, Map<String, Value>>,
}

#[derive(Deserialize)]
struct MoveBody {
    room: String,
}

#[derive(Deserialize)]
struct LinkBody {
    reference: String,
    from: String,
    to: String,
    rank: Option<i32>,
}

#[derive(Deserialize)]
struct AssignBody {
    profile: String,
    carrier: Uuid,
}

#[derive(Deserialize)]
struct TokenBody {
    account: Option<Uuid>,
    label: Option<String>,
    ttl: Option<i64>,
}

#[derive(Deserialize, Default)]
struct TokenQuery {
    account: Option<Uuid>,
}

/// Request counter (`GET /v1/metrics`), e.g. to measure what a file system walk costs.
#[derive(Default)]
pub struct Metrics {
    requests: AtomicU64,
}

async fn count(axum::extract::State(m): axum::extract::State<Arc<Metrics>>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    if req.uri().path() != "/v1/metrics" {
        m.requests.fetch_add(1, Ordering::Relaxed);
    }
    next.run(req).await
}

/// The API without module code (hooks and module operations unavailable).
pub fn router(store: Store) -> Router {
    router_with(store, None)
}

#[derive(Clone)]
struct Mods {
    store: Store,
    rt: Option<i3cm_wasm::Runtime>,
}

/// The API with the WebAssembly runtime for module operations and hooks (DEC-119).
pub fn router_with(store: Store, rt: Option<i3cm_wasm::Runtime>) -> Router {
    let mods = Router::new()
        .route("/v1/orders", post(mod_order))
        .route("/v1/modules", get(modules))
        .route("/v1/modules/{m}/activate", post(activate))
        .route("/v1/modules/{m}/ops/{op}", post(module_op))
        .route("/v1/modules/{m}/sources", get(module_sources))
        .route("/v1/catalog/check", post(catalog_check))
        .with_state(Mods { store: store.clone(), rt });
    base_router(store).merge(mods)
}

fn base_router(store: Store) -> Router {
    let metrics = Arc::new(Metrics::default());
    let m = metrics.clone();
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/", get(|| async { ([(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], describe::ENTRY) }))
        .route("/guide/agents", get(|| async { ([(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], describe::GUIDE) }))
        .route("/openapi.json", get(|| async { Json(describe::openapi()) }))
        .route("/skill.zip", get(|| async { ([(header::CONTENT_TYPE, "application/zip")], describe::skill_zip()) }))
        .route("/v1/version", get(|| async { Json(json!({"version": env!("CARGO_PKG_VERSION"), "revision": env!("I3CM_REV")})) }))
        .route("/v1/catalog", get(catalog))
        .route("/v1/objects", post(create))
        .route("/v1/objects/{id}", get(object).patch(update).delete(remove))
        .route("/v1/objects/{id}/types/{t}", post(attach).delete(detach))
        .route("/v1/objects/{id}/move", post(move_object))
        .route("/v1/objects/{id}/versions", get(versions))
        .route("/v1/objects/{id}/ends/{end}", get(ends))
        .route("/v1/objects/{id}/content", get(get_content).put(put_content))
        .route("/v1/objects/{id}/texts/{tt}", get(get_text).put(put_text))
        .route("/v1/references", post(link))
        .route("/v1/references/{reference}/{id}", delete(unlink))
        .route("/v1/me", get(me))
        .route("/v1/me/groups", get(my_groups))
        .route("/v1/groups/{id}/members", get(members))
        .route("/v1/rooms", get(rooms))
        .route("/v1/rooms/{id}/children", get(room_children))
        .route("/v1/rooms/{id}/objects", get(room_objects))
        .route("/v1/rooms/{id}/path", get(room_path))
        .route("/v1/types/{t}/objects", get(list))
        .route("/v1/types/{t}/groups", get(groups))
        .route("/v1/profiles", get(profiles))
        .route("/v1/assignments", post(assign).get(assignments))
        .route("/v1/assignments/{id}", delete(unassign))
        .route("/v1/tokens", post(token).get(tokens))
        .route("/v1/tokens/{id}", delete(revoke))
        .route("/v1/metrics", get(move |State(s): State<Store>, h: HeaderMap| async move {
            auth(&s, &h).await?;
            Ok::<_, ApiError>(Json(json!({"requests": m.requests.load(Ordering::Relaxed)})))
        }))
        .route("/v1/aspects/{a}/entry", get(aspect_entry))
        .route("/v1/aspects/{a}/index", get(aspect_index).put(put_aspect_index))
        .route("/v1/aspects/{a}/drop", post(aspect_drop))
        .route("/v1/aspects/{a}/files", post(aspect_files))
        .route("/v1/tools/exec", post(tool_exec))
        .route("/v1/tools/{tool}", post(tool_call))
        .route("/v1/aspects", get(aspects))
        .route("/v1/aspects/{a}/children", get(aspect_children))
        .route("/v1/aspects/{a}/node", get(aspect_node))
        .route("/v1/aspects/{a}/path", get(aspect_path))
        .route("/v1/aspects/{a}/file", get(aspect_file).put(put_aspect_file))
        .route("/v1/aspects/{a}/tab", get(aspect_tab))
        .route("/v1/objects/{id}/eval", get(eval_path))
        .layer(axum::extract::DefaultBodyLimit::max(64 << 20))
        .layer(axum::middleware::from_fn_with_state(metrics, count))
        .with_state(store)
}

async fn catalog(State(s): State<Store>, h: HeaderMap) -> R<Json<Value>> {
    auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(&s.model.catalog).unwrap_or_default()))
}

async fn run(s: &Store, h: &HeaderMap, order: Order) -> R<Json<Value>> {
    let mut r = auth(s, h).await?;
    let res = s.execute(&mut r.ctx, &order).await?;
    Ok(Json(serde_json::to_value(res).unwrap_or_default()))
}

async fn mod_order(State(m): State<Mods>, h: HeaderMap, Json(o): Json<Order>) -> R<Json<Value>> {
    match &m.rt {
        Some(rt) => {
            let mut r = auth(&m.store, &h).await?;
            let res = rt.execute(&mut r.ctx, &o).await?;
            Ok(Json(serde_json::to_value(res).unwrap_or_default()))
        }
        None => run(&m.store, &h, o).await,
    }
}

async fn modules(State(m): State<Mods>, h: HeaderMap) -> R<Json<Value>> {
    let r = auth(&m.store, &h).await?;
    let list: Vec<Value> = m
        .store
        .model
        .catalog
        .modules
        .iter()
        .map(|x| {
            json!({
                "name": x.name, "requires": x.requires, "mandatory": x.mandatory || x.name == "base", "developmentOnly": x.development_only,
                "active": !r.ctx.inactive.contains(&x.name),
                "operations": x.operations.iter().map(|o| o.name.clone()).collect::<Vec<_>>(),
                "code": m.rt.is_some() && !x.operations.is_empty(),
            })
        })
        .collect();
    Ok(Json(json!(list)))
}

/// Studio (DEC-136): check uploaded module sources; development tenants only, for guardians
/// or whoever may write `studio:SourceFile`.
async fn catalog_check(State(m): State<Mods>, h: HeaderMap, Json(b): Json<dev::CheckBody>) -> R<Json<Value>> {
    let r = auth(&m.store, &h).await?;
    if !r.ctx.development {
        return Err(BaseError::invalid("check.not_development", "checking sources is only available in a development tenant").into());
    }
    let may = r.ctx.subject.guardian
        || (!r.ctx.inactive.contains("studio") && r.ctx.subject.may(i3cm_core::Art::Write, "studio:SourceFile", &[r.ctx.rooms.root]));
    if !may {
        return Err(BaseError::not_found("rights.denied", "not allowed or not found").with("art", "write").with("type", "studio:SourceFile").into());
    }
    let installed: Vec<(String, Vec<String>)> = m.store.model.catalog.modules.iter().map(|x| (x.name.clone(), x.requires.clone())).collect();
    Ok(Json(dev::check(b, &installed).await?))
}

/// Sources of an installed module (from its package), for modules active in the tenant or any
/// installed module in a development tenant.
async fn module_sources(State(m): State<Mods>, h: HeaderMap, Path(name): Path<String>) -> R<Json<Value>> {
    let r = auth(&m.store, &h).await?;
    let known = m.store.model.catalog.modules.iter().any(|x| x.name == name);
    if !known || (r.ctx.inactive.contains(&name) && !r.ctx.development) {
        return Err(BaseError::not_found("module.not_installed", "module is not installed").with("module", name).into());
    }
    match dev::read_sources(&name) {
        Some(files) => Ok(Json(json!({ "module": name, "files": files }))),
        None => Err(BaseError::not_found("module.no_sources", "the package of this module carries no sources").with("module", name).into()),
    }
}

async fn activate(State(m): State<Mods>, h: HeaderMap, Path(name): Path<String>) -> R<Json<Value>> {
    let r = auth(&m.store, &h).await?;
    let a = m.store.activate_module(&r.ctx, &name).await?;
    Ok(Json(serde_json::to_value(a).unwrap_or_default()))
}

async fn module_op(State(m): State<Mods>, h: HeaderMap, Path((name, op)): Path<(String, String)>, body: Option<Json<Value>>) -> R<Json<Value>> {
    let r = auth(&m.store, &h).await?;
    let rt = m.rt.as_ref().ok_or_else(|| BaseError::not_found("module.no_runtime", "this server runs without module code"))?;
    if r.ctx.inactive.contains(&name) {
        return Err(BaseError::not_found("module.inactive", "module is not active in the tenant").with("module", name).into());
    }
    let params = body.map(|Json(v)| v).unwrap_or(Value::Null);
    Ok(Json(rt.run_op(&r.ctx, &name, &op, &params, Some("api")).await?))
}

async fn create(State(s): State<Store>, h: HeaderMap, Query(d): Query<DryRun>, Json(b): Json<CreateBody>) -> R<Json<Value>> {
    run(&s, &h, Order { dry_run: d.dry_run, ops: vec![Op::Create { local: Some("new".into()), room: b.room, types: b.types }] }).await
}

async fn object(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>, Query(at): Query<At>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.get_object(&r.ctx, id, at.at, &r.lang).await?).unwrap_or_default()))
}

async fn update(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>, Query(d): Query<DryRun>, Json(b): Json<UpdateBody>) -> R<Json<Value>> {
    run(&s, &h, Order { dry_run: d.dry_run, ops: vec![Op::Update { id: id.to_string(), types: b.types }] }).await
}

async fn remove(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>, Query(d): Query<DryRun>) -> R<Json<Value>> {
    run(&s, &h, Order { dry_run: d.dry_run, ops: vec![Op::Delete { id: id.to_string() }] }).await
}

async fn attach(State(s): State<Store>, h: HeaderMap, Path((id, t)): Path<(Uuid, String)>, Query(d): Query<DryRun>, body: Option<Json<Map<String, Value>>>) -> R<Json<Value>> {
    let values = body.map(|b| b.0).unwrap_or_default();
    run(&s, &h, Order { dry_run: d.dry_run, ops: vec![Op::Attach { id: id.to_string(), type_name: t, values }] }).await
}

async fn detach(State(s): State<Store>, h: HeaderMap, Path((id, t)): Path<(Uuid, String)>, Query(d): Query<DryRun>) -> R<Json<Value>> {
    run(&s, &h, Order { dry_run: d.dry_run, ops: vec![Op::Detach { id: id.to_string(), type_name: t }] }).await
}

async fn move_object(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>, Query(d): Query<DryRun>, Json(b): Json<MoveBody>) -> R<Json<Value>> {
    run(&s, &h, Order { dry_run: d.dry_run, ops: vec![Op::Move { id: id.to_string(), room: b.room }] }).await
}

async fn link(State(s): State<Store>, h: HeaderMap, Query(d): Query<DryRun>, Json(b): Json<LinkBody>) -> R<Json<Value>> {
    run(&s, &h, Order { dry_run: d.dry_run, ops: vec![Op::Link { reference: b.reference, from: b.from, to: b.to, rank: b.rank }] }).await
}

async fn unlink(State(s): State<Store>, h: HeaderMap, Path((reference, id)): Path<(String, Uuid)>, Query(d): Query<DryRun>) -> R<Json<Value>> {
    run(&s, &h, Order { dry_run: d.dry_run, ops: vec![Op::Unlink { reference, id }] }).await
}

async fn versions(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(Value::Array(s.versions(&r.ctx, id).await?)))
}

async fn ends(State(s): State<Store>, h: HeaderMap, Path((id, end)): Path<(Uuid, String)>, Query(p): Query<Page>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let v = s.ends(&r.ctx, id, &end, p.limit.unwrap_or(100).clamp(1, 1000), p.offset.unwrap_or(0).max(0), &r.lang).await?;
    Ok(Json(serde_json::to_value(v).unwrap_or_default()))
}

async fn get_content(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>) -> R<Response> {
    let r = auth(&s, &h).await?;
    let (data, media, filename) = s.get_content(&r.ctx, id).await?;
    let mut resp = (StatusCode::OK, [(header::CONTENT_TYPE, media)], data).into_response();
    if let Some(f) = filename
        && let Ok(v) = format!("attachment; filename=\"{}\"", f.replace('"', "")).parse() {
            resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    Ok(resp)
}

async fn put_content(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>, body: Bytes) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let media = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
    let filename = h.get("x-filename").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
    let txn = s.put_content(&r.ctx, id, &body, &media, filename.as_deref()).await?;
    Ok(Json(json!({"txn": txn})))
}

async fn get_text(State(s): State<Store>, h: HeaderMap, Path((id, tt)): Path<(Uuid, String)>) -> R<Response> {
    let r = auth(&s, &h).await?;
    let (content, media) = s.get_text(&r.ctx, id, &tt).await?;
    Ok((StatusCode::OK, [(header::CONTENT_TYPE, format!("{media}; charset=utf-8"))], content).into_response())
}

async fn put_text(State(s): State<Store>, h: HeaderMap, Path((id, tt)): Path<(Uuid, String)>, body: String) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let txn = s.put_text(&r.ctx, id, &tt, &body).await?;
    Ok(Json(json!({"txn": txn})))
}

async fn me(State(s): State<Store>, h: HeaderMap) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let mut v = serde_json::to_value(s.get_object(&r.ctx, r.ctx.me(), None, &r.lang).await?).unwrap_or_default();
    let name = s.tenant_name(r.ctx.tenant).await?;
    if let Some(o) = v.as_object_mut() {
        o.insert("_tenant".into(), json!({ "id": r.ctx.tenant, "name": name, "development": r.ctx.development }));
    }
    Ok(Json(v))
}

async fn my_groups(State(s): State<Store>, h: HeaderMap) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.my_groups(&r.ctx, &r.lang).await?).unwrap_or_default()))
}

async fn members(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>, Query(t): Query<Transitive>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.members(&r.ctx, id, t.transitive, &r.lang).await?).unwrap_or_default()))
}

async fn rooms(State(s): State<Store>, h: HeaderMap) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.module_rooms(&r.ctx).await?).unwrap_or_default()))
}

async fn room_children(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.room_children(&r.ctx, id, &r.lang).await?).unwrap_or_default()))
}

async fn room_objects(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>, Query(p): Query<Page>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let v = s.room_objects(&r.ctx, id, p.limit.unwrap_or(100).clamp(1, 1000), p.offset.unwrap_or(0).max(0), &r.lang).await?;
    Ok(Json(serde_json::to_value(v).unwrap_or_default()))
}

async fn room_path(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.room_path(&r.ctx, id, &r.lang).await?).unwrap_or_default()))
}

async fn list(State(s): State<Store>, h: HeaderMap, Path(t): Path<String>, Query(q): Query<ListQuery>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.list(&r.ctx, &t, &q, &r.lang).await?).unwrap_or_default()))
}

async fn groups(State(s): State<Store>, h: HeaderMap, Path(t): Path<String>, Query(q): Query<GroupQuery>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let lq = ListQuery { from: q.from, to: q.to, ..Default::default() };
    Ok(Json(Value::Array(s.group_counts(&r.ctx, &t, &q.attr, &q.unit, &lq).await?)))
}

async fn profiles(State(s): State<Store>, h: HeaderMap) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(Value::Array(s.profiles(&r.ctx).await?)))
}

async fn assign(State(s): State<Store>, h: HeaderMap, Json(b): Json<AssignBody>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let id = s.assign(&r.ctx, &b.profile, b.carrier).await?;
    Ok(Json(json!({"id": id})))
}

#[derive(Deserialize)]
struct AssignmentQuery {
    carrier: Option<Uuid>,
}

async fn assignments(State(s): State<Store>, h: HeaderMap, Query(q): Query<AssignmentQuery>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(Value::Array(s.assignments(&r.ctx, q.carrier).await?)))
}

async fn unassign(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let txn = s.unassign(&r.ctx, id).await?;
    Ok(Json(json!({"ended": id, "txn": txn})))
}

async fn token(State(s): State<Store>, h: HeaderMap, Json(b): Json<TokenBody>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let t = s.create_token_with(&r.ctx, &r.auth, b.account, b.label.as_deref(), b.ttl).await?;
    Ok(Json(serde_json::to_value(t).unwrap_or_default()))
}

async fn tokens(State(s): State<Store>, h: HeaderMap, Query(q): Query<TokenQuery>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.tokens(&r.ctx, q.account).await?).unwrap_or_default()))
}

async fn revoke(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    s.revoke_token(&r.ctx, id).await?;
    Ok(Json(json!({"revoked": id})))
}

#[derive(Deserialize, Default)]
struct NodeQ {
    #[serde(default)]
    node: String,
    name: Option<String>,
    offset: Option<i64>,
    limit: Option<i64>,
    /// Keyset paging: the last node id of the previous page.
    after: Option<String>,
}

#[derive(Deserialize)]
struct ExprQ {
    expr: String,
}

async fn aspects(State(s): State<Store>, h: HeaderMap) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(Value::Array(s.aspect_list(&r.ctx, &r.lang))))
}

async fn aspect_children(State(s): State<Store>, h: HeaderMap, Path(a): Path<String>, Query(q): Query<NodeQ>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.aspect_children_paged(&r.ctx, &a, &q.node, &r.lang, i3cm_store::aspect::Page { offset: q.offset.unwrap_or(0).max(0), limit: q.limit, after: q.after.clone() }).await?).unwrap_or_default()))
}

async fn aspect_path(State(s): State<Store>, h: HeaderMap, Path(a): Path<String>, Query(q): Query<NodeQ>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.aspect_path(&r.ctx, &a, &q.node, &r.lang).await?).unwrap_or_default()))
}

async fn aspect_node(State(s): State<Store>, h: HeaderMap, Path(a): Path<String>, Query(q): Query<NodeQ>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.aspect_node(&r.ctx, &a, &q.node, &r.lang).await?).unwrap_or_default()))
}

#[derive(Deserialize)]
struct TabQ {
    #[serde(default)]
    node: String,
    tab: String,
    offset: Option<i64>,
    limit: Option<i64>,
    sort: Option<usize>,
    #[serde(default)]
    desc: bool,
    format: Option<String>,
}

/// One tab of an object node: JSON paged (`offset`, `limit`, default 100), or every row as
/// `format=csv` / `format=xlsx` for download. Sorting by column index, done by the server.
async fn aspect_tab(State(s): State<Store>, h: HeaderMap, Path(a): Path<String>, Query(q): Query<TabQ>) -> R<Response> {
    let r = auth(&s, &h).await?;
    let format = q.format.as_deref().unwrap_or("json");
    let page = match format {
        "json" => i3cm_store::aspect::Page { offset: q.offset.unwrap_or(0).max(0), limit: Some(q.limit.unwrap_or(100)), after: None },
        "csv" | "xlsx" => i3cm_store::aspect::Page { offset: 0, limit: None, after: None },
        _ => return Err(BaseError::invalid("tab.bad_format", "format is json, csv or xlsx").with("format", format).into()),
    };
    let t = s.aspect_tab(&r.ctx, &a, &q.node, &q.tab, q.sort.map(|c| (c, q.desc)), page, &r.lang).await?;
    if format == "json" {
        return Ok(Json(serde_json::to_value(t).unwrap_or_default()).into_response());
    }
    // a tab without columns exports the display names
    let head: Vec<String> = if t.columns.is_empty() { vec!["Name".into()] } else { t.columns.clone() };
    let rows: Vec<Vec<String>> = t
        .rows
        .iter()
        .map(|row| if t.columns.is_empty() { vec![row.display_name.clone()] } else { row.cells.iter().map(i3cm_store::aspect::cell_text).collect() })
        .collect();
    let file: String = t.label.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let (body, media) = if format == "csv" {
        let field = |x: &str| if x.contains([',', '"', '\n', '\r']) { format!("\"{}\"", x.replace('"', "\"\"")) } else { x.to_string() };
        let mut out = String::from("\u{feff}");
        for line in std::iter::once(&head).chain(rows.iter()) {
            out.push_str(&line.iter().map(|x| field(x)).collect::<Vec<_>>().join(","));
            out.push_str("\r\n");
        }
        (out.into_bytes(), "text/csv; charset=utf-8")
    } else {
        (xlsx(&t.label, &head, &rows).map_err(|e| BaseError::internal(format!("export failed: {e}")))?, "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
    };
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, media.to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{file}.{format}\""))],
        body,
    )
        .into_response())
}

fn xlsx(sheet: &str, head: &[String], rows: &[Vec<String>]) -> Result<Vec<u8>, rust_xlsxwriter::XlsxError> {
    use rust_xlsxwriter::{Format, Workbook};
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    let name: String = sheet.chars().filter(|c| !"[]:*?/\\".contains(*c)).take(31).collect();
    if !name.is_empty() {
        ws.set_name(name)?;
    }
    let bold = Format::new().set_bold();
    for (c, h) in head.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, h, &bold)?;
    }
    for (r, row) in rows.iter().enumerate() {
        for (c, x) in row.iter().enumerate() {
            ws.write_string(r as u32 + 1, c as u16, x)?;
        }
    }
    ws.set_freeze_panes(1, 0)?;
    ws.autofit();
    wb.save_to_buffer()
}

async fn aspect_file(State(s): State<Store>, h: HeaderMap, Path(a): Path<String>, Query(q): Query<NodeQ>) -> R<Response> {
    let r = auth(&s, &h).await?;
    let name = q.name.ok_or_else(|| BaseError::invalid("aspect.no_file", "name required"))?;
    let (data, media) = s.aspect_file(&r.ctx, &a, &q.node, &name).await?;
    Ok((StatusCode::OK, [(header::CONTENT_TYPE, media)], data).into_response())
}

async fn eval_path(State(s): State<Store>, h: HeaderMap, Path(id): Path<Uuid>, Query(q): Query<ExprQ>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(Value::Array(s.eval_path(&r.ctx, id, &q.expr).await?)))
}

async fn aspect_entry(State(s): State<Store>, h: HeaderMap, Path(a): Path<String>, Query(q): Query<NodeQ>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(serde_json::to_value(s.aspect_entry(&r.ctx, &a, &q.node, q.name.as_deref(), &r.lang).await?).unwrap_or_default()))
}

async fn write_one(s: &Store, h: &HeaderMap, item: i3cm_store::write::WriteItem) -> R<Json<Value>> {
    let mut r = auth(s, h).await?;
    let lang = r.lang.clone();
    let res = s.write_files(&mut r.ctx, &[item], &lang).await?;
    Ok(Json(serde_json::to_value(res).unwrap_or_default()))
}

/// Write a file of a view node (content or text of the object behind it).
async fn put_aspect_file(State(s): State<Store>, h: HeaderMap, Path(a): Path<String>, Query(q): Query<NodeQ>, body: axum::body::Bytes) -> R<Json<Value>> {
    let name = q.name.clone().ok_or_else(|| BaseError::invalid("aspect.no_file", "parameter name missing"))?;
    write_one(&s, &h, i3cm_store::write::WriteItem { aspect: a, node: q.node, name: Some(name), data: body.to_vec(), drop: false }).await
}

/// Write the index of an object node: changed attributes in the front matter.
async fn put_aspect_index(State(s): State<Store>, h: HeaderMap, Path(a): Path<String>, Query(q): Query<NodeQ>, body: axum::body::Bytes) -> R<Json<Value>> {
    write_one(&s, &h, i3cm_store::write::WriteItem { aspect: a, node: q.node, name: None, data: body.to_vec(), drop: false }).await
}

/// A new file in a folder with `dropType`: creates an object.
async fn aspect_drop(State(s): State<Store>, h: HeaderMap, Path(a): Path<String>, Query(q): Query<NodeQ>, body: axum::body::Bytes) -> R<Json<Value>> {
    let name = q.name.clone().ok_or_else(|| BaseError::invalid("aspect.no_file", "parameter name missing"))?;
    write_one(&s, &h, i3cm_store::write::WriteItem { aspect: a, node: q.node, name: Some(name), data: body.to_vec(), drop: true }).await
}

async fn aspect_index(State(s): State<Store>, h: HeaderMap, Path(a): Path<String>, Query(q): Query<NodeQ>) -> R<Response> {
    let r = auth(&s, &h).await?;
    let md = s.aspect_index(&r.ctx, &a, &q.node, q.name.as_deref(), &r.lang).await?;
    Ok((StatusCode::OK, [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], md).into_response())
}

#[derive(Deserialize)]
struct FileItem {
    node: String,
    name: Option<String>,
    #[serde(default)]
    index: bool,
}

#[derive(Deserialize)]
struct FilesBody {
    items: Vec<FileItem>,
}

/// Several files and indexes in one request (base64): a file system reads a directory at once.
async fn aspect_files(State(s): State<Store>, h: HeaderMap, Path(a): Path<String>, Json(b): Json<FilesBody>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    if b.items.len() > 1000 {
        return Err(BaseError::invalid("aspect.too_many_files", "at most 1000 items").into());
    }
    let mut out = vec![];
    for it in b.items {
        let res = if it.index {
            s.aspect_index(&r.ctx, &a, &it.node, it.name.as_deref(), &r.lang).await.map(String::into_bytes)
        } else {
            match &it.name {
                Some(n) => s.aspect_file(&r.ctx, &a, &it.node, n).await.map(|x| x.0),
                None => Err(BaseError::invalid("aspect.no_file", "name required")),
            }
        };
        out.push(match res {
            Ok(d) => json!({"data": base64::engine::general_purpose::STANDARD.encode(d)}),
            Err(e) => json!({"error_id": e.error_id, "description": e.description}),
        });
    }
    Ok(Json(Value::Array(out)))
}

fn tool_error(e: i3cm_tools::ToolError) -> ApiError {
    ApiError(BaseError::invalid(&e.error_id, e.message).with("supported", e.supported))
}

/// Typed tool call: `{"cwd": "/", "paths": [...], "options": {...}}`.
async fn tool_call(State(s): State<Store>, h: HeaderMap, Path(t): Path<String>, Json(req): Json<i3cm_tools::Request>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let tool = i3cm_tools::TOOLS
        .contains(&t.as_str())
        .then(|| i3cm_tools::Tool::from_name(&t))
        .flatten()
        .ok_or_else(|| BaseError::not_found("tool.unknown", "unknown tool").with("tool", t.clone()).with("supported", i3cm_tools::TOOLS.to_vec()))?;
    let mut call = i3cm_tools::Call { tool, req };
    if call.req.cwd.is_empty() {
        call.req.cwd = "/".into();
    }
    i3cm_tools::check(&call).map_err(tool_error)?;
    let tree = FsTree::new(&s, &r.ctx, &r.lang);
    Ok(Json(serde_json::to_value(i3cm_tools::run(&tree, &call).await).unwrap_or_default()))
}

#[derive(Deserialize)]
struct ExecBody {
    #[serde(default)]
    cwd: String,
    argv: Vec<String>,
}

/// Shell form: `{"cwd": "/", "argv": ["grep", "-rn", "x", "."]}`; the answer also carries the
/// output as the tool prints it.
async fn tool_exec(State(s): State<Store>, h: HeaderMap, Json(b): Json<ExecBody>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let mut call = i3cm_tools::parse(&b.argv).map_err(tool_error)?;
    call.req.cwd = if b.cwd.is_empty() { "/".into() } else { b.cwd };
    let tree = FsTree::new(&s, &r.ctx, &r.lang);
    let res = i3cm_tools::run(&tree, &call).await;
    let (out, err) = i3cm_tools::render(&res);
    Ok(Json(json!({"status": res.status, "records": res.records, "stdout": String::from_utf8_lossy(&out), "stderr": err})))
}
