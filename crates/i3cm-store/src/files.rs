//! File store: binary content addressed by SHA-256 as files on the data volume, plus the staging
//! area of uploads (one directory per upload with its parts and the assembled file).
//!
//! Layout below the root (`I3CM_DATA_DIR`):
//! * `blobs/<aa>/<bb>/<sha256>`: content, written once, never changed;
//! * `staging/<upload>/part-<n>`: parts of an open upload;
//! * `staging/<upload>/data`: the assembled file of a completed upload;
//! * `tmp/`: files being written (renamed into place when complete).

use i3cm_core::{BaseError, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Largest part of an upload (also the largest single-request upload).
pub const MAX_UPLOAD_PART: u64 = 64 << 20;
/// Largest file.
pub const MAX_UPLOAD_SIZE: u64 = 16 << 30;
/// Time an upload may stay unassigned before it expires (seconds; operator: `I3CM_UPLOAD_TTL`).
pub const UPLOAD_TTL: i64 = 24 * 3600;
/// Open (not yet assigned) uploads per account.
pub const MAX_OPEN_UPLOADS: i64 = 100;
/// Bytes one account may hold in the staging area (open and completed, unassigned uploads).
pub const MAX_STAGED_BYTES: u64 = 64 << 30;
/// Largest content given as base64 inside an order sent by a client (transition to handles).
pub const MAX_INLINE_CONTENT: usize = 1 << 20;
/// Time a stored idempotent answer is kept (seconds).
pub const IDEMPOTENCY_TTL: i64 = 24 * 3600;

fn io(what: &str, e: std::io::Error) -> BaseError {
    BaseError::internal(format!("file store: {what}: {e}"))
}

#[derive(Debug)]
pub struct FileStore {
    root: PathBuf,
    /// Seconds an upload may stay unassigned.
    pub ttl: i64,
}

/// A source of bytes being written: an async reader of chunks.
pub type Chunks<'a> = std::pin::Pin<Box<dyn futures_core::Stream<Item = std::result::Result<bytes::Bytes, std::io::Error>> + Send + 'a>>;

impl FileStore {
    /// Open the store below `root`; creates the directories and checks that they are writable.
    pub fn open(root: &Path) -> Result<FileStore> {
        for d in ["blobs", "staging", "tmp"] {
            std::fs::create_dir_all(root.join(d)).map_err(|e| io(&format!("create {}", root.join(d).display()), e))?;
        }
        let probe = root.join("tmp").join(format!(".probe-{}", uuid::Uuid::new_v4()));
        std::fs::write(&probe, b"").map_err(|e| io(&format!("{} is not writable", root.display()), e))?;
        let _ = std::fs::remove_file(&probe);
        Ok(FileStore { root: root.to_path_buf(), ttl: UPLOAD_TTL })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn blob_path(&self, sha: &str) -> PathBuf {
        let (a, b) = (sha.get(0..2).unwrap_or("00"), sha.get(2..4).unwrap_or("00"));
        self.root.join("blobs").join(a).join(b).join(sha)
    }

    pub fn has(&self, sha: &str) -> bool {
        self.blob_path(sha).is_file()
    }

    fn tmp(&self) -> PathBuf {
        self.root.join("tmp").join(uuid::Uuid::new_v4().to_string())
    }

    fn staging(&self, upload: &str) -> PathBuf {
        self.root.join("staging").join(upload)
    }

    /// Move a finished temporary file to the blob of its hash (kept if the blob exists).
    fn settle(&self, tmp: &Path, sha: &str) -> Result<()> {
        let dst = self.blob_path(sha);
        if dst.is_file() {
            let _ = std::fs::remove_file(tmp);
            return Ok(());
        }
        std::fs::create_dir_all(dst.parent().expect("blob dir")).map_err(|e| io("create blob dir", e))?;
        std::fs::rename(tmp, &dst).map_err(|e| io("store blob", e))
    }

    /// Store bytes; returns their SHA-256.
    pub async fn put_bytes(&self, data: &[u8]) -> Result<String> {
        let sha = hex::encode(Sha256::digest(data));
        if self.has(&sha) {
            return Ok(sha);
        }
        let tmp = self.tmp();
        let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| io("create", e))?;
        f.write_all(data).await.map_err(|e| io("write", e))?;
        f.sync_all().await.map_err(|e| io("sync", e))?;
        drop(f);
        self.settle(&tmp, &sha)?;
        Ok(sha)
    }

    /// Write a stream to a temporary file, at most `limit` bytes; returns path, SHA-256 and size.
    async fn write_stream(&self, mut body: Chunks<'_>, limit: u64, too_large: &str) -> Result<(PathBuf, String, u64)> {
        use futures_util::StreamExt;
        let tmp = self.tmp();
        let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| io("create", e))?;
        let mut h = Sha256::new();
        let mut size: u64 = 0;
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return Err(BaseError::invalid("upload.interrupted", "the body ended before it was complete").hint("send the same part again").with("detail", e.to_string()));
                }
            };
            size += chunk.len() as u64;
            if size > limit {
                drop(f);
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(BaseError::invalid(too_large, "the body is larger than allowed").hint("split the file into parts of at most this size (PUT /v1/uploads/{id}/parts/{n})").with("limit", limit.to_string()));
            }
            h.update(&chunk);
            f.write_all(&chunk).await.map_err(|e| io("write", e))?;
        }
        f.sync_all().await.map_err(|e| io("sync", e))?;
        Ok((tmp, hex::encode(h.finalize()), size))
    }

    /// Store a stream as a blob; returns its SHA-256 and size.
    pub async fn put_stream(&self, body: Chunks<'_>, limit: u64) -> Result<(String, u64)> {
        let (tmp, sha, size) = self.write_stream(body, limit, "content.too_large").await?;
        self.settle(&tmp, &sha)?;
        Ok((sha, size))
    }

    /// Write a part of an upload to a temporary file; returns path, SHA-256 and size. The caller
    /// compares with an existing part before keeping it (see `Store::put_upload_part`).
    pub async fn write_part(&self, body: Chunks<'_>) -> Result<(PathBuf, String, u64)> {
        self.write_stream(body, MAX_UPLOAD_PART, "upload.part_too_large").await
    }

    /// Put a written part in place.
    pub fn keep_part(&self, upload: &str, n: i32, tmp: &Path) -> Result<()> {
        let dir = self.staging(upload);
        std::fs::create_dir_all(&dir).map_err(|e| io("create staging", e))?;
        std::fs::rename(tmp, dir.join(format!("part-{n}"))).map_err(|e| io("keep part", e))
    }

    pub fn drop_tmp(&self, tmp: &Path) {
        let _ = std::fs::remove_file(tmp);
    }

    /// Place a whole file (single-request upload) as the assembled file of an upload.
    pub fn keep_data(&self, upload: &str, tmp: &Path) -> Result<()> {
        let dir = self.staging(upload);
        std::fs::create_dir_all(&dir).map_err(|e| io("create staging", e))?;
        std::fs::rename(tmp, dir.join("data")).map_err(|e| io("keep upload", e))
    }

    /// Write a whole file (single-request upload) to a temporary file.
    pub async fn write_single(&self, body: Chunks<'_>) -> Result<(PathBuf, String, u64)> {
        self.write_stream(body, MAX_UPLOAD_PART, "upload.part_too_large").await
    }

    /// Concatenate parts 1..=count into the assembled file; returns SHA-256 and size. The parts
    /// stay until `drop_parts` (a failed checksum keeps the upload open).
    pub async fn assemble(&self, upload: &str, count: i32) -> Result<(String, u64)> {
        let dir = self.staging(upload);
        let tmp = self.tmp();
        let mut out = tokio::fs::File::create(&tmp).await.map_err(|e| io("create", e))?;
        let mut h = Sha256::new();
        let mut size: u64 = 0;
        let mut buf = vec![0u8; 1 << 20];
        for n in 1..=count {
            let mut f = tokio::fs::File::open(dir.join(format!("part-{n}"))).await.map_err(|e| io("open part", e))?;
            loop {
                let k = f.read(&mut buf).await.map_err(|e| io("read part", e))?;
                if k == 0 {
                    break;
                }
                h.update(&buf[..k]);
                out.write_all(&buf[..k]).await.map_err(|e| io("write", e))?;
                size += k as u64;
            }
        }
        out.sync_all().await.map_err(|e| io("sync", e))?;
        drop(out);
        std::fs::rename(&tmp, dir.join("data")).map_err(|e| io("assemble", e))?;
        Ok((hex::encode(h.finalize()), size))
    }

    /// Remove the parts of an upload (after it was assembled).
    pub fn drop_parts(&self, upload: &str, count: i32) {
        let dir = self.staging(upload);
        for n in 1..=count {
            let _ = std::fs::remove_file(dir.join(format!("part-{n}")));
        }
    }

    /// Remove the assembled file of an upload (failed checksum).
    pub fn drop_data(&self, upload: &str) {
        let _ = std::fs::remove_file(self.staging(upload).join("data"));
    }

    /// Make the assembled file of an upload the blob of `sha` (hard link; the staging copy is
    /// removed later by the sweep, so a rolled back order can use the upload again).
    pub fn promote(&self, upload: &str, sha: &str) -> Result<()> {
        let src = self.staging(upload).join("data");
        let dst = self.blob_path(sha);
        if dst.is_file() {
            return Ok(());
        }
        std::fs::create_dir_all(dst.parent().expect("blob dir")).map_err(|e| io("create blob dir", e))?;
        if std::fs::hard_link(&src, &dst).is_err() {
            let tmp = self.tmp();
            std::fs::copy(&src, &tmp).map_err(|e| io("copy upload", e))?;
            std::fs::rename(&tmp, &dst).map_err(|e| io("store blob", e))?;
        }
        Ok(())
    }

    /// Remove the staging directory of an upload.
    pub fn remove_staging(&self, upload: &str) {
        let _ = std::fs::remove_dir_all(self.staging(upload));
    }

    /// Upload ids that have a staging directory.
    pub fn staged(&self) -> Vec<String> {
        std::fs::read_dir(self.root.join("staging"))
            .map(|d| d.filter_map(|e| e.ok()).filter_map(|e| e.file_name().into_string().ok()).collect())
            .unwrap_or_default()
    }

    /// Remove temporary files older than an hour (interrupted writes).
    pub fn clean_tmp(&self) {
        if let Ok(d) = std::fs::read_dir(self.root.join("tmp")) {
            for e in d.filter_map(|e| e.ok()) {
                let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|a| a.as_secs() > 3600).unwrap_or(false);
                if old {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    /// Read a whole blob.
    pub async fn read(&self, sha: &str) -> Result<Vec<u8>> {
        tokio::fs::read(self.blob_path(sha)).await.map_err(|e| io("read blob", e))
    }
}
