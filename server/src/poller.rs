//! Background refresh of airing data and creation of new-episode notifications.

use anyhow::Result;
use chrono::Utc;
use std::time::Duration;

use crate::{model::FromMedia, SharedState};

/// Re-fetch airing info for every tracked anime. Returns the number of notifications created.
pub async fn refresh_all(state: &SharedState) -> Result<usize> {
    let ids: Vec<i64> = state.store.read().await.anime.keys().copied().collect();
    if ids.is_empty() {
        return Ok(0);
    }
    let media = state.anilist.by_ids(&ids).await?;
    let now = Utc::now();
    state
        .store
        .mutate(|d| {
            let before = d.notifications.len();
            let mut fresh = Vec::new();
            for m in &media {
                if let Some(a) = d.anime.get_mut(&m.id) {
                    if let Some(eps) = a.apply_media(m, now) {
                        fresh.push((m.id, eps));
                    }
                }
            }
            for (id, eps) in fresh {
                d.notify_new_episodes(id, eps, now);
            }
            d.notifications.len().saturating_sub(before)
        })
        .await
}

/// How long to sleep: the poll interval, or until just after the next episode airs if sooner.
async fn next_delay(state: &SharedState) -> Duration {
    let max = Duration::from_secs(state.config.poll_minutes * 60);
    let now = Utc::now();
    let soonest = state
        .store
        .read()
        .await
        .anime
        .values()
        .filter(|a| a.status.is_active())
        .filter_map(|a| a.airing.next_airing_at)
        .min();
    match soonest {
        // AniList usually updates right at air time; give it a couple of minutes.
        Some(t) => {
            let secs = (t - now).num_seconds() + 120;
            Duration::from_secs(secs.max(60) as u64).min(max)
        }
        None => max,
    }
}

pub fn spawn(state: SharedState) {
    tokio::spawn(async move {
        loop {
            match refresh_all(&state).await {
                Ok(0) => tracing::debug!("refresh done"),
                Ok(n) => tracing::info!("refresh done, {n} new notification(s)"),
                Err(e) => tracing::warn!("refresh failed: {e:#}"),
            }
            let delay = next_delay(&state).await;
            tracing::debug!("next refresh in {}s", delay.as_secs());
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = state.refresh_now.notified() => {}
            }
        }
    });
}
