//! Uploads (one file in, one handle out), consuming handles in orders, the sweep of the staging
//! area, idempotent requests and moving old database blobs to the file store.

use crate::context::{db, Ctx};
use crate::files::{Chunks, FileStore, IDEMPOTENCY_TTL, MAX_OPEN_UPLOADS, MAX_STAGED_BYTES, MAX_UPLOAD_SIZE};
use crate::Store;
use i3cm_core::{BaseError, ErrorKind, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio_postgres::GenericClient;

/// Highest part number of an upload.
pub const MAX_UPLOAD_PARTS: i32 = 100_000;

fn not_found() -> BaseError {
    BaseError::not_found("upload.not_found", "no such upload for this account").hint("open an upload with POST /v1/uploads")
}

fn valid_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Where the bytes of a content come from.
pub enum ContentBody {
    /// A file of the file store.
    File { path: PathBuf, size: u64 },
    /// Bytes still held in the database (content written before the file store; `migrate-blobs`).
    Bytes(Vec<u8>),
}

/// Content of an object with its description.
pub struct ContentInfo {
    pub body: ContentBody,
    pub media_type: String,
    pub filename: Option<String>,
    pub sha256: String,
    pub size: u64,
}

/// Start of an idempotent request.
pub enum IdemStart {
    /// First request with this key: go on and record the answer afterwards.
    New,
    /// The same request was answered before: status and body of that answer.
    Replay(u16, Value),
}

impl Store {
    /// Use a file store below `dir` for new content and uploads.
    pub fn with_files(mut self, dir: &std::path::Path) -> Result<Store> {
        let mut fs = FileStore::open(dir)?;
        fs.ttl = match std::env::var("I3CM_UPLOAD_TTL") {
            Ok(v) if !v.trim().is_empty() => v.trim().parse::<i64>().ok().filter(|n| *n > 0).ok_or_else(|| {
                BaseError::invalid("config.bad_value", "I3CM_UPLOAD_TTL must be a positive number of seconds").with("value", v.clone())
            })?,
            _ => crate::files::UPLOAD_TTL,
        };
        self.files = Some(std::sync::Arc::new(fs));
        Ok(self)
    }

    /// Set the time unassigned uploads stay (seconds), e.g. short in tests.
    pub fn with_upload_ttl(mut self, secs: i64) -> Result<Store> {
        let root = self.files()?.root().to_path_buf();
        let mut fs = FileStore::open(&root)?;
        fs.ttl = secs;
        self.files = Some(std::sync::Arc::new(fs));
        Ok(self)
    }

    pub fn files(&self) -> Result<&FileStore> {
        self.files.as_deref().ok_or_else(|| {
            BaseError::new(ErrorKind::Internal, "upload.no_store", "this server runs without a data directory; operator: set I3CM_DATA_DIR")
        })
    }

    /// Store bytes as content: in the file store, or in the database without one.
    pub(crate) async fn store_bytes<C: GenericClient>(&self, c: &C, data: &[u8]) -> Result<String> {
        match &self.files {
            Some(f) => f.put_bytes(data).await,
            None => {
                let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(data));
                c.execute("INSERT INTO blob (sha256, data) VALUES ($1,$2) ON CONFLICT DO NOTHING", &[&sha, &data]).await.map_err(db)?;
                Ok(sha)
            }
        }
    }

    /// The bytes of a stored content hash: file store first, then the database.
    pub(crate) async fn body_of<C: GenericClient>(&self, c: &C, sha: &str, size: u64) -> Result<ContentBody> {
        if let Some(f) = &self.files {
            let p = f.blob_path(sha);
            if p.is_file() {
                return Ok(ContentBody::File { path: p, size });
            }
        }
        let r = c.query_opt("SELECT data FROM blob WHERE sha256=$1", &[&sha]).await.map_err(db)?;
        if let Some(r) = r {
            return Ok(ContentBody::Bytes(r.get(0)));
        }
        // just assigned: the order committed, the file is linked into the store a moment later
        if let Some(f) = &self.files {
            let ids: Vec<String> = c.query("SELECT id FROM upload WHERE sha256=$1 AND state='consumed'", &[&sha]).await.map_err(db)?.iter().map(|r| r.get(0)).collect();
            for id in ids {
                let p = f.root().join("staging").join(&id).join("data");
                if p.is_file() {
                    return Ok(ContentBody::File { path: p, size });
                }
            }
        }
        Err(BaseError::internal(format!("content {sha} is missing in the file store and the database")))
    }

    fn upload_row_error(state: &str, expired: bool) -> BaseError {
        match state {
            "consumed" => BaseError::conflict("upload.consumed", "this upload was already assigned").hint("upload the file again for another assignment"),
            "expired" => BaseError::invalid("upload.expired", "this upload expired before it was assigned").hint("upload the file again"),
            _ if expired => BaseError::invalid("upload.expired", "this upload expired before it was assigned").hint("upload the file again"),
            "open" => BaseError::invalid("upload.incomplete", "the upload is not complete").hint("send POST /v1/uploads/{id}/complete first"),
            _ => not_found(),
        }
    }

    async fn count_open<C: GenericClient>(&self, c: &C, ctx: &Ctx) -> Result<()> {
        let n: i64 = c
            .query_one("SELECT count(*) FROM upload WHERE tenant_id=$1 AND account=$2 AND state IN ('open','complete') AND expires_at > now()", &[&ctx.tenant, &ctx.me()])
            .await
            .map_err(db)?
            .get(0);
        if n >= MAX_OPEN_UPLOADS {
            return Err(BaseError::conflict("upload.too_many_open", "too many open uploads for this account").hint("assign open uploads in an order or let them expire").with("limit", MAX_OPEN_UPLOADS.to_string()));
        }
        Ok(())
    }

    /// Bytes this account holds in the staging area plus `more` must stay within the limit.
    async fn check_staged<C: GenericClient>(&self, c: &C, ctx: &Ctx, more: u64) -> Result<()> {
        let held: i64 = c
            .query_one(
                "SELECT (coalesce((SELECT sum(p.size) FROM upload_part p JOIN upload u ON u.id=p.upload_id WHERE u.tenant_id=$1 AND u.account=$2 AND u.state='open' AND u.expires_at > now()), 0) \
                 + coalesce((SELECT sum(size) FROM upload WHERE tenant_id=$1 AND account=$2 AND state='complete' AND expires_at > now()), 0))::bigint",
                &[&ctx.tenant, &ctx.me()],
            )
            .await
            .map_err(db)?
            .get(0);
        if held as u64 + more > MAX_STAGED_BYTES {
            return Err(BaseError::conflict("upload.staging_full", "this account holds too many unassigned upload bytes")
                .hint("assign completed uploads in an order or let them expire")
                .with("limit", MAX_STAGED_BYTES.to_string()));
        }
        Ok(())
    }

    /// Serialize the upload limits of one account inside a transaction.
    async fn lock_account<C: GenericClient>(c: &C, ctx: &Ctx) -> Result<()> {
        let key = format!("upload:{}:{}", ctx.tenant, ctx.me());
        c.execute("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", &[&key]).await.map_err(db)?;
        Ok(())
    }

    fn new_upload_id() -> String {
        format!("upl_{}", uuid::Uuid::new_v4().simple())
    }

    /// Open an upload; `sha256` (optional) is checked when it is completed.
    pub async fn open_upload(&self, ctx: &Ctx, sha256: Option<&str>) -> Result<Value> {
        let fs = self.files()?;
        if let Some(s) = sha256
            && !valid_sha(s) {
                return Err(BaseError::invalid("upload.bad_checksum", "sha256 must be 64 lowercase hex digits"));
            }
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db)?;
        Self::lock_account(&*tx, ctx).await?;
        self.count_open(&*tx, ctx).await?;
        let id = Self::new_upload_id();
        let row = tx
            .query_one(
                "INSERT INTO upload (id, tenant_id, account, state, expires_at, expect_sha256) VALUES ($1,$2,$3,'open', now() + make_interval(secs => $4), $5) RETURNING expires_at",
                &[&id, &ctx.tenant, &ctx.me(), &(fs.ttl as f64), &sha256],
            )
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        let exp: chrono::DateTime<chrono::Utc> = row.get(0);
        Ok(json!({"id": id, "state": "open", "expiresAt": exp, "maxPartSize": crate::files::MAX_UPLOAD_PART, "maxSize": MAX_UPLOAD_SIZE}))
    }

    /// The upload row of this account: (state, expired, expect_sha256).
    async fn own_upload<C: GenericClient>(&self, c: &C, ctx: &Ctx, id: &str) -> Result<(String, bool, Option<String>)> {
        let r = c
            .query_opt("SELECT state, expires_at <= now(), expect_sha256 FROM upload WHERE id=$1 AND tenant_id=$2 AND account=$3", &[&id, &ctx.tenant, &ctx.me()])
            .await
            .map_err(db)?
            .ok_or_else(not_found)?;
        Ok((r.get(0), r.get(1), r.get(2)))
    }

    /// Add part `n` of an open upload. Sending the same part again changes nothing; other bytes
    /// under the same number are a conflict.
    pub async fn put_upload_part(&self, ctx: &Ctx, id: &str, n: i32, body: Chunks<'_>) -> Result<Value> {
        let fs = self.files()?;
        if !(1..=MAX_UPLOAD_PARTS).contains(&n) {
            return Err(BaseError::invalid("upload.bad_part", "part numbers run from 1").with("max", MAX_UPLOAD_PARTS.to_string()));
        }
        let c = self.client().await?;
        let (state, expired, _) = self.own_upload(&**c, ctx, id).await?;
        if state != "open" || expired {
            return Err(Self::upload_row_error(&state, expired));
        }
        drop(c);
        let (tmp, sha, size) = fs.write_part(body).await?;
        let res = self.register_part(ctx, id, n, &tmp, &sha, size).await;
        fs.drop_tmp(&tmp);
        res?;
        Ok(json!({"id": id, "part": n, "size": size, "sha256": sha}))
    }

    /// Record a written part under the limits (one transaction, serialized per account): the file
    /// is put in place before the row commits, so a committed part always has its file.
    async fn register_part(&self, ctx: &Ctx, id: &str, n: i32, tmp: &std::path::Path, sha: &str, size: u64) -> Result<()> {
        let fs = self.files()?;
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db)?;
        Self::lock_account(&*tx, ctx).await?;
        let (state, expired): (String, bool) = tx
            .query_opt("SELECT state, expires_at <= now() FROM upload WHERE id=$1 AND tenant_id=$2 AND account=$3 FOR UPDATE", &[&id, &ctx.tenant, &ctx.me()])
            .await
            .map_err(db)?
            .map(|r| (r.get(0), r.get(1)))
            .ok_or_else(not_found)?;
        if state != "open" || expired {
            return Err(Self::upload_row_error(&state, expired));
        }
        if let Some(r) = tx.query_opt("SELECT sha256 FROM upload_part WHERE upload_id=$1 AND n=$2", &[&id, &n]).await.map_err(db)? {
            let had: String = r.get(0);
            if had != sha {
                return Err(BaseError::conflict("upload.part_conflict", "this part was sent before with other bytes").hint("open a new upload to change the file").with("part", n.to_string()));
            }
            return Ok(());
        }
        let others: i64 = tx.query_one("SELECT coalesce(sum(size), 0)::bigint FROM upload_part WHERE upload_id=$1", &[&id]).await.map_err(db)?.get(0);
        if others as u64 + size > MAX_UPLOAD_SIZE {
            return Err(BaseError::invalid("upload.too_large", "the file is larger than allowed").with("limit", MAX_UPLOAD_SIZE.to_string()));
        }
        self.check_staged(&*tx, ctx, size).await?;
        tx.execute("INSERT INTO upload_part (upload_id, n, sha256, size) VALUES ($1,$2,$3,$4)", &[&id, &n, &sha, &(size as i64)]).await.map_err(db)?;
        fs.keep_part(id, n, tmp)?;
        if let Err(e) = tx.commit().await {
            fs.remove_part(id, n);
            return Err(db(e));
        }
        Ok(())
    }

    /// Complete an upload: parts 1..n without gaps, checksum if one was given. The parts are
    /// assembled while the upload is marked `completing`, without an open transaction.
    pub async fn complete_upload(&self, ctx: &Ctx, id: &str, sha256: Option<&str>, parts_expected: Option<i32>) -> Result<Value> {
        let fs = self.files()?;
        if let Some(p) = parts_expected
            && !(1..=MAX_UPLOAD_PARTS).contains(&p) {
                return Err(BaseError::invalid("upload.bad_part", "parts must be between 1 and the highest part number").with("max", MAX_UPLOAD_PARTS.to_string()));
            }
        if let Some(e) = sha256
            && !valid_sha(e) {
                return Err(BaseError::invalid("upload.bad_checksum", "sha256 must be 64 lowercase hex digits"));
            }
        // 1. check and mark `completing` (short transaction)
        let (count, expect, exp) = {
            let mut c = self.client().await?;
            let tx = c.transaction().await.map_err(db)?;
            let r = tx
                .query_opt("SELECT state, expires_at <= now(), expect_sha256, sha256, size, expires_at FROM upload WHERE id=$1 AND tenant_id=$2 AND account=$3 FOR UPDATE", &[&id, &ctx.tenant, &ctx.me()])
                .await
                .map_err(db)?
                .ok_or_else(not_found)?;
            let (state, expired): (String, bool) = (r.get(0), r.get(1));
            let exp: chrono::DateTime<chrono::Utc> = r.get(5);
            if state == "complete" && !expired {
                // completing twice gives the same answer
                let (s, z): (Option<String>, Option<i64>) = (r.get(3), r.get(4));
                return Ok(json!({"id": id, "state": "complete", "sha256": s, "size": z, "expiresAt": exp}));
            }
            if state == "completing" && !expired {
                return Err(BaseError::conflict("upload.completing", "this upload is being completed").hint("ask GET /v1/uploads/{id} in a moment"));
            }
            if state != "open" || expired {
                return Err(Self::upload_row_error(&state, expired));
            }
            let at_open: Option<String> = r.get(2);
            if let (Some(a), Some(b)) = (&at_open, sha256)
                && a != b {
                    return Err(BaseError::invalid("upload.checksum_conflict", "the checksum differs from the one given when the upload was opened").hint("complete without sha256, or open a new upload"));
                }
            let expect = sha256.map(str::to_string).or(at_open);
            let parts: Vec<i32> = tx.query("SELECT n FROM upload_part WHERE upload_id=$1 ORDER BY n", &[&id]).await.map_err(db)?.iter().map(|r| r.get(0)).collect();
            let count = parts.len() as i32;
            let upto = parts_expected.unwrap_or(0).max(parts.last().copied().unwrap_or(0)).max(1);
            if count != upto {
                // parts are sorted and within 1..=MAX_UPLOAD_PARTS: one pass, at most 100 named
                let have: std::collections::HashSet<i32> = parts.iter().copied().collect();
                let missing: Vec<String> = (1..=upto).filter(|n| !have.contains(n)).take(100).map(|n| n.to_string()).collect();
                return Err(BaseError::invalid("upload.incomplete", "parts are missing").hint("send the missing parts, then complete again").with("missing", missing.join(",")));
            }
            tx.execute("UPDATE upload SET state='completing' WHERE id=$1", &[&id]).await.map_err(db)?;
            tx.commit().await.map_err(db)?;
            (count, expect, exp)
        };
        // 2. assemble without a transaction; a failure returns the upload to `open`
        let reopen = |e: BaseError| async move {
            if let Ok(c) = self.client().await {
                let _ = c.execute("UPDATE upload SET state='open' WHERE id=$1 AND state='completing'", &[&id]).await;
            }
            e
        };
        let (sha, size) = match fs.assemble(id, count).await {
            Ok(x) => x,
            Err(e) => return Err(reopen(e).await),
        };
        if let Some(e) = &expect
            && e != &sha {
                // the upload stays open: a missing last part or a wrong part can still be sent
                fs.drop_data(id);
                return Err(reopen(
                    BaseError::invalid("upload.checksum_mismatch", "the assembled file has another SHA-256 than expected")
                        .hint("check for a missing last part or a wrong part; send it (a changed part needs a new upload) and complete again")
                        .with("expected", e.clone())
                        .with("actual", sha),
                )
                .await);
            }
        if size > MAX_UPLOAD_SIZE {
            fs.drop_data(id);
            return Err(reopen(BaseError::invalid("upload.too_large", "the file is larger than allowed").with("limit", MAX_UPLOAD_SIZE.to_string())).await);
        }
        // 3. record (short transaction); parts go only after the state is durably complete
        let mut c = self.client().await?;
        let tx = c.transaction().await.map_err(db)?;
        tx.execute("UPDATE upload SET state='complete', sha256=$2, size=$3 WHERE id=$1 AND state='completing'", &[&id, &sha, &(size as i64)]).await.map_err(db)?;
        tx.execute("DELETE FROM upload_part WHERE upload_id=$1", &[&id]).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        fs.drop_parts(id, count);
        Ok(json!({"id": id, "state": "complete", "sha256": sha, "size": size, "expiresAt": exp}))
    }

    /// Upload a small file in one request: write it (`FileStore::write_single`), then register
    /// it here as a completed upload.
    pub async fn single_upload(&self, ctx: &Ctx, tmp: &std::path::Path, sha: &str, size: u64, expect: Option<&str>) -> Result<Value> {
        let fs = self.files()?;
        if let Some(s) = expect
            && !valid_sha(s) {
                fs.drop_tmp(tmp);
                return Err(BaseError::invalid("upload.bad_checksum", "sha256 must be 64 lowercase hex digits"));
            }
        if let Some(e) = expect
            && e != sha {
                fs.drop_tmp(tmp);
                return Err(BaseError::invalid("upload.checksum_mismatch", "the file has another SHA-256 than expected").with("expected", e).with("actual", sha));
            }
        let res = async {
            let mut c = self.client().await?;
            let tx = c.transaction().await.map_err(db)?;
            Self::lock_account(&*tx, ctx).await?;
            self.count_open(&*tx, ctx).await?;
            self.check_staged(&*tx, ctx, size).await?;
            let id = Self::new_upload_id();
            let row = tx
                .query_one(
                    "INSERT INTO upload (id, tenant_id, account, state, expires_at, sha256, size) VALUES ($1,$2,$3,'complete', now() + make_interval(secs => $4), $5, $6) RETURNING expires_at",
                    &[&id, &ctx.tenant, &ctx.me(), &(fs.ttl as f64), &sha, &(size as i64)],
                )
                .await
                .map_err(db)?;
            // the file is in place before the row commits
            fs.keep_data(&id, tmp)?;
            if let Err(e) = tx.commit().await {
                fs.remove_staging(&id);
                return Err(db(e));
            }
            Ok::<_, BaseError>((id, row.get::<_, chrono::DateTime<chrono::Utc>>(0)))
        }
        .await;
        let (id, exp) = match res {
            Ok(x) => x,
            Err(e) => {
                fs.drop_tmp(tmp);
                return Err(e);
            }
        };
        Ok(json!({"id": id, "state": "complete", "sha256": sha, "size": size, "expiresAt": exp}))
    }

    /// State of an upload of this account.
    pub async fn upload_info(&self, ctx: &Ctx, id: &str) -> Result<Value> {
        let c = self.client().await?;
        let r = c
            .query_opt("SELECT state, expires_at <= now(), sha256, size, expires_at FROM upload WHERE id=$1 AND tenant_id=$2 AND account=$3", &[&id, &ctx.tenant, &ctx.me()])
            .await
            .map_err(db)?
            .ok_or_else(not_found)?;
        let (state, expired): (String, bool) = (r.get(0), r.get(1));
        let state = if expired && matches!(state.as_str(), "open" | "completing" | "complete") { "expired".to_string() } else { state };
        let parts: Vec<Value> = c
            .query("SELECT n, size, sha256 FROM upload_part WHERE upload_id=$1 ORDER BY n", &[&id])
            .await
            .map_err(db)?
            .iter()
            .map(|p| json!({"part": p.get::<_, i32>(0), "size": p.get::<_, i64>(1), "sha256": p.get::<_, String>(2)}))
            .collect();
        let exp: chrono::DateTime<chrono::Utc> = r.get(4);
        Ok(json!({"id": id, "state": state, "sha256": r.get::<_, Option<String>>(2), "size": r.get::<_, Option<i64>>(3), "expiresAt": exp, "parts": parts}))
    }

    /// Assign a completed upload inside an order (transaction `c`, number `txn`): returns its
    /// hash and size and marks it consumed.
    pub(crate) async fn consume_upload<C: GenericClient>(&self, c: &C, ctx: &Ctx, id: &str, txn: i64) -> Result<(String, i64)> {
        let fs = self.files()?;
        let r = c
            .query_opt(
                "UPDATE upload SET state='consumed', consumed_txn=$4, updated_at=now() WHERE id=$1 AND tenant_id=$2 AND account=$3 AND state='complete' AND expires_at > now() RETURNING sha256, size",
                &[&id, &ctx.tenant, &ctx.me(), &txn],
            )
            .await
            .map_err(db)?;
        let Some(r) = r else {
            let (state, expired, _) = self.own_upload(c, ctx, id).await?;
            return Err(Self::upload_row_error(&state, expired));
        };
        let (sha, size): (String, i64) = (r.get(0), r.get(1));
        if !fs.has(&sha) && !fs.has_staged(id) {
            return Err(BaseError::internal(format!("upload {id}: its file is missing in the staging area")));
        }
        Ok((sha, size))
    }

    /// After an order committed: link the files of its assigned uploads into the file store.
    pub(crate) fn promote_uploads(&self, list: &[(String, String)]) {
        let Some(fs) = &self.files else { return };
        for (id, sha) in list {
            if let Err(e) = fs.promote(id, sha) {
                tracing::error!(upload = %id, sha256 = %sha, error = %e.description, "linking an assigned upload into the file store failed");
            }
        }
    }

    /// Expire unassigned uploads, clean the staging area and old idempotent answers. Returns the
    /// number of uploads that expired.
    pub async fn sweep(&self) -> Result<u64> {
        let c = self.client().await?;
        let expired: Vec<String> = c
            .query("UPDATE upload SET state='expired' WHERE state IN ('open','completing','complete') AND expires_at <= now() RETURNING id", &[])
            .await
            .map_err(db)?
            .iter()
            .map(|r| r.get(0))
            .collect();
        c.execute("DELETE FROM upload_part WHERE upload_id IN (SELECT id FROM upload WHERE state IN ('expired','consumed'))", &[]).await.map_err(db)?;
        c.execute("DELETE FROM upload WHERE expires_at < now() - interval '7 days'", &[]).await.map_err(db)?;
        c.execute("DELETE FROM idempotency WHERE created_at < now() - make_interval(secs => $1)", &[&(IDEMPOTENCY_TTL as f64)]).await.map_err(db)?;
        if let Some(fs) = &self.files {
            for id in fs.staged() {
                let row: Option<(String, bool)> = c
                    .query_opt("SELECT state, consumed_txn IS NULL OR updated_at < now() - interval '10 minutes' FROM upload WHERE id=$1", &[&id])
                    .await
                    .map_err(db)?
                    .map(|r| (r.get(0), r.get(1)));
                let remove = match &row {
                    // a directory without a row can be an upload being registered right now
                    None => fs.staging_age(&id).is_some_and(|a| a.as_secs() > 3600),
                    Some((st, _)) if matches!(st.as_str(), "open" | "completing" | "complete") => false,
                    // a consumed upload keeps its staging copy a while: its blob is linked after the
                    // order commits
                    Some((st, old)) if st == "consumed" => *old,
                    Some(_) => true,
                };
                if remove {
                    fs.remove_staging(&id);
                }
            }
            fs.clean_tmp();
        }
        Ok(expired.len() as u64)
    }

    /// Begin an idempotent request: `New` (go on, then `idem_finish` or `idem_abort`) or the
    /// stored answer. The same key with another body, or while the first request still runs,
    /// is a conflict.
    pub async fn idem_begin(&self, ctx: &Ctx, key: &str, scope: &str, body_sha: &str) -> Result<IdemStart> {
        if key.is_empty() || key.len() > 255 || key.chars().any(|ch| ch.is_control()) {
            return Err(BaseError::invalid("idempotency.bad_key", "Idempotency-Key must be 1 to 255 visible characters"));
        }
        let c = self.client().await?;
        // a key older than the retention counts as new
        c.execute(
            "DELETE FROM idempotency WHERE tenant_id=$1 AND account=$2 AND key=$3 AND created_at < now() - make_interval(secs => $4)",
            &[&ctx.tenant, &ctx.me(), &key, &(IDEMPOTENCY_TTL as f64)],
        )
        .await
        .map_err(db)?;
        let mut tries = 0;
        let r = loop {
            let ins = c
                .execute(
                    "INSERT INTO idempotency (tenant_id, account, key, scope, body_sha, state) VALUES ($1,$2,$3,$4,$5,'running') ON CONFLICT DO NOTHING",
                    &[&ctx.tenant, &ctx.me(), &key, &scope, &body_sha],
                )
                .await
                .map_err(db)?;
            if ins == 1 {
                return Ok(IdemStart::New);
            }
            // the row can vanish between insert and select (aborted first request): try again
            match c
                .query_opt("SELECT scope, body_sha, state, status, response FROM idempotency WHERE tenant_id=$1 AND account=$2 AND key=$3", &[&ctx.tenant, &ctx.me(), &key])
                .await
                .map_err(db)?
            {
                Some(r) => break r,
                None if tries < 3 => tries += 1,
                None => return Err(BaseError::conflict("idempotency.in_progress", "this Idempotency-Key is changing state").hint("retry in a moment with the same key")),
            }
        };
        let (sc, bs, st): (String, String, String) = (r.get(0), r.get(1), r.get(2));
        if sc != scope || bs != body_sha {
            return Err(BaseError::conflict("idempotency.conflict", "this Idempotency-Key was used for another request").hint("use a new key for a new request"));
        }
        if st != "done" {
            // a request that ran longer than the slowest request can run was cut off (server
            // restart): its key is taken over by the repeat
            let taken = c
                .execute(
                    "UPDATE idempotency SET created_at=now() WHERE tenant_id=$1 AND account=$2 AND key=$3 AND state='running' AND created_at < now() - interval '15 minutes'",
                    &[&ctx.tenant, &ctx.me(), &key],
                )
                .await
                .map_err(db)?;
            if taken == 1 {
                return Ok(IdemStart::New);
            }
            return Err(BaseError::conflict("idempotency.in_progress", "the first request with this Idempotency-Key is still running").hint("retry in a moment with the same key"));
        }
        let status: i32 = r.get(3);
        Ok(IdemStart::Replay(status as u16, r.get::<_, Option<Value>>(4).unwrap_or(Value::Null)))
    }

    /// Record the answer of an idempotent request.
    pub async fn idem_finish(&self, ctx: &Ctx, key: &str, status: u16, response: &Value) -> Result<()> {
        let c = self.client().await?;
        c.execute(
            "UPDATE idempotency SET state='done', status=$4, response=$5 WHERE tenant_id=$1 AND account=$2 AND key=$3",
            &[&ctx.tenant, &ctx.me(), &key, &(status as i32), response],
        )
        .await
        .map_err(db)?;
        Ok(())
    }

    /// Forget a key whose request failed, so that it can be retried.
    pub async fn idem_abort(&self, ctx: &Ctx, key: &str) -> Result<()> {
        let c = self.client().await?;
        c.execute("DELETE FROM idempotency WHERE tenant_id=$1 AND account=$2 AND key=$3 AND state='running'", &[&ctx.tenant, &ctx.me(), &key]).await.map_err(db)?;
        Ok(())
    }

    /// Move content held in the database to the file store; returns the number of blobs moved.
    pub async fn migrate_blobs(&self) -> Result<u64> {
        let fs = self.files()?;
        let c = self.client().await?;
        let mut moved = 0u64;
        loop {
            let keys: Vec<String> = c.query("SELECT sha256 FROM blob ORDER BY sha256 LIMIT 50", &[]).await.map_err(db)?.iter().map(|r| r.get(0)).collect();
            if keys.is_empty() {
                break;
            }
            for sha in keys {
                // one blob in memory at a time
                let Some(r) = c.query_opt("SELECT data FROM blob WHERE sha256=$1", &[&sha]).await.map_err(db)? else { continue };
                let data: Vec<u8> = r.get(0);
                let got = fs.put_bytes(&data).await?;
                if got != sha {
                    return Err(BaseError::internal(format!("blob {sha} has the hash {got}; left in the database")));
                }
                c.execute("DELETE FROM blob WHERE sha256=$1", &[&sha]).await.map_err(db)?;
                moved += 1;
            }
        }
        Ok(moved)
    }
}
