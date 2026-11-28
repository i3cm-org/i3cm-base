//! Uploads, idempotent requests and streamed downloads with `Range`.

use super::{auth, ApiError, R};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use i3cm_core::BaseError;
use i3cm_store::files::Chunks;
use i3cm_store::uploads::{ContentBody, ContentInfo, IdemStart};
use i3cm_store::{Ctx, Store};
use serde::Deserialize;
use serde_json::Value;
use sha2::Digest;

/// An idempotent request in progress.
pub(crate) enum Idem {
    /// No `Idempotency-Key`.
    Off,
    /// First request with this key.
    Key(String),
    /// The stored answer of an earlier request with the same key and body.
    Replay(Response),
}

fn replay(status: u16, body: Value) -> Response {
    let mut r = (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), Json(body)).into_response();
    r.headers_mut().insert("idempotent-replay", HeaderValue::from_static("true"));
    r
}

/// Start an idempotent request when the header `Idempotency-Key` is present; `scope` names the
/// endpoint, `body` the request content (compared on a repeat).
pub(crate) async fn idem_start(s: &Store, ctx: &Ctx, h: &HeaderMap, scope: &str, body: &[u8]) -> R<Idem> {
    let Some(key) = h.get("idempotency-key") else { return Ok(Idem::Off) };
    let key = key.to_str().map_err(|_| BaseError::invalid("idempotency.bad_key", "Idempotency-Key must be visible ASCII characters"))?.trim().to_string();
    let sha = hex::encode(sha2::Sha256::digest(body));
    Ok(match s.idem_begin(ctx, &key, scope, &sha).await? {
        IdemStart::New => Idem::Key(key),
        IdemStart::Replay(status, v) => Idem::Replay(replay(status, v)),
    })
}

/// Run idempotent work to its end even when the client goes away: the work and the recording of
/// its answer run in a task of their own.
pub(crate) async fn idem_run<F>(s: Store, ctx: Ctx, idem: Idem, work: F) -> R<Response>
where
    F: std::future::Future<Output = R<Value>> + Send + 'static,
{
    if let Idem::Replay(r) = idem {
        return Ok(r);
    }
    let task = tokio::spawn(async move {
        let res = work.await;
        match (idem, res) {
            (Idem::Key(k), Ok(v)) => {
                s.idem_finish(&ctx, &k, 200, &v).await.map_err(ApiError::from)?;
                Ok(v)
            }
            (Idem::Key(k), Err(e)) => {
                let _ = s.idem_abort(&ctx, &k).await;
                Err(e)
            }
            (_, res) => res,
        }
    });
    let v = task.await.map_err(|e| BaseError::internal(format!("request task: {e}")))??;
    Ok(Json(v).into_response())
}

/// Finish an idempotent request: store a success, forget the key after an error.
pub(crate) async fn idem_end(s: &Store, ctx: &Ctx, idem: Idem, res: R<Value>) -> R<Response> {
    match (idem, res) {
        (Idem::Replay(r), _) => Ok(r),
        (Idem::Off, res) => Ok(Json(res?).into_response()),
        (Idem::Key(k), Ok(v)) => {
            s.idem_finish(ctx, &k, 200, &v).await?;
            Ok(Json(v).into_response())
        }
        (Idem::Key(k), Err(e)) => {
            let _ = s.idem_abort(ctx, &k).await;
            Err(e)
        }
    }
}

fn chunks(body: Body) -> Chunks<'static> {
    use futures_util::TryStreamExt;
    Box::pin(body.into_data_stream().map_err(std::io::Error::other))
}

#[derive(Deserialize, Default)]
pub(crate) struct OpenQ {
    #[serde(default)]
    single: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct OpenBody {
    #[serde(default)]
    sha256: Option<String>,
    /// Number of parts (complete): missing parts are named even at the end.
    #[serde(default)]
    parts: Option<i32>,
}

/// `POST /v1/uploads`: open an upload (JSON body `{sha256?}`), or with `?single=1` upload a
/// whole small file as the body.
pub(crate) async fn open_upload(State(s): State<Store>, h: HeaderMap, Query(q): Query<OpenQ>, body: Body) -> R<Response> {
    let r = auth(&s, &h).await?;
    if q.single.as_deref().is_some_and(|v| v != "0" && v != "false") {
        let fs = s.files()?;
        let (tmp, sha, size) = fs.write_single(chunks(body)).await?;
        let fp = format!("{sha}\n{}", q.sha256.as_deref().unwrap_or(""));
        let idem = match idem_start(&s, &r.ctx, &h, "upload:single", fp.as_bytes()).await {
            Ok(i) => i,
            Err(e) => {
                fs.drop_tmp(&tmp);
                return Err(e);
            }
        };
        if let Idem::Replay(resp) = idem {
            fs.drop_tmp(&tmp);
            return Ok(resp);
        }
        let res = s.single_upload(&r.ctx, &tmp, &sha, size, q.sha256.as_deref()).await.map_err(ApiError::from);
        return idem_end(&s, &r.ctx, idem, res).await;
    }
    let bytes = axum::body::to_bytes(body, 64 << 10).await.map_err(|_| BaseError::invalid("upload.bad_request", "the body of an open request is a small JSON object; use ?single=1 to send a file"))?;
    let b: OpenBody = if bytes.iter().all(u8::is_ascii_whitespace) {
        OpenBody::default()
    } else {
        serde_json::from_slice(&bytes).map_err(|e| BaseError::invalid("upload.bad_request", "the body must be JSON like {\"sha256\": \"…\"}").with("detail", e.to_string()))?
    };
    // the query is part of the request: a checksum given there counts like one in the body
    let fp = [bytes.as_ref(), b"\n", q.sha256.as_deref().unwrap_or("").as_bytes()].concat();
    let idem = idem_start(&s, &r.ctx, &h, "upload:open", &fp).await?;
    let sha = b.sha256.or(q.sha256);
    let res = match &idem {
        Idem::Replay(_) => Ok(Value::Null),
        _ => s.open_upload(&r.ctx, sha.as_deref()).await.map_err(ApiError::from),
    };
    idem_end(&s, &r.ctx, idem, res).await
}

/// `PUT /v1/uploads/{id}/parts/{n}`: one part as the body.
pub(crate) async fn put_part(State(s): State<Store>, h: HeaderMap, Path((id, n)): Path<(String, i32)>, body: Body) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(s.put_upload_part(&r.ctx, &id, n, chunks(body)).await?))
}

/// `POST /v1/uploads/{id}/complete` (JSON body `{sha256?}`).
pub(crate) async fn complete(State(s): State<Store>, h: HeaderMap, Path(id): Path<String>, body: axum::body::Bytes) -> R<Response> {
    let r = auth(&s, &h).await?;
    let b: OpenBody = if body.iter().all(u8::is_ascii_whitespace) {
        OpenBody::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| BaseError::invalid("upload.bad_request", "the body must be JSON like {\"sha256\": \"…\"}").with("detail", e.to_string()))?
    };
    let idem = idem_start(&s, &r.ctx, &h, &format!("upload:complete:{id}"), &body).await?;
    // assembling runs to its end even when the client goes away
    let (st, ctx) = (s.clone(), r.ctx.clone());
    idem_run(s, r.ctx, idem, async move { st.complete_upload(&ctx, &id, b.sha256.as_deref(), b.parts).await.map_err(ApiError::from) }).await
}

/// `GET /v1/uploads/{id}`: state and parts of an upload.
pub(crate) async fn upload_info(State(s): State<Store>, h: HeaderMap, Path(id): Path<String>) -> R<Json<Value>> {
    let r = auth(&s, &h).await?;
    Ok(Json(s.upload_info(&r.ctx, &id).await?))
}

/// A byte range of `size` bytes from a `Range` header: `Ok(None)` for the whole, `Err` when it
/// cannot be satisfied.
fn range(h: &HeaderMap, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(v) = h.get(header::RANGE).and_then(|v| v.to_str().ok()) else { return Ok(None) };
    let v = v.trim();
    let Some((unit, spec)) = v.split_once('=') else { return Ok(None) };
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return Ok(None);
    }
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    if spec.contains(',') {
        // several ranges: answer with the whole file
        return Ok(None);
    }
    let (a, b) = spec.split_once('-').ok_or(())?;
    let (a, b) = (a.trim(), b.trim());
    if (!a.is_empty() && !digits(a)) || (!b.is_empty() && !digits(b)) || (a.is_empty() && b.is_empty()) {
        return Ok(None);
    }
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        (size.saturating_sub(n), size.saturating_sub(1))
    } else {
        let start: u64 = a.parse().map_err(|_| ())?;
        let end = if b.is_empty() { size.saturating_sub(1) } else { b.parse::<u64>().map_err(|_| ())?.min(size.saturating_sub(1)) };
        (start, end)
    };
    if size == 0 || start > end || start >= size {
        return Err(());
    }
    Ok(Some((start, end)))
}

/// A streamed answer for a content, with `Range` and `Accept-Ranges`.
pub(crate) async fn stream(h: &HeaderMap, info: ContentInfo, disposition: bool) -> R<Response> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let size = info.size;
    let r = match range(h, size) {
        Ok(r) => r,
        Err(()) => {
            let mut resp = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            if let Ok(v) = format!("bytes */{size}").parse() {
                resp.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return Ok(resp);
        }
    };
    let (start, len) = match r {
        Some((a, b)) => (a, b - a + 1),
        None => (0, size),
    };
    let body = match info.body {
        ContentBody::Bytes(b) => Body::from(b[start as usize..(start + len) as usize].to_vec()),
        ContentBody::File { path, .. } => {
            let mut f = tokio::fs::File::open(&path).await.map_err(|e| BaseError::internal(format!("file store: open: {e}")))?;
            f.seek(std::io::SeekFrom::Start(start)).await.map_err(|e| BaseError::internal(format!("file store: seek: {e}")))?;
            Body::from_stream(tokio_util::io::ReaderStream::with_capacity(f.take(len), 256 << 10))
        }
    };
    let mut resp = Response::new(body);
    *resp.status_mut() = if r.is_some() { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK };
    let hs = resp.headers_mut();
    if let Ok(v) = info.media_type.parse() {
        hs.insert(header::CONTENT_TYPE, v);
    }
    hs.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    hs.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    if let Ok(v) = format!("\"{}\"", info.sha256).parse() {
        hs.insert(header::ETAG, v);
    }
    if let Some((a, b)) = r
        && let Ok(v) = format!("bytes {a}-{b}/{size}").parse() {
            hs.insert(header::CONTENT_RANGE, v);
        }
    if disposition
        && let Some(f) = info.filename
        && let Ok(v) = format!("attachment; filename=\"{}\"", f.replace(['"', '\r', '\n'], "")).parse() {
            hs.insert(header::CONTENT_DISPOSITION, v);
        }
    Ok(resp)
}
