//! HTTP API and static frontend.

use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection, QueryRejection},
        FromRequest, FromRequestParts, Query, Request, State,
    },
    http::{Method, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{any, get, patch, post},
    Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use std::collections::{btree_map::Entry, HashMap};
use std::time::{Duration, Instant};
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    mal::{new_code_verifier, random_token},
    model::{
        AddReq, Anime, AnimeView, ErrorResp, FromMedia, ImportReq, ImportResp, MalStatus,
        NotificationsResp, ReadReq, RefreshResp, SearchResult, UpdateReq, UpdateResp, WatchReq,
        WatchStatus, CSRF_HEADER, CSRF_HEADER_VALUE,
    },
    poller, SharedState,
};

pub fn router(state: SharedState) -> Router {
    let dir = std::path::PathBuf::from(&state.config.static_dir);
    // The WebAssembly frontend (built with `trunk build`) is served from `static_dir`.
    let frontend = ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")));
    Router::new()
        .route("/api/anime", get(list_anime).post(add_anime))
        .route("/api/anime/{id}", patch(update_anime).delete(remove_anime))
        .route("/api/anime/{id}/watch", post(log_watch))
        .route("/api/search", get(search))
        .route("/api/refresh", post(refresh))
        .route(
            "/api/notifications",
            get(list_notifications).delete(clear_notifications),
        )
        .route("/api/notifications/read", post(mark_read))
        .route("/api/mal", get(mal_status))
        .route("/api/mal/import", post(mal_import))
        .route("/api/mal/disconnect", post(mal_disconnect))
        .route("/auth/mal/login", get(mal_login))
        .route("/auth/mal/callback", get(mal_callback))
        .route(
            "/api/{*rest}",
            any(|| async { AppError::not_found("no such API endpoint") }),
        )
        .fallback_service(frontend)
        .layer(middleware::from_fn(csrf_guard))
        .with_state(state)
}

/// How long a started MAL login stays valid.
const OAUTH_TTL: Duration = Duration::from_secs(10 * 60);

/// Blocks cross-site requests that could change state (CSRF): every non-GET request
/// must carry [`CSRF_HEADER`] and not be marked cross-site by the browser, and the MAL
/// login can't be started from another site.
async fn csrf_guard(req: Request, next: Next) -> Response {
    let headers = req.headers();
    let cross_site = matches!(
        headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()),
        Some("cross-site" | "same-site")
    );
    let safe_method = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let has_header = headers
        .get(CSRF_HEADER)
        .is_some_and(|v| v.as_bytes() == CSRF_HEADER_VALUE.as_bytes());
    let blocked = if safe_method {
        cross_site && req.uri().path() == "/auth/mal/login"
    } else {
        cross_site || !has_header
    };
    if blocked {
        return AppError(StatusCode::FORBIDDEN, "cross-site request blocked".into())
            .into_response();
    }
    next.run(req).await
}

// ---------- errors & extractors ----------

/// An API error: an HTTP status plus a message that is safe to show the user.
pub struct AppError(StatusCode, String);

impl AppError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }

    fn not_found(msg: &str) -> Self {
        Self(StatusCode::NOT_FOUND, msg.into())
    }

    fn not_in_list() -> Self {
        Self::not_found("anime not in your list")
    }

    fn already_listed(title: &str) -> Self {
        Self::bad_request(format!("{title} is already in your list"))
    }

    /// A local failure (e.g. the data file can't be written). Details go to the log only.
    fn internal(e: anyhow::Error) -> Self {
        tracing::error!("{e:#}");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not save your data (see the server log)".into(),
        )
    }

    /// AniList or MyAnimeList failed. The top-level message is shown; the cause chain
    /// (which may contain URLs or response details) goes to the log.
    fn upstream(e: anyhow::Error) -> Self {
        tracing::warn!("{e:#}");
        Self(StatusCode::BAD_GATEWAY, e.to_string())
    }

    /// For operations that both call upstream and save (e.g. a refresh).
    fn classify(e: anyhow::Error) -> Self {
        if e.downcast_ref::<std::io::Error>().is_some() {
            Self::internal(e)
        } else {
            Self::upstream(e)
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorResp { error: self.1 })).into_response()
    }
}

macro_rules! rejection_to_app_error {
    ($($rejection:ty),*) => {$(
        impl From<$rejection> for AppError {
            fn from(r: $rejection) -> Self {
                Self(r.status(), r.body_text())
            }
        }
    )*};
}
rejection_to_app_error!(JsonRejection, PathRejection, QueryRejection);

/// `Json` whose rejections use our JSON error shape.
#[derive(FromRequest)]
#[from_request(via(Json), rejection(AppError))]
struct ApiJson<T>(T);

/// `Path` whose rejections use our JSON error shape.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(AppError))]
struct ApiPath<T>(T);

/// `Query` whose rejections use our JSON error shape.
#[derive(FromRequestParts)]
#[from_request(via(Query), rejection(AppError))]
struct ApiQuery<T>(T);

type ApiResult<T> = Result<T, AppError>;

// ---------- list ----------

async fn list_anime(State(s): State<SharedState>) -> Json<Vec<AnimeView>> {
    let now = Utc::now();
    let d = s.store.read().await;
    let list = d.anime.values().cloned().map(|a| AnimeView::new(a, now));
    Json(list.collect())
}

async fn add_anime(
    State(s): State<SharedState>,
    ApiJson(req): ApiJson<AddReq>,
) -> ApiResult<Json<UpdateResp>> {
    if let Some(existing) = s.store.read().await.anime.get(&req.anilist_id) {
        return Err(AppError::already_listed(&existing.title));
    }
    let media = s
        .anilist
        .by_ids(&[req.anilist_id])
        .await
        .map_err(AppError::upstream)?;
    let m = media
        .first()
        .ok_or_else(|| AppError::bad_request("no such anime on AniList"))?;
    let now = Utc::now();
    let mut anime = Anime::from_media(m, req.status, now);
    let mal_error = merge_with_mal(&s, &mut anime).await;

    // Re-check under the write lock: the show may have been added meanwhile.
    let inserted = s
        .store
        .mutate(|d| match d.anime.entry(anime.anilist_id) {
            Entry::Occupied(_) => false,
            Entry::Vacant(v) => {
                v.insert(anime.clone());
                true
            }
        })
        .await
        .map_err(AppError::internal)?;
    if !inserted {
        return Err(AppError::already_listed(&anime.title));
    }
    s.reschedule.notify_one();
    Ok(Json(UpdateResp {
        anime: AnimeView::new(anime, now),
        mal_error,
    }))
}

/// Reconcile a newly added show with the user's MyAnimeList. If MAL already has
/// progress for it, adopt that; otherwise add it to MAL with the status the user picked.
/// Returns a message if MAL couldn't be reached.
async fn merge_with_mal(s: &SharedState, anime: &mut Anime) -> Option<String> {
    let (Some(mal), Some(mal_id)) = (&s.mal, anime.mal_id) else {
        return None;
    };
    let token = match s.mal_token().await {
        Ok(Some(token)) => token,
        Ok(None) => return None, // not connected
        Err(e) => return Some(e.to_string()),
    };
    match mal.my_list_status(&token, mal_id).await {
        Ok(Some((status, watched))) if watched > 0 => {
            anime.import_progress(watched, status.unwrap_or(anime.status));
            None
        }
        Ok(_) => s.sync_to_mal(anime).await,
        Err(e) => Some(format!("couldn't check your MyAnimeList entry: {e}")),
    }
}

async fn update_anime(
    State(s): State<SharedState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(req): ApiJson<UpdateReq>,
) -> ApiResult<Json<UpdateResp>> {
    let now = Utc::now();
    let anime = s
        .store
        .mutate(|d| {
            let a = d.anime.get_mut(&id)?;
            if let Some(w) = req.watched {
                a.set_progress(w, now);
            }
            if let Some(st) = req.status {
                a.status = st;
            }
            Some(a.clone())
        })
        .await
        .map_err(AppError::internal)?
        .ok_or_else(AppError::not_in_list)?;
    respond_after_sync(&s, anime).await
}

async fn log_watch(
    State(s): State<SharedState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(req): ApiJson<WatchReq>,
) -> ApiResult<Json<UpdateResp>> {
    let now = Utc::now();
    let anime = s
        .store
        .mutate(|d| {
            let a = d.anime.get_mut(&id)?;
            let episode = req.episode.unwrap_or(a.watched + 1);
            let logged = a.log_watch(episode, req.watched_at.unwrap_or(now));
            Some(logged.map(|()| a.clone()))
        })
        .await
        .map_err(AppError::internal)?
        .ok_or_else(AppError::not_in_list)?
        .map_err(AppError::bad_request)?;
    respond_after_sync(&s, anime).await
}

/// Push a change to MyAnimeList and build the response (MAL failures don't fail the request).
async fn respond_after_sync(s: &SharedState, anime: Anime) -> ApiResult<Json<UpdateResp>> {
    let mal_error = s.sync_to_mal(&anime).await;
    Ok(Json(UpdateResp {
        anime: AnimeView::new(anime, Utc::now()),
        mal_error,
    }))
}

async fn remove_anime(
    State(s): State<SharedState>,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    let removed = s
        .store
        .mutate(|d| d.anime.remove(&id))
        .await
        .map_err(AppError::internal)?;
    match removed {
        Some(_) => Ok(StatusCode::NO_CONTENT),
        None => Err(AppError::not_in_list()),
    }
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(
    State(s): State<SharedState>,
    ApiQuery(q): ApiQuery<SearchQuery>,
) -> ApiResult<Json<Vec<SearchResult>>> {
    let q = q.q.trim();
    if q.is_empty() {
        return Ok(Json(vec![]));
    }
    let media = s.anilist.search(q).await.map_err(AppError::upstream)?;
    let d = s.store.read().await;
    let results = media.into_iter().map(|m| SearchResult {
        season: m.season_label(),
        anilist_id: m.id,
        title: m.display_title(),
        title_english: m.title.english.clone(),
        cover: m.cover_image.and_then(|c| c.large),
        format: m.format,
        media_status: m.status,
        episodes: m.episodes,
        in_list: d.anime.contains_key(&m.id),
    });
    Ok(Json(results.collect()))
}

async fn refresh(State(s): State<SharedState>) -> ApiResult<Json<RefreshResp>> {
    let created = poller::refresh_all(&s).await.map_err(AppError::classify)?;
    // Let the poller recompute when to wake up against the fresh data.
    s.reschedule.notify_one();
    Ok(Json(RefreshResp {
        new_notifications: created,
    }))
}

// ---------- notifications ----------

async fn list_notifications(State(s): State<SharedState>) -> Json<NotificationsResp> {
    let d = s.store.read().await;
    let notifications: Vec<_> = d.notifications.iter().rev().cloned().collect();
    let unread = notifications.iter().filter(|n| !n.read).count();
    Json(NotificationsResp {
        unread,
        notifications,
    })
}

async fn mark_read(
    State(s): State<SharedState>,
    ApiJson(req): ApiJson<ReadReq>,
) -> ApiResult<StatusCode> {
    s.store
        .mutate(|d| {
            for n in &mut d.notifications {
                if req.ids.as_ref().is_none_or(|ids| ids.contains(&n.id)) {
                    n.read = true;
                }
            }
        })
        .await
        .map_err(AppError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_notifications(State(s): State<SharedState>) -> ApiResult<StatusCode> {
    s.store
        .mutate(|d| d.notifications.clear())
        .await
        .map_err(AppError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- MyAnimeList ----------

fn mal_not_configured() -> AppError {
    AppError::bad_request("MyAnimeList is not configured (set MAL_CLIENT_ID)")
}

async fn mal_status(State(s): State<SharedState>) -> Json<MalStatus> {
    let d = s.store.read().await;
    Json(MalStatus {
        configured: s.mal.is_some(),
        connected: d.mal.is_some(),
        username: d.mal.as_ref().and_then(|t| t.username.clone()),
        redirect_uri: s.config.mal_redirect_uri(),
    })
}

async fn mal_import(
    State(s): State<SharedState>,
    ApiJson(req): ApiJson<ImportReq>,
) -> ApiResult<Json<ImportResp>> {
    let mal = s.mal.as_ref().ok_or_else(mal_not_configured)?;
    let username = req
        .username
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    // Your own list needs a working login; a public list can be read without one.
    let (user, token) = match username {
        Some(u) => (u.to_string(), s.mal_token().await.ok().flatten()),
        None => match s.mal_token().await.map_err(AppError::upstream)? {
            Some(token) => ("@me".to_string(), Some(token)),
            None => {
                return Err(AppError::bad_request(
                    "enter a MyAnimeList username or connect your account",
                ))
            }
        },
    };
    let statuses = req.statuses.unwrap_or_else(|| {
        WatchStatus::ALL
            .into_iter()
            .filter(|s| s.is_active())
            .collect()
    });

    let entries: Vec<_> = mal
        .user_list(&user, token.as_deref())
        .await
        .map_err(AppError::upstream)?
        .into_iter()
        .filter(|e| e.status.is_some_and(|st| statuses.contains(&st)))
        .collect();
    let mal_ids: Vec<i64> = entries.iter().map(|e| e.mal_id).collect();
    let media = s
        .anilist
        .by_mal_ids(&mal_ids)
        .await
        .map_err(AppError::upstream)?;
    let by_mal: HashMap<i64, _> = media.iter().filter_map(|m| Some((m.id_mal?, m))).collect();

    let now = Utc::now();
    let resp = s
        .store
        .mutate(|d| {
            let mut resp = ImportResp {
                added: 0,
                updated: 0,
                unmatched: vec![],
            };
            for e in &entries {
                let Some(m) = by_mal.get(&e.mal_id) else {
                    resp.unmatched.push(e.title.clone());
                    continue;
                };
                let status = e.status.unwrap_or(WatchStatus::Planning);
                let anime = match d.anime.entry(m.id) {
                    Entry::Occupied(o) => {
                        resp.updated += 1;
                        o.into_mut()
                    }
                    Entry::Vacant(v) => {
                        resp.added += 1;
                        v.insert(Anime::from_media(m, status, now))
                    }
                };
                anime.mal_id = Some(e.mal_id);
                anime.import_progress(e.watched, status);
            }
            resp
        })
        .await
        .map_err(AppError::internal)?;
    s.reschedule.notify_one();
    Ok(Json(resp))
}

async fn mal_disconnect(State(s): State<SharedState>) -> ApiResult<StatusCode> {
    s.store
        .mutate(|d| d.mal = None)
        .await
        .map_err(AppError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mal_login(State(s): State<SharedState>) -> ApiResult<Redirect> {
    let mal = s.mal.as_ref().ok_or_else(mal_not_configured)?;
    let verifier = new_code_verifier();
    let state = random_token(32);
    let url = mal
        .authorize_url(&verifier, &state, &s.config.mal_redirect_uri())
        .map_err(AppError::internal)?;

    let mut pending = s.pending_oauth.lock().await;
    pending.retain(|_, (_, created)| created.elapsed() < OAUTH_TTL);
    // Bound memory if someone hammers this endpoint: drop the oldest pending login.
    if pending.len() >= 50 {
        let oldest = pending
            .iter()
            .min_by_key(|(_, (_, created))| *created)
            .map(|(k, _)| k.clone());
        if let Some(oldest) = oldest {
            pending.remove(&oldest);
        }
    }
    pending.insert(state, (verifier, Instant::now()));
    Ok(Redirect::to(&url))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn mal_callback(State(s): State<SharedState>, Query(q): Query<CallbackQuery>) -> Response {
    match finish_mal_login(&s, q).await {
        Ok(()) => Redirect::to("/?mal=connected").into_response(),
        Err(e) => {
            tracing::warn!("MAL login failed: {e:#}");
            let page = format!(
                "<h1>MyAnimeList login failed</h1><p>{}</p><p><a href=\"/\">Back</a></p>",
                html_escape(&e.to_string())
            );
            (StatusCode::BAD_REQUEST, Html(page)).into_response()
        }
    }
}

async fn finish_mal_login(s: &SharedState, q: CallbackQuery) -> anyhow::Result<()> {
    if let Some(err) = q.error {
        anyhow::bail!("MyAnimeList denied access: {err}");
    }
    let mal = s
        .mal
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("MyAnimeList is not configured"))?;
    let (Some(code), Some(state)) = (q.code, q.state) else {
        anyhow::bail!("missing code or state");
    };
    let verifier = s
        .pending_oauth
        .lock()
        .await
        .remove(&state)
        .filter(|(_, created)| created.elapsed() < OAUTH_TTL)
        .map(|(verifier, _)| verifier)
        .ok_or_else(|| anyhow::anyhow!("login session expired, please try again"))?;
    let mut tokens = mal
        .exchange_code(&code, &verifier, &s.config.mal_redirect_uri())
        .await?;
    tokens.username = mal.me(&tokens.access_token).await.ok();
    s.store.mutate(|d| d.mal = Some(tokens)).await?;
    Ok(())
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}
