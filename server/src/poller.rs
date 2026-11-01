//! Background refresh of airing data and creation of new-episode notifications.

use anyhow::Result;
use chrono::{DateTime, Utc};
use std::time::Duration;

use crate::{
    model::{FromMedia, Notification},
    SharedState,
};

/// Re-fetch airing info for every tracked anime. Returns the number of notifications created.
///
/// Runs are serialised, so the poller and the Refresh button can't interleave and apply
/// stale results over fresh ones.
///
/// # Errors
/// Fails if AniList can't be reached or the data file can't be written.
pub async fn refresh_all(state: &SharedState) -> Result<usize> {
    let _guard = state.refresh_lock.lock().await;
    let ids: Vec<i64> = state.store.read().await.anime.keys().copied().collect();
    if ids.is_empty() {
        return Ok(0);
    }
    let media = state.anilist.by_ids(&ids).await?;
    let now = Utc::now();
    let created = state
        .store
        .mutate(|d| {
            let first_new_id = d.next_notification_id;
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
            // By id, not position: the list is capped, so old entries may be dropped.
            d.notifications
                .iter()
                .filter(|n| n.id >= first_new_id)
                .cloned()
                .collect::<Vec<_>>()
        })
        .await?;
    push(state, &created).await;
    Ok(created.len())
}

/// Send new notifications to the phone via ntfy, if configured. Failures are only
/// logged: the in-app notification list still has them.
async fn push(state: &SharedState, notifications: &[Notification]) {
    let Some(ntfy) = &state.ntfy else { return };
    for n in notifications {
        if let Err(e) = ntfy.send(n).await {
            tracing::warn!("ntfy push failed for {}: {e:#}", n.title);
        }
    }
}

/// How long to sleep before the next refresh.
///
/// Normally the poll interval, or until just after the next known air time if that's
/// sooner (AniList updates at air time; we give it two minutes). Air times more than an
/// hour old are ignored so a show AniList stopped updating can't make us poll every
/// minute forever. After `failures` consecutive errors we back off exponentially.
pub fn compute_delay(
    now: DateTime<Utc>,
    airing_times: impl IntoIterator<Item = DateTime<Utc>>,
    max: Duration,
    failures: u32,
) -> Duration {
    let soonest = airing_times
        .into_iter()
        .filter(|t| *t > now - chrono::Duration::hours(1))
        .min();
    let mut delay = match soonest {
        Some(t) => {
            let secs = ((t - now).num_seconds() + 120).max(60);
            Duration::from_secs(secs.unsigned_abs()).min(max)
        }
        None => max,
    };
    if failures > 0 {
        let backoff = Duration::from_secs(60u64 << failures.min(6)).min(max);
        delay = delay.max(backoff);
    }
    delay
}

async fn next_delay(state: &SharedState, failures: u32) -> Duration {
    let times: Vec<_> = state
        .store
        .read()
        .await
        .anime
        .values()
        .filter(|a| a.status.is_active())
        .filter_map(|a| a.airing.next_airing_at)
        .collect();
    compute_delay(
        Utc::now(),
        times,
        Duration::from_secs(state.config.poll_minutes * 60),
        failures,
    )
}

pub fn spawn(state: SharedState) {
    tokio::spawn(async move {
        let mut failures = 0u32;
        loop {
            match refresh_all(&state).await {
                Ok(n) => {
                    failures = 0;
                    if n > 0 {
                        tracing::info!("refresh done, {n} new notification(s)");
                    }
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    tracing::warn!("refresh failed ({failures} in a row): {e:#}");
                }
            }
            // Sleep until the next refresh is due. A reschedule (after a manual refresh
            // or a newly added show) recomputes the wake-up time without refreshing.
            loop {
                let delay = next_delay(&state, failures).await;
                tracing::debug!("next refresh in {}s", delay.as_secs());
                tokio::select! {
                    () = tokio::time::sleep(delay) => break,
                    () = state.reschedule.notified() => {}
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: Duration = Duration::from_secs(30 * 60);

    #[test]
    fn delay_rules() {
        let now = Utc::now();
        let mins = |m: i64| now + chrono::Duration::minutes(m);
        assert_eq!(compute_delay(now, [], MAX, 0), MAX);
        // Episode in 10 min -> wake 2 min after it airs.
        assert_eq!(
            compute_delay(now, [mins(10), mins(20)], MAX, 0),
            Duration::from_secs(12 * 60)
        );
        // Far future -> capped at the poll interval.
        assert_eq!(compute_delay(now, [mins(600)], MAX, 0), MAX);
        // Just aired / slightly overdue -> at least a minute.
        assert_eq!(
            compute_delay(now, [mins(-30)], MAX, 0),
            Duration::from_secs(60)
        );
        // Long overdue (AniList stopped updating) -> ignored.
        assert_eq!(compute_delay(now, [mins(-90)], MAX, 0), MAX);
        // Errors back off: 2, 4, 8 ... minutes, capped.
        assert_eq!(
            compute_delay(now, [mins(-30)], MAX, 1),
            Duration::from_secs(120)
        );
        assert_eq!(
            compute_delay(now, [mins(-30)], MAX, 3),
            Duration::from_secs(480)
        );
        assert_eq!(compute_delay(now, [mins(-30)], MAX, 20), MAX);
    }
}
