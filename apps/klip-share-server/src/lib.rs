//! Scout's self-hostable share server (M11.6, ADR 0021). Scout uploads a clip with an
//! upload token and copies the link; the link opens a player page whose Open Graph tags
//! let Discord play the clip inline. Links are unlisted: 128-bit random ids, no index,
//! `noindex` everywhere. The protocol is in `klip-share`.
//!
//! Hostile input: uploads stream to `tmp/` under a size cap and an idle timeout, count
//! against the quota before they're kept, and must be an MP4 with a video track
//! (`klip_mp4::probe`, which reads boxes only); posters are sniffed by their first
//! bytes; ids are checked before they name a folder; titles are cleaned and escaped.

pub mod config;
pub mod page;
pub mod store;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, Request, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware::{Next, from_fn};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use futures_util::StreamExt;
use klip_share::{
    ApiError, MAX_POSTER_BYTES, PATH_CLIPS, PATH_INFO, ServerInfo, UploadQuery, Uploaded,
    clean_title, hash_token, poster_kind, same_secret,
};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::config::Config;
use crate::store::{Meta, Store, StoreError, now_unix, write_atomic};

/// How often expired clips are removed.
const SWEEP_EVERY: Duration = Duration::from_secs(3600);
const SECONDS_PER_DAY: u64 = 86_400;
/// A refused upload's body is read up to this much before the answer, so a small
/// upload's sender sees the answer rather than a reset connection.
const DRAIN_CAP: u64 = 1 << 20;

/// What every handler shares.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub store: Arc<Store>,
    uploads: Arc<Semaphore>,
}

impl AppState {
    /// Open the data folder for `cfg` (checked, `public_url` included).
    pub fn new(cfg: Config) -> Result<AppState, StoreError> {
        let store = Store::open(&cfg.data_dir)?;
        Ok(AppState {
            uploads: Arc::new(Semaphore::new(cfg.max_concurrent_uploads)),
            cfg: Arc::new(cfg),
            store: Arc::new(store),
        })
    }
}

fn err(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiError {
            error: code.into(),
            message: message.into(),
        }),
    )
        .into_response()
}

fn unauthorized() -> Response {
    err(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "the token is missing or wrong",
    )
}

fn not_found() -> Response {
    err(StatusCode::NOT_FOUND, "not_found", "no such clip")
}

fn internal(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "request failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "the server couldn't do that",
    )
}

fn too_big(max: u64) -> Response {
    err(
        StatusCode::PAYLOAD_TOO_LARGE,
        "too_big",
        format!(
            "uploads on this server are at most {} MB",
            max.checked_div(1_000_000).unwrap_or(0)
        ),
    )
}

/// The `Authorization: Bearer …` token.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.len() <= 200)
}

fn manage_ok(headers: &HeaderMap, meta: &Meta) -> bool {
    bearer(headers).is_some_and(|t| same_secret(&hash_token(t), &meta.manage_hash))
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'none'; media-src 'self'; img-src 'self'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    res
}

/// All routes.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/robots.txt", get(robots))
        .route("/healthz", get(|| async { "ok" }))
        .route(PATH_INFO, get(info))
        .route(PATH_CLIPS, post(upload).layer(DefaultBodyLimit::disable()))
        .route("/api/v1/clips/{id}", delete(delete_clip))
        .route(
            "/api/v1/clips/{id}/poster",
            put(set_poster).layer(DefaultBodyLimit::max(MAX_POSTER_BYTES)),
        )
        .route("/c/{file}", get(public))
        .fallback(|| async { not_found() })
        .layer(from_fn(security_headers))
        .with_state(state)
}

/// Serve on `listener` until `shutdown` resolves; removes expired clips every hour.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let store = state.store.clone();
    let sweeper = tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            let s = store.clone();
            let n = tokio::task::spawn_blocking(move || s.sweep(now_unix()))
                .await
                .unwrap_or(0);
            if n > 0 {
                tracing::info!(n, "expired clips removed");
            }
        }
    });
    let served = axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await;
    sweeper.abort();
    served
}

async fn home(State(s): State<AppState>) -> Html<String> {
    Html(page::home(&s.cfg.site_name))
}

/// Discord's link preview may read clips; nothing else is invited.
async fn robots() -> &'static str {
    "User-agent: Discordbot\nAllow: /c/\n\nUser-agent: *\nDisallow: /\n"
}

async fn info(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let Some(name) = bearer(&headers).and_then(|t| s.store.check_token(t)) else {
        return unauthorized();
    };
    Json(ServerInfo {
        server: "scout-share".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        max_upload_bytes: s.cfg.max_upload_bytes(),
        quota_bytes: s.cfg.quota_bytes(),
        used_bytes: s.store.used(),
        max_expiry_days: s.cfg.max_expiry_days,
        default_expiry_days: s.cfg.default_expiry_days,
        token_name: name,
    })
    .into_response()
}

/// Answer `res` to an upload after reading (and dropping) the start of its body.
async fn refuse(body: Body, res: Response) -> Response {
    let mut stream = body.into_data_stream();
    let mut seen: u64 = 0;
    while let Ok(Some(Ok(chunk))) =
        tokio::time::timeout(Duration::from_secs(2), stream.next()).await
    {
        seen = seen.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if seen > DRAIN_CAP {
            break;
        }
    }
    res
}

async fn upload(State(s): State<AppState>, headers: HeaderMap, uri: Uri, body: Body) -> Response {
    let Some(uploader) = bearer(&headers).and_then(|t| s.store.check_token(t)) else {
        return refuse(body, unauthorized()).await;
    };
    let q = match Query::<UploadQuery>::try_from_uri(&uri) {
        Ok(Query(q)) => q,
        Err(_) => {
            let res = err(
                StatusCode::BAD_REQUEST,
                "bad_query",
                "the query takes title and expires_days (a whole number)",
            );
            return refuse(body, res).await;
        }
    };
    let days = q.expires_days.unwrap_or(s.cfg.default_expiry_days);
    let max_days = s.cfg.max_expiry_days;
    if max_days > 0 && (days == 0 || days > max_days) {
        let res = err(
            StatusCode::BAD_REQUEST,
            "bad_expiry",
            format!("links on this server last 1 to {max_days} days"),
        );
        return refuse(body, res).await;
    }
    let max = s.cfg.max_upload_bytes();
    let declared = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > max) {
        return too_big(max);
    }
    if declared == Some(0) {
        return err(StatusCode::BAD_REQUEST, "empty", "the upload is empty");
    }
    let Ok(_permit) = s.uploads.clone().try_acquire_owned() else {
        let res = err(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            "too many uploads at once; try again in a moment",
        );
        return refuse(body, res).await;
    };
    let hold = declared.unwrap_or(max);
    if !s.store.reserve(hold, s.cfg.quota_bytes()) {
        let res = err(
            StatusCode::INSUFFICIENT_STORAGE,
            "quota",
            "the server has no room for this clip",
        );
        return refuse(body, res).await;
    }
    let (tmp, written) = match receive(&s, body, hold, max).await {
        Ok(r) => r,
        Err(res) => {
            s.store.release(hold);
            return *res;
        }
    };
    s.store.release(hold.saturating_sub(written));
    let expires_unix = if days == 0 {
        0
    } else {
        now_unix().saturating_add(u64::from(days).saturating_mul(SECONDS_PER_DAY))
    };
    let title = clean_title(q.title.as_deref().unwrap_or(""));
    let store = s.store.clone();
    let tmp_for_err = tmp.clone();
    let kept = tokio::task::spawn_blocking(move || {
        keep(&store, &tmp, written, title, expires_unix, uploader)
    })
    .await;
    let (meta, manage) = match kept {
        Ok(Ok(r)) => r,
        Ok(Err(res)) => {
            s.store.release(written);
            return *res;
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_for_err);
            s.store.release(written);
            return internal(e);
        }
    };
    tracing::info!(id = %meta.id, bytes = meta.bytes, uploader = %meta.uploader, "clip stored");
    let url = format!(
        "{}{}/{}",
        s.cfg.public_url,
        klip_share::PATH_PUBLIC,
        meta.id
    );
    (
        StatusCode::CREATED,
        Json(Uploaded {
            video_url: format!("{url}.mp4"),
            url,
            id: meta.id,
            manage_token: manage,
            expires_unix: meta.expires_unix,
            bytes: meta.bytes,
        }),
    )
        .into_response()
}

/// Stream the body to a new file in `tmp/`, at most `limit` bytes, giving up when
/// nothing comes for `upload_idle_secs`.
async fn receive(
    s: &AppState,
    body: Body,
    limit: u64,
    max: u64,
) -> Result<(PathBuf, u64), Box<Response>> {
    let tmp = s.store.tmp_path().map_err(|e| Box::new(internal(e)))?;
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| Box::new(internal(e)))?;
    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;
    let outcome = loop {
        let next = match tokio::time::timeout(s.cfg.upload_idle(), stream.next()).await {
            Ok(n) => n,
            Err(_) => {
                break Err(err(
                    StatusCode::REQUEST_TIMEOUT,
                    "timeout",
                    "the upload stopped sending",
                ));
            }
        };
        let chunk = match next {
            None => break Ok(()),
            Some(Err(_)) => {
                break Err(err(
                    StatusCode::BAD_REQUEST,
                    "broken",
                    "the upload broke off",
                ));
            }
            Some(Ok(c)) => c,
        };
        written = written.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if written > limit {
            break Err(too_big(max));
        }
        if let Err(e) = file.write_all(&chunk).await {
            break Err(internal(e));
        }
    };
    let outcome = match outcome {
        Ok(()) if written == 0 => Err(err(StatusCode::BAD_REQUEST, "empty", "the upload is empty")),
        Ok(()) => file.sync_all().await.map_err(internal),
        Err(e) => Err(e),
    };
    drop(file);
    match outcome {
        Ok(()) => Ok((tmp, written)),
        Err(res) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(Box::new(res))
        }
    }
}

/// Check the received file is an MP4 with a video track and keep it as a new clip.
fn keep(
    store: &Store,
    tmp: &Path,
    bytes: u64,
    title: String,
    expires_unix: u64,
    uploader: String,
) -> Result<(Meta, String), Box<Response>> {
    let fail = |res: Response| {
        let _ = std::fs::remove_file(tmp);
        Box::new(res)
    };
    let probed = std::fs::File::open(tmp)
        .map_err(|e| e.to_string())
        .and_then(|mut f| klip_mp4::probe(&mut f).map_err(|e| e.to_string()));
    let Some((video, duration_ms)) = probed
        .ok()
        .and_then(|i| i.video.map(|v| (v, i.duration_ms.unwrap_or(0))))
    else {
        return Err(fail(err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "not_mp4",
            "that isn't an MP4 video",
        )));
    };
    let (id, manage) = match (klip_share::new_id(), klip_share::new_manage_token()) {
        (Ok(i), Ok(m)) => (i, m),
        (Err(e), _) | (_, Err(e)) => return Err(fail(internal(e))),
    };
    let meta = Meta {
        id,
        title,
        created_unix: now_unix(),
        expires_unix,
        bytes,
        width: video.width,
        height: video.height,
        duration_ms,
        codec: video.codec,
        poster: None,
        manage_hash: hash_token(&manage),
        uploader,
    };
    store.commit(tmp, &meta).map_err(|e| fail(internal(e)))?;
    Ok((meta, manage))
}

async fn set_poster(
    State(s): State<AppState>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(meta) = s.store.read_meta(&id) else {
        return not_found();
    };
    if !manage_ok(&headers, &meta) {
        return unauthorized();
    }
    let Some((ext, _)) = poster_kind(&body) else {
        return err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "not_picture",
            "the poster must be a JPEG, PNG or WebP picture",
        );
    };
    let store = s.store.clone();
    let done = tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
        let path = store
            .poster_path(&meta.id, ext)
            .ok_or_else(|| StoreError::Bad("bad id".into()))?;
        write_atomic(&path, &body)?;
        if let Some(old) = meta.poster.as_deref().filter(|o| *o != ext)
            && let Some(p) = store.poster_path(&meta.id, old)
        {
            let _ = std::fs::remove_file(p);
        }
        store.write_meta(&Meta {
            poster: Some(ext.into()),
            ..meta
        })
    })
    .await;
    match done {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

async fn delete_clip(
    State(s): State<AppState>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    let Some(meta) = s.store.read_meta(&id) else {
        return not_found();
    };
    if !manage_ok(&headers, &meta) {
        return unauthorized();
    }
    let store = s.store.clone();
    match tokio::task::spawn_blocking(move || store.delete(&meta.id)).await {
        Ok(true) => {
            tracing::info!(%id, "clip deleted");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found(),
        Err(e) => internal(e),
    }
}

enum Want {
    Page,
    Video,
    Poster(&'static str),
}

/// `/c/{id}`, `/c/{id}.mp4`, `/c/{id}.jpg|png|webp`.
async fn public(
    State(s): State<AppState>,
    UrlPath(file): UrlPath<String>,
    req: Request,
) -> Response {
    let (id, want) = if let Some(id) = file.strip_suffix(".mp4") {
        (id, Want::Video)
    } else if let Some((id, ext)) = ["jpg", "png", "webp"]
        .into_iter()
        .find_map(|e| file.strip_suffix(&format!(".{e}")).map(|id| (id, e)))
    {
        (id, Want::Poster(ext))
    } else {
        (file.as_str(), Want::Page)
    };
    let Some(meta) = s.store.read_meta(id) else {
        return not_found();
    };
    if meta.expired(now_unix()) {
        return not_found();
    }
    match want {
        Want::Page => (
            [(CACHE_CONTROL, "no-cache")],
            Html(page::render(&meta, &s.cfg.public_url, &s.cfg.site_name)),
        )
            .into_response(),
        Want::Video => match s.store.video_path(&meta.id) {
            Some(p) => serve_file(p, "video/mp4", req).await,
            None => not_found(),
        },
        Want::Poster(ext) if meta.poster.as_deref() == Some(ext) => {
            match (s.store.poster_path(&meta.id, ext), poster_mime(ext)) {
                (Some(p), Some(mime)) => serve_file(p, mime, req).await,
                _ => not_found(),
            }
        }
        Want::Poster(_) => not_found(),
    }
}

fn poster_mime(ext: &str) -> Option<&'static str> {
    match ext {
        "jpg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// A file with Range and conditional requests (`ServeFile`), its type set by us.
async fn serve_file(path: PathBuf, mime: &'static str, req: Request) -> Response {
    let res = match ServeFile::new(path).oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let mut res = res.map(Body::new);
    if res.status().is_success() {
        res.headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(mime));
        res.headers_mut().insert(
            CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=300"),
        );
    }
    res
}
