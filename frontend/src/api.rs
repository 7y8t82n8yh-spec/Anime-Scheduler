//! Typed client for the server's JSON API.

use anime_shared::*;
use gloo_net::http::{Request, RequestBuilder, Response};
use serde::{de::DeserializeOwned, Serialize};

pub type ApiResult<T> = Result<T, String>;

/// Send a request (without a body) and fail on non-2xx responses.
async fn send(req: RequestBuilder) -> ApiResult<Response> {
    check(csrf(req).send().await).await
}

/// Send a request with a JSON body and fail on non-2xx responses.
async fn send_json(req: RequestBuilder, body: &impl Serialize) -> ApiResult<Response> {
    let req = csrf(req).json(body).map_err(|e| e.to_string())?;
    check(req.send().await).await
}

/// Decode a JSON response body.
async fn read<T: DeserializeOwned>(resp: Response) -> ApiResult<T> {
    resp.json().await.map_err(|e| format!("bad response: {e}"))
}

/// The server rejects state-changing requests without this header (CSRF protection).
fn csrf(req: RequestBuilder) -> RequestBuilder {
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

pub async fn list_anime() -> ApiResult<Vec<AnimeView>> {
    read(send(Request::get("/api/anime")).await?).await
}

pub async fn add_anime(anilist_id: i64, status: WatchStatus) -> ApiResult<UpdateResp> {
    let body = AddReq { anilist_id, status };
    read(send_json(Request::post("/api/anime"), &body).await?).await
}

pub async fn update_anime(id: i64, req: UpdateReq) -> ApiResult<UpdateResp> {
    read(send_json(Request::patch(&format!("/api/anime/{id}")), &req).await?).await
}

pub async fn log_watch(id: i64, req: WatchReq) -> ApiResult<UpdateResp> {
    read(send_json(Request::post(&format!("/api/anime/{id}/watch")), &req).await?).await
}

pub async fn remove_anime(id: i64) -> ApiResult<()> {
    send(Request::delete(&format!("/api/anime/{id}")))
        .await
        .map(drop)
}

pub async fn search(q: &str) -> ApiResult<Vec<SearchResult>> {
    let q = String::from(js_sys::encode_uri_component(q));
    read(send(Request::get(&format!("/api/search?q={q}"))).await?).await
}

pub async fn refresh() -> ApiResult<RefreshResp> {
    read(send(Request::post("/api/refresh")).await?).await
}

pub async fn notifications() -> ApiResult<NotificationsResp> {
    read(send(Request::get("/api/notifications")).await?).await
}

pub async fn mark_all_read() -> ApiResult<()> {
    send_json(
        Request::post("/api/notifications/read"),
        &ReadReq::default(),
    )
    .await
    .map(drop)
}

pub async fn clear_notifications() -> ApiResult<()> {
    send(Request::delete("/api/notifications")).await.map(drop)
}

pub async fn mal_status() -> ApiResult<MalStatus> {
    read(send(Request::get("/api/mal")).await?).await
}

pub async fn mal_import(req: ImportReq) -> ApiResult<ImportResp> {
    read(send_json(Request::post("/api/mal/import"), &req).await?).await
}

pub async fn mal_disconnect() -> ApiResult<()> {
    send(Request::post("/api/mal/disconnect")).await.map(drop)
}
