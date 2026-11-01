pub mod anilist;
pub mod mal;
pub mod model;
pub mod poller;
pub mod routes;
pub mod store;

use anyhow::Result;
use chrono::{Duration, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, Notify};

use anilist::AniList;
use mal::Mal;
use model::Anime;
use store::Store;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    /// Base URL the browser uses to reach this app (for the MAL OAuth redirect).
    pub public_url: String,
    pub data_file: String,
    /// Directory holding the built WebAssembly frontend (`trunk build` output).
    pub static_dir: String,
    pub poll_minutes: u64,
    pub anilist_url: String,
    pub mal_client_id: Option<String>,
    pub mal_client_secret: Option<String>,
    pub mal_api_base: String,
    pub mal_auth_base: String,
}

impl Config {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let bind = var("BIND").unwrap_or_else(|| "127.0.0.1:3000".into());
        let public_url = var("PUBLIC_URL").unwrap_or_else(|| format!("http://{bind}"));
        Self {
            public_url: public_url.trim_end_matches('/').to_string(),
            bind,
            data_file: var("DATA_FILE").unwrap_or_else(|| "anime_data.json".into()),
            static_dir: var("STATIC_DIR").unwrap_or_else(default_static_dir),
            poll_minutes: var("POLL_MINUTES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(30)
                .max(1),
            anilist_url: var("ANILIST_URL").unwrap_or_else(|| "https://graphql.anilist.co".into()),
            mal_client_id: var("MAL_CLIENT_ID"),
            mal_client_secret: var("MAL_CLIENT_SECRET"),
            mal_api_base: var("MAL_API_BASE")
                .unwrap_or_else(|| "https://api.myanimelist.net".into()),
            mal_auth_base: var("MAL_AUTH_BASE").unwrap_or_else(|| "https://myanimelist.net".into()),
        }
    }

    pub fn mal_redirect_uri(&self) -> String {
        format!("{}/auth/mal/callback", self.public_url)
    }
}

/// `frontend/dist` relative to the working directory if present, otherwise relative to
/// the source checkout this binary was built from.
fn default_static_dir() -> String {
    let local = std::path::Path::new("frontend/dist");
    if local.join("index.html").exists() {
        return local.to_string_lossy().into_owned();
    }
    concat!(env!("CARGO_MANIFEST_DIR"), "/../frontend/dist").to_string()
}

pub struct AppState {
    pub config: Config,
    pub store: Store,
    pub anilist: AniList,
    pub mal: Option<Mal>,
    /// Tells the background poller to recompute when it should next wake up.
    pub reschedule: Notify,
    /// Serialises AniList refreshes (poller vs. the Refresh button).
    pub refresh_lock: Mutex<()>,
    /// Serialises MAL token refreshes (MAL rotates refresh tokens).
    pub mal_refresh_lock: Mutex<()>,
    /// OAuth `state` -> (PKCE verifier, created) for logins in progress.
    pub pending_oauth: Mutex<HashMap<String, (String, Instant)>>,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(config: Config) -> Result<SharedState> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("anime-scheduler/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let store = Store::open(&config.data_file)?;
        let anilist = AniList::new(http.clone(), config.anilist_url.clone());
        let mal = config.mal_client_id.clone().map(|id| {
            Mal::new(
                http,
                id,
                config.mal_client_secret.clone(),
                config.mal_api_base.clone(),
                config.mal_auth_base.clone(),
            )
        });
        Ok(Arc::new(Self {
            config,
            store,
            anilist,
            mal,
            reschedule: Notify::new(),
            refresh_lock: Mutex::new(()),
            mal_refresh_lock: Mutex::new(()),
            pending_oauth: Mutex::new(HashMap::new()),
        }))
    }

    /// A valid MAL access token, refreshing it if it is about to expire.
    pub async fn mal_token(&self) -> Result<Option<String>> {
        let (Some(mal), Some(tokens)) = (&self.mal, self.store.read().await.mal.clone()) else {
            return Ok(None);
        };
        let fresh_enough = |t: &model::MalTokens| t.expires_at > Utc::now() + Duration::minutes(5);
        if fresh_enough(&tokens) {
            return Ok(Some(tokens.access_token));
        }
        // Only one refresh at a time; whoever waited re-reads the (possibly new) tokens.
        let _guard = self.mal_refresh_lock.lock().await;
        let Some(tokens) = self.store.read().await.mal.clone() else {
            return Ok(None); // disconnected meanwhile
        };
        if fresh_enough(&tokens) {
            return Ok(Some(tokens.access_token));
        }
        let fresh = mal.refresh(&tokens).await?;
        let token = fresh.access_token.clone();
        let stored = self
            .store
            .mutate(|d| {
                // Don't resurrect a connection the user removed while we were refreshing.
                let still_same = d
                    .mal
                    .as_ref()
                    .is_some_and(|t| t.refresh_token == tokens.refresh_token);
                if still_same {
                    d.mal = Some(fresh);
                }
                still_same
            })
            .await?;
        Ok(stored.then_some(token))
    }

    /// Push an entry's progress to MyAnimeList if connected. Returns a human-readable
    /// error on failure (the local change is kept either way).
    pub async fn sync_to_mal(&self, anime: &Anime) -> Option<String> {
        let mal = self.mal.as_ref()?;
        let mal_id = anime.mal_id?;
        let result = async {
            let Some(token) = self.mal_token().await? else {
                return Ok(false);
            };
            mal.update_progress(&token, mal_id, anime.watched, anime.status)
                .await?;
            Ok::<_, anyhow::Error>(true)
        }
        .await;
        match result {
            Ok(_) => None,
            Err(e) => {
                tracing::warn!("MAL sync failed for {}: {e:#}", anime.title);
                Some(format!("{e:#}"))
            }
        }
    }
}
