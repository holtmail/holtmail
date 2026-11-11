use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use holtmail_blob::BlobId;
use holtmail_core::{DeviceScope, UserId};
use holtmail_mailview::{MailViewError, Viewer};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Semaphore;

use crate::auth::{self, Authed};
use crate::error::{MethodError, Problem};
use crate::request::Ctx;
use crate::{Inner, body, ids};

/// How long an upload stays usable by its uploader.
pub(crate) const UPLOAD_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

struct Upload {
    at: Instant,
    kind: String,
    size: u64,
}

/// Blobs uploaded per user, usable by that user only for [`UPLOAD_LIFETIME`]. The registry is
/// the only reference to an upload: an expired one is deleted from the upload store unless
/// another live upload holds the same bytes.
#[derive(Default)]
pub(crate) struct Uploads {
    blobs: Mutex<HashMap<(UserId, BlobId), Upload>>,
    slots: Mutex<HashMap<UserId, Arc<Semaphore>>>,
}

impl Uploads {
    /// Drops expired uploads and returns the blobs no live upload holds any more.
    fn expire(blobs: &mut HashMap<(UserId, BlobId), Upload>) -> Vec<BlobId> {
        let mut gone: Vec<BlobId> = Vec::new();
        blobs.retain(|(_, blob), u| {
            let live = u.at.elapsed() < UPLOAD_LIFETIME;
            if !live {
                gone.push(*blob);
            }
            live
        });
        gone.retain(|blob| !blobs.keys().any(|(_, b)| b == blob));
        gone.sort();
        gone.dedup();
        gone
    }

    /// Whether `user` may keep `size` more bytes of uploads within `budget`.
    fn fits(&self, user: UserId, size: u64, budget: u64) -> bool {
        let blobs = self.blobs.lock().expect("uploads");
        let held: u64 = blobs
            .iter()
            .filter(|((u, _), up)| *u == user && up.at.elapsed() < UPLOAD_LIFETIME)
            .map(|(_, up)| up.size)
            .sum();
        held.saturating_add(size) <= budget
    }

    fn record(&self, user: UserId, blob: BlobId, kind: String, size: u64) -> Vec<BlobId> {
        let mut blobs = self.blobs.lock().expect("uploads");
        blobs.insert(
            (user, blob),
            Upload {
                at: Instant::now(),
                kind,
                size,
            },
        );
        Self::expire(&mut blobs)
    }

    /// Drops expired uploads, returning the blobs to delete from the upload store.
    pub(crate) fn sweep(&self) -> Vec<BlobId> {
        Self::expire(&mut self.blobs.lock().expect("uploads"))
    }

    fn kind(&self, user: UserId, blob: &BlobId) -> Option<String> {
        self.blobs
            .lock()
            .expect("uploads")
            .get(&(user, *blob))
            .filter(|u| u.at.elapsed() < UPLOAD_LIFETIME)
            .map(|u| u.kind.clone())
    }
}

/// Deletes expired uploads from the upload store.
pub(crate) async fn delete_expired(inner: &Inner, gone: Vec<BlobId>) {
    for blob in gone {
        if let Err(error) = inner.services.blobs.delete(&blob).await {
            tracing::warn!(%error, "expired upload not deleted");
        }
    }
}

/// The bytes of a blob the viewer may use: an upload of theirs, an email's raw form, or a
/// decoded part of an email. `None` when there is no such blob for them.
pub(crate) async fn fetch(
    inner: &Inner,
    viewer: &Viewer,
    id: &str,
) -> Result<Option<(Bytes, Option<String>)>, MethodError> {
    let view = &inner.services.view;
    match ids::parse_blob(id) {
        None => Ok(None),
        Some(ids::Blob::Hash(blob)) => {
            if let Some(kind) = inner.uploads.kind(viewer.user, &blob) {
                return match inner.services.blobs.get(&blob).await {
                    Ok(bytes) => Ok(bytes.map(|b| (b, Some(kind)))),
                    Err(error) => {
                        tracing::warn!(%error, "upload unreadable");
                        Err(MethodError::new("serverFail"))
                    }
                };
            }
            match view.blob(viewer, &blob).await {
                Ok(bytes) => Ok(Some((bytes, Some("message/rfc822".into())))),
                Err(MailViewError::NotFound(_)) => Ok(None),
                Err(error) => Err(error.into()),
            }
        }
        Some(ids::Blob::Part { email, part }) => {
            let raw = match view.raw(viewer, &email).await {
                Ok(raw) => raw,
                Err(MailViewError::NotFound(_)) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let root = view.structure(viewer, &email).await?;
            let Some(found) = body::find(&root, &part) else {
                return Ok(None);
            };
            Ok(Some((
                Bytes::from(body::decoded(&raw, found)),
                Some(found.content_type.clone()),
            )))
        }
    }
}

pub(crate) async fn resolve(
    ctx: &Ctx<'_>,
    viewer: &Viewer,
    id: &str,
) -> Result<Option<Bytes>, MethodError> {
    Ok(fetch(ctx.inner, viewer, id).await?.map(|(b, _)| b))
}

#[derive(Deserialize)]
pub(crate) struct DownloadQuery {
    accept: Option<String>,
}

async fn signed_in(inner: &Inner, parts: &Parts) -> Result<Authed, Box<Response>> {
    auth::authenticate(inner, &parts.headers, auth::peer(&parts.extensions)).await
}

fn not_found() -> Response {
    Problem {
        status: StatusCode::NOT_FOUND,
        kind: "about:blank",
        detail: "no such blob".into(),
        limit: None,
    }
    .into_response()
}

fn safe_type(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 255
        && kind.contains('/')
        && kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/.+-_;= ".contains(&b))
}

pub(crate) async fn download(
    State(inner): State<Arc<Inner>>,
    Path((account, blob, name)): Path<(String, String, String)>,
    Query(query): Query<DownloadQuery>,
    request: Request,
) -> Response {
    let (parts, _) = request.into_parts();
    let authed = match signed_in(&inner, &parts).await {
        Ok(authed) => authed,
        Err(response) => return *response,
    };
    let Some(archive) = ids::parse_account(&account).filter(|a| authed.has(*a)) else {
        return not_found();
    };
    let viewer = authed.viewer(archive);
    let (bytes, kind) = match fetch(&inner, &viewer, &blob).await {
        Ok(Some(found)) => found,
        Ok(None) => return not_found(),
        Err(error) => {
            return Problem {
                status: StatusCode::SERVICE_UNAVAILABLE,
                kind: "about:blank",
                detail: error.description.unwrap_or_else(|| error.kind.into()),
                limit: None,
            }
            .into_response();
        }
    };
    let kind = query
        .accept
        .filter(|a| safe_type(a))
        .or(kind.filter(|k| safe_type(k)))
        .unwrap_or_else(|| "application/octet-stream".into());
    let mut response = (StatusCode::OK, bytes).into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&kind) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    let filename: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, immutable, max-age=31536000"),
    );
    response
}

pub(crate) async fn upload(
    State(inner): State<Arc<Inner>>,
    Path(account): Path<String>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let authed = match signed_in(&inner, &parts).await {
        Ok(authed) => authed,
        Err(response) => return *response,
    };
    let Some(archive) = ids::parse_account(&account).filter(|a| authed.has(*a)) else {
        return Problem {
            status: StatusCode::NOT_FOUND,
            kind: "about:blank",
            detail: "no such account".into(),
            limit: None,
        }
        .into_response();
    };
    let scopes = &authed.principal.scopes;
    if !scopes.contains(&DeviceScope::WriteMail) && !scopes.contains(&DeviceScope::SendMail) {
        return Problem {
            status: StatusCode::FORBIDDEN,
            kind: "about:blank",
            detail: "uploads need a device that may write or send mail".into(),
            limit: None,
        }
        .into_response();
    }
    let semaphore = inner
        .uploads
        .slots
        .lock()
        .expect("upload slots")
        .entry(authed.principal.user)
        .or_insert_with(|| {
            Arc::new(Semaphore::new(
                inner.settings.limits.max_concurrent_upload as usize,
            ))
        })
        .clone();
    let Ok(_slot) = semaphore.try_acquire_owned() else {
        return Problem::limit("maxConcurrentUpload", "too many uploads at once").into_response();
    };
    let viewer = authed.viewer(archive);
    let limit = max_upload(&inner, &viewer).await;
    let kind = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|k| safe_type(k))
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = match axum::body::to_bytes(body, limit as usize).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Problem::limit("maxSizeUpload", "the upload is too large").into_response();
        }
    };
    let size = bytes.len();
    let budget = inner.settings.limits.upload_budget;
    if !inner
        .uploads
        .fits(authed.principal.user, size as u64, budget)
    {
        return Problem::limit(
            "maxSizeUpload",
            "this user's uploads of the last day are over the upload budget",
        )
        .into_response();
    }
    let blob = match inner.services.blobs.put(bytes).await {
        Ok(blob) => blob,
        Err(error) => {
            tracing::warn!(%error, "upload not stored");
            return Problem {
                status: StatusCode::SERVICE_UNAVAILABLE,
                kind: "about:blank",
                detail: "the upload could not be stored".into(),
                limit: None,
            }
            .into_response();
        }
    };
    let gone = inner
        .uploads
        .record(authed.principal.user, blob, kind.clone(), size as u64);
    delete_expired(&inner, gone).await;
    (
        StatusCode::CREATED,
        Json(json!({
            "accountId": account,
            "blobId": ids::blob(&blob),
            "type": kind,
            "size": size,
        })),
    )
        .into_response()
}

/// The largest upload for the viewer: the configured limit, or less when the mail view takes
/// less.
pub(crate) async fn max_upload(inner: &Inner, viewer: &Viewer) -> u64 {
    let configured = inner.settings.limits.max_size_upload;
    match inner.services.actions.append_limit(viewer).await {
        Ok(limit) if limit > 0 => configured.min(limit),
        _ => configured,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_uploads_are_deleted_unless_another_holds_the_bytes() {
        let uploads = Uploads::default();
        let (ada, bob) = (UserId::new(), UserId::new());
        let (shared, own) = (BlobId::of(b"shared upload"), BlobId::of(b"own upload"));
        let old = Instant::now()
            .checked_sub(UPLOAD_LIFETIME + Duration::from_secs(1))
            .expect("an instant a day ago");
        {
            let mut blobs = uploads.blobs.lock().unwrap();
            for (user, blob) in [(ada, shared), (ada, own)] {
                blobs.insert(
                    (user, blob),
                    Upload {
                        at: old,
                        kind: "text/plain".into(),
                        size: 1,
                    },
                );
            }
        }
        assert!(
            uploads.fits(ada, 10, 10),
            "expired uploads leave the budget"
        );
        let gone = uploads.record(bob, shared, "text/plain".into(), 1);
        assert_eq!(gone, [own], "bytes another live upload holds stay");
        assert!(uploads.kind(ada, &shared).is_none());
        assert!(uploads.kind(bob, &shared).is_some());
        assert!(uploads.sweep().is_empty());
    }
}
