//! End-to-end tests of the HTTP API against mock AniList and MyAnimeList servers.

use anime_scheduler::{poller, routes, AppState, Config, SharedState};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
    Form, Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Mock {
    /// anilist id -> next airing episode
    next_ep: HashMap<i64, u32>,
    /// MAL PATCH calls: (mal id, form)
    mal_updates: Vec<(i64, HashMap<String, String>)>,
    /// mal id -> (status, watched) already on the user's MAL list
    mal_list: HashMap<i64, (&'static str, u32)>,
    token_calls: usize,
}
type MockState = Arc<Mutex<Mock>>;

fn media(id: i64, next_ep: Option<u32>) -> Value {
    json!({
        "id": id, "idMal": id * 10,
        "title": { "romaji": format!("Show {id}"), "english": null, "userPreferred": format!("Show {id}") },
        "coverImage": { "large": null }, "siteUrl": null,
        "status": if next_ep.is_some() { "RELEASING" } else { "FINISHED" },
        "episodes": 12, "format": "TV", "season": "FALL", "seasonYear": 2026,
        "startDate": { "year": 2026, "month": 10, "day": 1 },
        "nextAiringEpisode": next_ep.map(|e| json!({ "episode": e, "airingAt": 1_900_000_000 })),
    })
}

async fn graphql(State(m): State<MockState>, Json(body): Json<Value>) -> Json<Value> {
    // A little latency so concurrent refreshes really overlap.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let m = m.lock().unwrap();
    let q = body["query"].as_str().unwrap();
    let ids: Vec<i64> = body["variables"]["ids"]
        .as_array()
        .map(|a| a.iter().map(|v| v.as_i64().unwrap()).collect())
        .unwrap_or_default();
    let list: Vec<Value> = if q.contains("search:") {
        vec![media(1, m.next_ep.get(&1).copied())]
    } else if q.contains("idMal_in") {
        ids.iter()
            .filter(|i| *i % 10 == 0 && **i != 990)
            .map(|i| media(i / 10, m.next_ep.get(&(i / 10)).copied()))
            .collect()
    } else {
        ids.iter()
            .map(|i| media(*i, m.next_ep.get(i).copied()))
            .collect()
    };
    Json(json!({ "data": { "Page": { "pageInfo": { "hasNextPage": false }, "media": list } } }))
}

async fn mal_list(Path(user): Path<String>) -> Json<Value> {
    assert_eq!(user, "@me");
    Json(json!({
        "data": [
            { "node": { "id": 20, "title": "Show 2" }, "list_status": { "status": "watching", "num_episodes_watched": 3 } },
            { "node": { "id": 30, "title": "Show 3" }, "list_status": { "status": "completed", "num_episodes_watched": 12 } },
            { "node": { "id": 990, "title": "Obscure" }, "list_status": { "status": "plan_to_watch", "num_episodes_watched": 0 } }
        ],
        "paging": {}
    }))
}

async fn mal_anime(State(m): State<MockState>, Path(id): Path<i64>) -> Json<Value> {
    let m = m.lock().unwrap();
    Json(match m.mal_list.get(&id) {
        Some((status, watched)) => {
            json!({ "id": id, "my_list_status": { "status": status, "num_episodes_watched": watched } })
        }
        None => json!({ "id": id }),
    })
}

async fn mal_patch(
    State(m): State<MockState>,
    Path(id): Path<i64>,
    Form(f): Form<HashMap<String, String>>,
) -> Json<Value> {
    m.lock().unwrap().mal_updates.push((id, f));
    Json(json!({}))
}

async fn mal_token(State(m): State<MockState>) -> Json<Value> {
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut m = m.lock().unwrap();
    m.token_calls += 1;
    Json(
        json!({ "access_token": format!("fresh{}", m.token_calls), "refresh_token": "r2", "expires_in": 3600 }),
    )
}

async fn spawn(router: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    format!("http://{addr}")
}

struct Env {
    app: String,
    state: SharedState,
    mock: MockState,
    /// Sends the CSRF header like the real frontend does.
    c: reqwest::Client,
    _dir: tempfile::TempDir,
}

impl Env {
    async fn get(&self, path: &str) -> Value {
        self.c
            .get(format!("{}{path}", self.app))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn send(&self, method: reqwest::Method, path: &str, body: Value) -> (StatusCode, Value) {
        let r = self
            .c
            .request(method, format!("{}{path}", self.app))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = StatusCode::from_u16(r.status().as_u16()).unwrap();
        (status, r.json().await.unwrap_or(Value::Null))
    }
    async fn post(&self, path: &str, body: Value) -> Value {
        let (status, v) = self.send(reqwest::Method::POST, path, body).await;
        assert!(status.is_success(), "{path}: {status} {v}");
        v
    }
}

/// Start the mocks and the app. `token_expires_at` sets the stored MAL token's expiry.
async fn setup(token_expires_at: &str) -> Env {
    let mock: MockState = Arc::default();
    mock.lock().unwrap().next_ep.insert(1, 4); // episodes 1-3 aired
    let mock_url = spawn(
        Router::new()
            .route("/graphql", post(graphql))
            .route("/v2/users/{user}/animelist", get(mal_list))
            .route("/v2/anime/{id}", get(mal_anime))
            .route("/v2/anime/{id}/my_list_status", patch(mal_patch))
            .route("/v1/oauth2/token", post(mal_token))
            .with_state(mock.clone()),
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("dist")).unwrap();
    std::fs::write(
        dir.path().join("dist/index.html"),
        "<title>Anime Scheduler</title>",
    )
    .unwrap();
    let data_file = dir.path().join("data.json");
    // Pretend we're already logged in to MAL.
    std::fs::write(
        &data_file,
        json!({ "mal": { "access_token": "tok", "refresh_token": "r", "expires_at": token_expires_at, "username": "me" } })
            .to_string(),
    )
    .unwrap();

    let state = AppState::new(Config {
        bind: "127.0.0.1:0".into(),
        public_url: "http://localhost".into(),
        data_file: data_file.to_string_lossy().into(),
        static_dir: dir.path().join("dist").to_string_lossy().into(),
        poll_minutes: 30,
        anilist_url: format!("{mock_url}/graphql"),
        mal_client_id: Some("cid".into()),
        mal_client_secret: None,
        mal_api_base: mock_url.clone(),
        mal_auth_base: mock_url.clone(),
    })
    .unwrap();
    let app = spawn(routes::router(state.clone())).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-requested-with", "anime-scheduler".parse().unwrap());
    let c = reqwest::Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    Env {
        app,
        state,
        mock,
        c,
        _dir: dir,
    }
}

#[tokio::test]
async fn full_flow() {
    let env = setup("2099-01-01T00:00:00Z").await;
    let (app, c, mock) = (&env.app, &env.c, &env.mock);

    // Frontend is served, including for unknown (client-side) paths; unknown API paths 404.
    for path in ["", "/some/page"] {
        let html = c
            .get(format!("{app}{path}"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(html.contains("Anime Scheduler"), "{path}: {html}");
    }
    let r = c.get(format!("{app}/api/nope")).send().await.unwrap();
    assert_eq!(r.status(), 404);

    // Search + add. Not on MAL yet -> added there with the chosen status.
    let res = env.get("/api/search?q=show").await;
    assert_eq!(res[0]["anilist_id"], 1);
    let res = env
        .post(
            "/api/anime",
            json!({ "anilist_id": 1, "status": "planning" }),
        )
        .await;
    assert_eq!(
        res["anime"]["availability"],
        json!({ "kind": "available", "from": 1, "to": 3, "count": 3 })
    );
    assert_eq!(
        mock.lock().unwrap().mal_updates.last().unwrap().1["status"],
        "plan_to_watch"
    );

    // Watch next episode -> moves to watching and syncs to MAL.
    let res = env.post("/api/anime/1/watch", json!({})).await;
    assert_eq!(res["anime"]["watched"], 1);
    assert_eq!(res["anime"]["status"], "watching");
    assert!(res["mal_error"].is_null());
    {
        let m = mock.lock().unwrap();
        let (id, form) = m.mal_updates.last().unwrap();
        assert_eq!(*id, 10);
        assert_eq!(form["num_watched_episodes"], "1");
        assert_eq!(form["status"], "watching");
    }

    // Log an episode with an explicit time; out-of-range episodes are rejected.
    let res = env
        .post(
            "/api/anime/1/watch",
            json!({ "episode": 3, "watched_at": "2026-10-20T19:00:00Z" }),
        )
        .await;
    assert_eq!(res["anime"]["watched"], 3);
    assert_eq!(res["anime"]["availability"]["kind"], "upcoming");
    assert_eq!(res["anime"]["availability"]["episode"], 4);
    assert_eq!(
        res["anime"]["watch_log"][1]["watched_at"],
        "2026-10-20T19:00:00Z"
    );
    let (status, _) = env
        .send(
            reqwest::Method::POST,
            "/api/anime/1/watch",
            json!({ "episode": 13 }),
        )
        .await;
    assert_eq!(status, 400);

    // No notification without new episodes; then two new episodes drop.
    assert_eq!(poller::refresh_all(&env.state).await.unwrap(), 0);
    mock.lock().unwrap().next_ep.insert(1, 6);
    let res = env.post("/api/refresh", json!({})).await;
    assert_eq!(res["new_notifications"], 1);
    let res = env.get("/api/notifications").await;
    assert_eq!(res["unread"], 1);
    assert_eq!(res["notifications"][0]["message"], "Episodes 4–5 are out!");
    env.post("/api/notifications/read", json!({})).await;
    assert_eq!(env.get("/api/notifications").await["unread"], 0);

    // Import from MAL.
    let res = env.post("/api/mal/import", json!({})).await;
    assert_eq!(res["added"], 1, "{res}"); // completed show 3 filtered out by default
    assert_eq!(res["unmatched"], json!(["Obscure"]));
    let list = env.get("/api/anime").await;
    let show2 = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["anilist_id"] == 2)
        .unwrap();
    assert_eq!(show2["watched"], 3);
    assert_eq!(show2["status"], "watching");
    assert_eq!(show2["availability"]["kind"], "available"); // finished show, 4..12

    // Data persisted to disk and reloads.
    let reloaded = anime_scheduler::store::Store::open(&env.state.config.data_file).unwrap();
    assert_eq!(reloaded.read().await.anime.len(), 2);

    // Remove.
    let r = c.delete(format!("{app}/api/anime/2")).send().await.unwrap();
    assert_eq!(r.status(), 204);
}

#[tokio::test]
async fn adding_a_show_keeps_mal_progress() {
    let env = setup("2099-01-01T00:00:00Z").await;
    env.mock
        .lock()
        .unwrap()
        .mal_list
        .insert(20, ("watching", 8));
    let res = env
        .post(
            "/api/anime",
            json!({ "anilist_id": 2, "status": "planning" }),
        )
        .await;
    assert_eq!(res["anime"]["watched"], 8);
    assert_eq!(res["anime"]["status"], "watching");
    assert!(
        env.mock.lock().unwrap().mal_updates.is_empty(),
        "MAL must not be overwritten"
    );

    // Adding twice is rejected.
    let (status, _) = env
        .send(
            reqwest::Method::POST,
            "/api/anime",
            json!({ "anilist_id": 2 }),
        )
        .await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn import_does_not_lower_local_progress() {
    let env = setup("2099-01-01T00:00:00Z").await;
    env.post(
        "/api/anime",
        json!({ "anilist_id": 2, "status": "watching" }),
    )
    .await;
    let (status, _) = env
        .send(
            reqwest::Method::PATCH,
            "/api/anime/2",
            json!({ "watched": 5 }),
        )
        .await;
    assert!(status.is_success());
    let res = env.post("/api/mal/import", json!({})).await; // MAL says 3 watched
    assert_eq!(res["updated"], 1);
    let list = env.get("/api/anime").await;
    assert_eq!(list[0]["watched"], 5);
}

#[tokio::test]
async fn csrf_guard() {
    let env = setup("2099-01-01T00:00:00Z").await;
    let plain = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    // A cross-site "simple" request without our header is refused.
    let r = plain
        .post(format!("{}/api/refresh", env.app))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = plain
        .post(format!("{}/api/mal/disconnect", env.app))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert!(env.state.store.read().await.mal.is_some());
    // Even with the header, a browser-flagged cross-site request is refused.
    let r = env
        .c
        .post(format!("{}/api/refresh", env.app))
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // Reads are fine; starting a login from another site is not.
    assert_eq!(
        plain
            .get(format!("{}/api/anime", env.app))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let r = plain
        .get(format!("{}/auth/mal/login", env.app))
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = plain
        .get(format!("{}/auth/mal/login", env.app))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 303, "same-site login redirects to MAL");
}

#[tokio::test]
async fn expired_token_is_refreshed_once() {
    let env = setup("2000-01-01T00:00:00Z").await;
    env.post("/api/anime", json!({ "anilist_id": 1 })).await;
    env.post("/api/anime", json!({ "anilist_id": 2 })).await;
    // Concurrent syncs that both need a token -> exactly one refresh.
    let (a, b) = tokio::join!(
        env.post("/api/anime/1/watch", json!({})),
        env.post("/api/anime/2/watch", json!({}))
    );
    assert!(
        a["mal_error"].is_null() && b["mal_error"].is_null(),
        "{a} {b}"
    );
    assert_eq!(env.mock.lock().unwrap().token_calls, 1);
    let stored = env.state.store.read().await.mal.clone().unwrap();
    assert_eq!(stored.access_token, "fresh1");
    assert_eq!(stored.refresh_token, "r2");
}

#[tokio::test]
async fn concurrent_refreshes_notify_once() {
    let env = setup("2099-01-01T00:00:00Z").await;
    env.post(
        "/api/anime",
        json!({ "anilist_id": 1, "status": "watching" }),
    )
    .await;
    env.mock.lock().unwrap().next_ep.insert(1, 5);
    let (a, b) = tokio::join!(
        poller::refresh_all(&env.state),
        poller::refresh_all(&env.state)
    );
    assert_eq!(a.unwrap() + b.unwrap(), 1);
    // Stale data later can't make the episode "un-air" and be announced again.
    env.mock.lock().unwrap().next_ep.insert(1, 4);
    assert_eq!(poller::refresh_all(&env.state).await.unwrap(), 0);
    env.mock.lock().unwrap().next_ep.insert(1, 5);
    assert_eq!(poller::refresh_all(&env.state).await.unwrap(), 0);
}

#[tokio::test]
async fn oauth_callback_errors() {
    let env = setup("2099-01-01T00:00:00Z").await;
    let r = env
        .c
        .get(format!(
            "{}/auth/mal/callback?code=x&state=unknown",
            env.app
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert!(r.text().await.unwrap().contains("expired"));

    let r = env
        .c
        .get(format!(
            "{}/auth/mal/callback?error=%3Cscript%3Ealert(1)%3C/script%3E",
            env.app
        ))
        .send()
        .await
        .unwrap();
    let body = r.text().await.unwrap();
    assert!(!body.contains("<script>"), "{body}");
}
