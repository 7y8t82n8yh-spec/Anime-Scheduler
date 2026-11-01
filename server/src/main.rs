use anime_scheduler::{poller, routes, AppState, Config};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env();
    let state = AppState::new(config.clone())?;
    if state.mal.is_none() {
        tracing::info!("MAL_CLIENT_ID not set: MyAnimeList import/sync disabled");
    }
    poller::spawn(state.clone());

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("Anime Scheduler running at {}", config.public_url);
    axum::serve(listener, routes::router(state)).await?;
    Ok(())
}
