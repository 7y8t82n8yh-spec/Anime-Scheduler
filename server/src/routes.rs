//! HTTP API and static frontend.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, patch, post},
    Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use std::collections::HashMap;
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    mal::new_code_verifier,
    model::{
        AddReq, Anime, AnimeView, ErrorResp, FromMedia, ImportReq, ImportResp, MalStatus,
        NotificationsResp, ReadReq, RefreshResp, SearchResult, UpdateReq, UpdateResp, WatchReq,
        WatchStatus,
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
        .fallback_service(frontend)
        .with_state(state)
}

pub struct AppError(StatusCode, String);

impl AppError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    fn not_found() -> Self {
        Self(StatusCode::NOT_FOUND, "anime not in your list".into())
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        Self(StatusCode::BAD_GATEWAY, format!("{:#}", e.into()))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorResp { error: self.1 })).into_response()
    }
}

type ApiResult<T> = Result<T, AppError>;

async fn list_anime(State(s): State<SharedState>) -> Json<Vec<AnimeView>> {
    let now = Utc::now();
    let d = s.store.read().await;
    Json(
        d.anime
            .values()
            .cloned()
            .map(|a| AnimeView::new(a, now))
            .collect(),
    )
}

async fn add_anime(
    State(s): State<SharedState>,
    Json(req): Json<AddReq>,
) -> ApiResult<Json<UpdateResp>> {
    if let Some(existing) = s.store.read().await.anime.get(&req.anilist_id) {
        return Err(AppError::bad_request(format!(
            "{} is already in your list",
            existing.title
        )));
    }
    let media = s.anilist.by_ids(&[req.anilist_id]).await?;
    let m = media
        .first()
        .ok_or_else(|| AppError::bad_request("no such anime on AniList"))?;
    let now = Utc::now();
    let anime = Anime::from_media(m, req.status, now);
    s.store
        .mutate(|d| d.anime.insert(anime.anilist_id, anime.clone()))
        .await?;
    let mal_error = s.sync_to_mal(&anime).await;
    Ok(Json(UpdateResp {
        anime: AnimeView::new(anime, now),
        mal_error,
    }))
}

async fn update_anime(
    State(s): State<SharedState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateReq>,
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
        .await?
        .ok_or_else(AppError::not_found)?;
    let mal_error = s.sync_to_mal(&anime).await;
    Ok(Json(UpdateResp {
        anime: AnimeView::new(anime, now),
        mal_error,
    }))
}

async fn log_watch(
    State(s): State<SharedState>,
    Path(id): Path<i64>,
    body: Option<Json<WatchReq>>,
) -> ApiResult<Json<UpdateResp>> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let now = Utc::now();
    let anime = s
        .store
        .mutate(|d| {
            let a = d.anime.get_mut(&id)?;
            let ep = req.episode.unwrap_or(a.watched + 1);
            a.log_watch(ep, req.watched_at.unwrap_or(now));
            Some(a.clone())
        })
        .await?
        .ok_or_else(AppError::not_found)?;
    let mal_error = s.sync_to_mal(&anime).await;
    Ok(Json(UpdateResp {
        anime: AnimeView::new(anime, now),
        mal_error,
    }))
}

async fn remove_anime(State(s): State<SharedState>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let removed = s.store.mutate(|d| d.anime.remove(&id)).await?;
    Ok(if removed.is_some() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    })
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(
    State(s): State<SharedState>,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<Vec<SearchResult>>> {
    let q = q.q.trim();
    if q.is_empty() {
        return Ok(Json(vec![]));
    }
    let media = s.anilist.search(q).await?;
    let d = s.store.read().await;
    Ok(Json(
        media
            .into_iter()
            .map(|m| SearchResult {
                season: m.season_label(),
                anilist_id: m.id,
                title: m.display_title(),
                title_english: m.title.english.clone(),
                cover: m.cover_image.and_then(|c| c.large),
                format: m.format,
                media_status: m.status,
                episodes: m.episodes,
                in_list: d.anime.contains_key(&m.id),
            })
            .collect(),
    ))
}

async fn refresh(State(s): State<SharedState>) -> ApiResult<Json<RefreshResp>> {
    let created = poller::refresh_all(&s).await?;
    // Let the poller recompute its schedule against the fresh data.
    s.refresh_now.notify_one();
    Ok(Json(RefreshResp {
        new_notifications: created,
    }))
}

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
    Json(req): Json<ReadReq>,
) -> ApiResult<StatusCode> {
    s.store
        .mutate(|d| {
            for n in d.notifications.iter_mut() {
                if req.ids.as_ref().is_none_or(|ids| ids.contains(&n.id)) {
                    n.read = true;
                }
            }
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_notifications(State(s): State<SharedState>) -> ApiResult<StatusCode> {
    s.store.mutate(|d| d.notifications.clear()).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- MyAnimeList ----------

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
    Json(req): Json<ImportReq>,
) -> ApiResult<Json<ImportResp>> {
    let mal = s.mal.as_ref().ok_or_else(|| {
        AppError::bad_request("MyAnimeList is not configured (set MAL_CLIENT_ID)")
    })?;
    let token = s.mal_token().await?;
    let user = match req
        .username
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        Some(u) => u.to_string(),
        None if token.is_some() => "@me".to_string(),
        None => {
            return Err(AppError::bad_request(
                "enter a MyAnimeList username or connect your account",
            ))
        }
    };
    let statuses = req.statuses.unwrap_or_else(|| {
        vec![
            WatchStatus::Watching,
            WatchStatus::Planning,
            WatchStatus::Paused,
        ]
    });

    let entries: Vec<_> = mal
        .user_list(&user, token.as_deref())
        .await?
        .into_iter()
        .filter(|e| e.status.is_some_and(|st| statuses.contains(&st)))
        .collect();
    let mal_ids: Vec<i64> = entries.iter().map(|e| e.mal_id).collect();
    let media = s.anilist.by_mal_ids(&mal_ids).await?;
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
                let a = match d.anime.get_mut(&m.id) {
                    Some(a) => {
                        resp.updated += 1;
                        a
                    }
                    None => {
                        resp.added += 1;
                        d.anime
                            .entry(m.id)
                            .or_insert_with(|| Anime::from_media(m, status, now))
                    }
                };
                // MAL is the source of truth on import; don't invent watch timestamps.
                a.status = status;
                a.mal_id = Some(e.mal_id);
                a.watched = e.watched;
                a.watch_log.retain(|w| w.episode <= e.watched);
            }
            resp
        })
        .await?;
    Ok(Json(resp))
}

async fn mal_disconnect(State(s): State<SharedState>) -> ApiResult<StatusCode> {
    s.store.mutate(|d| d.mal = None).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mal_login(State(s): State<SharedState>) -> ApiResult<Redirect> {
    let mal = s.mal.as_ref().ok_or_else(|| {
        AppError::bad_request("MyAnimeList is not configured (set MAL_CLIENT_ID)")
    })?;
    let verifier = new_code_verifier();
    let state: String = new_code_verifier().chars().take(32).collect();
    let url = mal.authorize_url(&verifier, &state, &s.config.mal_redirect_uri())?;
    let mut pending = s.pending_oauth.lock().await;
    if pending.len() > 20 {
        pending.clear();
    }
    pending.insert(state, verifier);
    Ok(Redirect::to(&url))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn mal_callback(State(s): State<SharedState>, Query(q): Query<CallbackQuery>) -> Response {
    let result: anyhow::Result<()> = async {
        if let Some(err) = q.error {
            anyhow::bail!("MyAnimeList denied access: {err}");
        }
        let mal = s
            .mal
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MyAnimeList is not configured"))?;
        let (Some(code), Some(state)) = (q.code, q.state) else {
            anyhow::bail!("missing code or state")
        };
        let verifier = s
            .pending_oauth
            .lock()
            .await
            .remove(&state)
            .ok_or_else(|| anyhow::anyhow!("login session expired, please try again"))?;
        let mut tokens = mal
            .exchange_code(&code, &verifier, &s.config.mal_redirect_uri())
            .await?;
        tokens.username = mal.me(&tokens.access_token).await.ok();
        s.store.mutate(|d| d.mal = Some(tokens)).await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => Redirect::to("/?mal=connected").into_response(),
        Err(e) => {
            tracing::warn!("MAL login failed: {e:#}");
            let msg: String = format!("{e:#}")
                .chars()
                .map(|c| if "<>&\"".contains(c) { ' ' } else { c })
                .collect();
            (
                StatusCode::BAD_REQUEST,
                Html(format!(
                    "<h1>MyAnimeList login failed</h1><p>{msg}</p><p><a href=\"/\">Back</a></p>"
                )),
            )
                .into_response()
        }
    }
}
