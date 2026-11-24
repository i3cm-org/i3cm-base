//! base-api: REST API (`/v1`, JSON). Every request carries `Authorization: Bearer <token>`;
//! the token determines tenant and `me`. Display names follow `Accept-Language`.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base_core::{BaseError, ErrorKind};
use base_store::read::ListQuery;
use base_store::{Ctx, Op, Order, Store};
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
}

async fn auth(s: &Store, h: &HeaderMap) -> R<Req> {
    let token = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| BaseError::new(ErrorKind::Unauthenticated, "auth.missing_token", "bearer token required"))?;
    let (tenant, account) = s.authenticate(token.trim()).await?;
    let ctx = s.ctx(tenant, account).await?;
    let lang = s.i18n.pick(h.get(header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()));
    Ok(Req { ctx, lang })
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
    role: String,
    carrier: Uuid,
}

#[derive(Deserialize)]
struct TokenBody {
    account: Uuid,
}

pub fn router(store: Store) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/catalog", get(catalog))
        .route("/v1/orders", post(order))
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
        .route("/v1/roles", get(roles))
        .route("/v1/assignments", post(assign))
        .route("/v1/tokens", post(token))
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

async fn order(State(s): State<Store>, h: HeaderMap, Json(o): Json<Order>) -> R<Json<Value>> {
    run(&s, &h, o).await
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
    if let Some(f) = filename {
        if let Ok(v) = format!("attachment; filename=\"{}\"", f.replace('"', "")).parse() {
            resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
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
    Ok(Json(serde_json::to_value(s.get_object(&r.ctx, r.ctx.me(), None, &r.lang).await?).unwrap_or_default()))
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

async fn roles(State(s): State<Store>, h: HeaderMap) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(Value::Array(s.roles(&r.ctx).await?)))
}

async fn assign(State(s): State<Store>, h: HeaderMap, Json(b): Json<AssignBody>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let id = s.assign(&r.ctx, &b.role, b.carrier).await?;
    Ok(Json(json!({"id": id})))
}

async fn token(State(s): State<Store>, h: HeaderMap, Json(b): Json<TokenBody>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    let t = s.create_token(&r.ctx, b.account).await?;
    Ok(Json(json!({"token": t})))
}
