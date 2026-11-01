//! Typed client for the server's JSON API.

use anime_shared::*;
use gloo_net::http::{Request, RequestBuilder, Response};
use serde::{de::DeserializeOwned, Serialize};

pub type ApiResult<T> = Result<T, String>;

/// The server rejects state-changing requests without this header (CSRF protection).
fn with_csrf(req: RequestBuilder) -> RequestBuilder {
    req.header(CSRF_HEADER, CSRF_HEADER_VALUE)
}

async fn check(resp: Result<Response, gloo_net::Error>) -> ApiResult<Response> {
    let resp = resp.map_err(|e| format!("network error: {e}"))?;
    if resp.ok() {
        return Ok(resp);
    }
    let status = resp.status();
    Err(match resp.json::<ErrorResp>().await {
        Ok(e) => e.error,
        Err(_) => format!("HTTP {status}"),
    })
}

async fn json<T: DeserializeOwned>(req: RequestBuilder) -> ApiResult<T> {
    let req = with_csrf(req);
    check(req.send().await)
        .await?
        .json()
        .await
        .map_err(|e| format!("bad response: {e}"))
}

async fn json_body<B: Serialize, T: DeserializeOwned>(
    req: RequestBuilder,
    body: &B,
) -> ApiResult<T> {
    let req = with_csrf(req).json(body).map_err(|e| e.to_string())?;
    check(req.send().await)
        .await?
        .json()
        .await
        .map_err(|e| format!("bad response: {e}"))
}

async fn empty(req: RequestBuilder) -> ApiResult<()> {
    let req = with_csrf(req);
    check(req.send().await).await.map(|_| ())
}

async fn empty_body<B: Serialize>(req: RequestBuilder, body: &B) -> ApiResult<()> {
    let req = with_csrf(req).json(body).map_err(|e| e.to_string())?;
    check(req.send().await).await.map(|_| ())
}

pub async fn list_anime() -> ApiResult<Vec<AnimeView>> {
    json(Request::get("/api/anime")).await
}

pub async fn add_anime(anilist_id: i64, status: WatchStatus) -> ApiResult<UpdateResp> {
    json_body(Request::post("/api/anime"), &AddReq { anilist_id, status }).await
}

pub async fn update_anime(id: i64, req: UpdateReq) -> ApiResult<UpdateResp> {
    json_body(Request::patch(&format!("/api/anime/{id}")), &req).await
}

pub async fn log_watch(id: i64, req: WatchReq) -> ApiResult<UpdateResp> {
    json_body(Request::post(&format!("/api/anime/{id}/watch")), &req).await
}

pub async fn remove_anime(id: i64) -> ApiResult<()> {
    empty(Request::delete(&format!("/api/anime/{id}"))).await
}

pub async fn search(q: &str) -> ApiResult<Vec<SearchResult>> {
    let q = String::from(js_sys::encode_uri_component(q));
    json(Request::get(&format!("/api/search?q={q}"))).await
}

pub async fn refresh() -> ApiResult<RefreshResp> {
    json(Request::post("/api/refresh")).await
}

pub async fn notifications() -> ApiResult<NotificationsResp> {
    json(Request::get("/api/notifications")).await
}

pub async fn mark_all_read() -> ApiResult<()> {
    empty_body(
        Request::post("/api/notifications/read"),
        &ReadReq::default(),
    )
    .await
}

pub async fn clear_notifications() -> ApiResult<()> {
    empty(Request::delete("/api/notifications")).await
}

pub async fn mal_status() -> ApiResult<MalStatus> {
    json(Request::get("/api/mal")).await
}

pub async fn mal_import(req: ImportReq) -> ApiResult<ImportResp> {
    json_body(Request::post("/api/mal/import"), &req).await
}

pub async fn mal_disconnect() -> ApiResult<()> {
    empty(Request::post("/api/mal/disconnect")).await
}
