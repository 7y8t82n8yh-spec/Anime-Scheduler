//! Server-side model: persisted data, AniList → [`Anime`] conversion and new-episode detection.
//! Types shared with the frontend live in the `anime-shared` crate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use anime_shared::*;

use crate::anilist::Media;

/// A range of episodes that newly became available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewEpisodes {
    pub from: u32,
    pub to: u32,
}

/// Building and updating [`Anime`] entries from AniList data.
pub trait FromMedia {
    fn from_media(m: &Media, status: WatchStatus, now: DateTime<Utc>) -> Self;
    /// Update metadata from a fresh AniList fetch. Returns the episodes that became
    /// available since the previous fetch (never on the very first fetch).
    fn apply_media(&mut self, m: &Media, now: DateTime<Utc>) -> Option<NewEpisodes>;
}

impl FromMedia for Anime {
    fn from_media(m: &Media, status: WatchStatus, now: DateTime<Utc>) -> Self {
        let mut a = Anime {
            anilist_id: m.id,
            mal_id: m.id_mal,
            title: String::new(),
            title_english: None,
            cover: None,
            site_url: None,
            status,
            watched: 0,
            watch_log: Vec::new(),
            airing: AiringInfo::default(),
            added_at: now,
        };
        a.apply_media(m, now);
        a
    }

    fn apply_media(&mut self, m: &Media, now: DateTime<Utc>) -> Option<NewEpisodes> {
        self.title = m.display_title();
        self.title_english = m.title.english.clone();
        self.cover = m.cover_image.as_ref().and_then(|c| c.large.clone());
        self.site_url = m.site_url.clone();
        if m.id_mal.is_some() {
            self.mal_id = m.id_mal;
        }

        let first_fetch = self.airing.fetched_at.is_none();
        let prev = self.airing.latest_aired;
        let next = m.next_airing_episode.as_ref();
        let latest = latest_aired(
            m.status.as_deref(),
            m.episodes,
            next.map(|n| n.episode),
            prev,
        );

        self.airing = AiringInfo {
            media_status: m.status.clone(),
            total_episodes: m.episodes,
            latest_aired: latest,
            next_episode: next.map(|n| n.episode),
            next_airing_at: next.and_then(|n| DateTime::from_timestamp(n.airing_at, 0)),
            start_date: m.start_date.as_ref().and_then(|d| d.to_naive()),
            format: m.format.clone(),
            season: m.season_label(),
            fetched_at: Some(now),
        };

        (!first_fetch && latest > prev).then_some(NewEpisodes {
            from: prev + 1,
            to: latest,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MalTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub username: Option<String>,
}

/// Everything that gets persisted to disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub anime: BTreeMap<i64, Anime>,
    #[serde(default)]
    pub notifications: Vec<Notification>,
    #[serde(default)]
    pub next_notification_id: u64,
    #[serde(default)]
    pub mal: Option<MalTokens>,
}

const MAX_NOTIFICATIONS: usize = 300;

impl Data {
    pub fn notify_new_episodes(&mut self, anilist_id: i64, eps: NewEpisodes, now: DateTime<Utc>) {
        let Some(a) = self.anime.get(&anilist_id) else {
            return;
        };
        if !a.status.is_active() {
            return;
        }
        // Skip episodes already marked watched (e.g. seen elsewhere before AniList updated).
        let eps = NewEpisodes {
            from: eps.from.max(a.watched + 1),
            to: eps.to,
        };
        if eps.from > eps.to {
            return;
        }
        let message = if eps.from == eps.to {
            format!("Episode {} is out!", eps.to)
        } else {
            format!("Episodes {}–{} are out!", eps.from, eps.to)
        };
        let n = Notification {
            id: self.next_notification_id,
            anilist_id,
            title: a.title.clone(),
            message,
            cover: a.cover.clone(),
            created_at: now,
            read: false,
        };
        self.next_notification_id += 1;
        self.notifications.push(n);
        if self.notifications.len() > MAX_NOTIFICATIONS {
            let excess = self.notifications.len() - MAX_NOTIFICATIONS;
            self.notifications.drain(..excess);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anilist::{NextAiring, Title};

    fn media(status: &str, episodes: Option<u32>, next: Option<(u32, i64)>) -> Media {
        Media {
            id: 1,
            id_mal: Some(10),
            title: Title {
                romaji: Some("Test".into()),
                english: None,
                user_preferred: None,
            },
            cover_image: None,
            site_url: None,
            status: Some(status.into()),
            episodes,
            format: None,
            season: Some("FALL".into()),
            season_year: Some(2026),
            start_date: None,
            next_airing_episode: next.map(|(episode, airing_at)| NextAiring { episode, airing_at }),
        }
    }

    #[test]
    fn from_media_fills_airing() {
        let now = Utc::now();
        let a = Anime::from_media(
            &media("RELEASING", Some(12), Some((5, 1_900_000_000))),
            WatchStatus::Planning,
            now,
        );
        assert_eq!(a.airing.latest_aired, 4);
        assert_eq!(a.airing.season.as_deref(), Some("Fall 2026"));
        assert_eq!(a.mal_id, Some(10));
    }

    #[test]
    fn detects_new_episodes_only_after_first_fetch() {
        let now = Utc::now();
        let mut a = Anime::from_media(
            &media("RELEASING", Some(12), Some((3, 0))),
            WatchStatus::Watching,
            now,
        );
        assert_eq!(a.airing.latest_aired, 2);
        assert_eq!(
            a.apply_media(&media("RELEASING", Some(12), Some((3, 0))), now),
            None
        );
        assert_eq!(
            a.apply_media(&media("RELEASING", Some(12), Some((5, 0))), now),
            Some(NewEpisodes { from: 3, to: 4 })
        );
        assert_eq!(
            a.apply_media(&media("FINISHED", Some(12), None), now),
            Some(NewEpisodes { from: 5, to: 12 })
        );
    }

    #[test]
    fn notifications_respect_status() {
        let now = Utc::now();
        let mut d = Data::default();
        let mut a = Anime::from_media(
            &media("RELEASING", None, Some((2, 0))),
            WatchStatus::Dropped,
            now,
        );
        d.anime.insert(1, a.clone());
        d.notify_new_episodes(1, NewEpisodes { from: 2, to: 2 }, now);
        assert!(d.notifications.is_empty());
        a.status = WatchStatus::Watching;
        d.anime.insert(1, a);
        d.notify_new_episodes(1, NewEpisodes { from: 2, to: 3 }, now);
        assert_eq!(d.notifications.len(), 1);
        assert_eq!(d.notifications[0].message, "Episodes 2–3 are out!");
    }

    #[test]
    fn notifications_skip_watched_episodes() {
        let now = Utc::now();
        let mut d = Data::default();
        let mut a = Anime::from_media(
            &media("RELEASING", None, Some((2, 0))),
            WatchStatus::Watching,
            now,
        );
        a.watched = 3;
        d.anime.insert(1, a);
        d.notify_new_episodes(1, NewEpisodes { from: 2, to: 3 }, now);
        assert!(d.notifications.is_empty());
        d.notify_new_episodes(1, NewEpisodes { from: 2, to: 5 }, now);
        assert_eq!(d.notifications[0].message, "Episodes 4–5 are out!");
    }
}
