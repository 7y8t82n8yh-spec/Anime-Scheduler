//! End-to-end test of the HTTP API against mock AniList and MyAnimeList servers.

use anime_scheduler::{poller, routes, AppState, Config};
use axum::{
    extract::{Path, State},
    routing::{get, patch, post},
    Form, Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Mock {
    /// anilist id -> next airing episode
    next_ep: HashMap<i64, u32>,
    /// MAL PATCH calls: (mal id, form)
    mal_updates: Vec<(i64, HashMap<String, String>)>,
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

async fn mal_patch(
    State(m): State<MockState>,
    Path(id): Path<i64>,
    Form(f): Form<HashMap<String, String>>,
) -> Json<Value> {
    m.lock().unwrap().mal_updates.push((id, f));
    Json(json!({}))
}

async fn spawn(router: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn full_flow() {
    let mock: MockState = Arc::default();
    mock.lock().unwrap().next_ep.insert(1, 4); // episodes 1-3 aired
    let mock_url = spawn(
        Router::new()
            .route("/graphql", post(graphql))
            .route("/v2/users/{user}/animelist", get(mal_list))
            .route("/v2/anime/{id}/my_list_status", patch(mal_patch))
            .with_state(mock.clone()),
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let data_file = dir.path().join("data.json");
    // Pretend we're already logged in to MAL.
    std::fs::write(
        &data_file,
        json!({ "mal": { "access_token": "tok", "refresh_token": "r", "expires_at": "2099-01-01T00:00:00Z", "username": "me" } })
            .to_string(),
    )
    .unwrap();

    let state = AppState::new(Config {
        bind: "127.0.0.1:0".into(),
        public_url: "http://localhost".into(),
        data_file: data_file.to_string_lossy().into(),
        poll_minutes: 30,
        anilist_url: format!("{mock_url}/graphql"),
        mal_client_id: Some("cid".into()),
        mal_client_secret: None,
        mal_api_base: mock_url.clone(),
        mal_auth_base: mock_url.clone(),
    })
    .unwrap();
    let app = spawn(routes::router(state.clone())).await;
    let c = reqwest::Client::new();

    // Frontend is served.
    let html = c.get(&app).send().await.unwrap().text().await.unwrap();
    assert!(html.contains("Anime Scheduler"));

    // Search + add.
    let res: Value = c
        .get(format!("{app}/api/search?q=show"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res[0]["anilist_id"], 1);
    let res: Value = c
        .post(format!("{app}/api/anime"))
        .json(&json!({ "anilist_id": 1, "status": "planning" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        res["anime"]["availability"],
        json!({ "kind": "available", "from": 1, "to": 3, "count": 3 })
    );

    // Watch next episode -> moves to watching and syncs to MAL.
    let res: Value = c
        .post(format!("{app}/api/anime/1/watch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
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

    // Log an episode with an explicit time.
    let res: Value = c
        .post(format!("{app}/api/anime/1/watch"))
        .json(&json!({ "episode": 3, "watched_at": "2026-10-20T19:00:00Z" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["anime"]["watched"], 3);
    assert_eq!(res["anime"]["availability"]["kind"], "upcoming");
    assert_eq!(res["anime"]["availability"]["episode"], 4);
    assert_eq!(
        res["anime"]["watch_log"][1]["watched_at"],
        "2026-10-20T19:00:00Z"
    );

    // No notification without new episodes; then two new episodes drop.
    assert_eq!(poller::refresh_all(&state).await.unwrap(), 0);
    mock.lock().unwrap().next_ep.insert(1, 6);
    let res: Value = c
        .post(format!("{app}/api/refresh"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["new_notifications"], 1);
    let res: Value = c
        .get(format!("{app}/api/notifications"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["unread"], 1);
    assert_eq!(res["notifications"][0]["message"], "Episodes 4–5 are out!");
    c.post(format!("{app}/api/notifications/read"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let res: Value = c
        .get(format!("{app}/api/notifications"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["unread"], 0);

    // Import from MAL.
    let res: Value = c
        .post(format!("{app}/api/mal/import"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["added"], 1, "{res}"); // completed show 3 filtered out by default
    assert_eq!(res["unmatched"], json!(["Obscure"]));
    let list: Value = c
        .get(format!("{app}/api/anime"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
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
    let reloaded = anime_scheduler::store::Store::open(&data_file).unwrap();
    assert_eq!(reloaded.read().await.anime.len(), 2);

    // Remove.
    let r = c.delete(format!("{app}/api/anime/2")).send().await.unwrap();
    assert_eq!(r.status(), 204);
}
