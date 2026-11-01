//! Core data model and the pure logic that works on it (availability, new-episode detection).

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::anilist::Media;

/// Where an anime sits in *your* list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchStatus {
    Watching,
    Planning,
    Paused,
    Completed,
    Dropped,
}

impl WatchStatus {
    /// Statuses for which new-episode notifications are generated.
    pub fn notifies(self) -> bool {
        matches!(self, Self::Watching | Self::Planning | Self::Paused)
    }

    pub fn to_mal(self) -> &'static str {
        match self {
            Self::Watching => "watching",
            Self::Planning => "plan_to_watch",
            Self::Paused => "on_hold",
            Self::Completed => "completed",
            Self::Dropped => "dropped",
        }
    }

    pub fn from_mal(s: &str) -> Option<Self> {
        Some(match s {
            "watching" => Self::Watching,
            "plan_to_watch" => Self::Planning,
            "on_hold" => Self::Paused,
            "completed" => Self::Completed,
            "dropped" => Self::Dropped,
            _ => return None,
        })
    }
}

/// Airing information as last fetched from AniList.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiringInfo {
    /// AniList media status: FINISHED, RELEASING, NOT_YET_RELEASED, CANCELLED, HIATUS.
    pub media_status: Option<String>,
    pub total_episodes: Option<u32>,
    /// Highest episode number that has aired.
    pub latest_aired: u32,
    pub next_episode: Option<u32>,
    pub next_airing_at: Option<DateTime<Utc>>,
    pub start_date: Option<NaiveDate>,
    pub format: Option<String>,
    pub season: Option<String>,
    pub fetched_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchEvent {
    pub episode: u32,
    pub watched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anime {
    pub anilist_id: i64,
    pub mal_id: Option<i64>,
    pub title: String,
    pub title_english: Option<String>,
    pub cover: Option<String>,
    pub site_url: Option<String>,
    pub status: WatchStatus,
    /// Number of episodes watched (progress).
    pub watched: u32,
    /// When each episode was watched, sorted by episode.
    #[serde(default)]
    pub watch_log: Vec<WatchEvent>,
    pub airing: AiringInfo,
    pub added_at: DateTime<Utc>,
}

/// A range of episodes that newly became available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewEpisodes {
    pub from: u32,
    pub to: u32,
}

impl Anime {
    pub fn from_media(m: &Media, status: WatchStatus, now: DateTime<Utc>) -> Self {
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

    /// Update metadata from a fresh AniList fetch. Returns the episodes that became
    /// available since the previous fetch (never on the very first fetch).
    pub fn apply_media(&mut self, m: &Media, now: DateTime<Utc>) -> Option<NewEpisodes> {
        self.title = m.display_title();
        self.title_english = m.title.english.clone();
        self.cover = m.cover_image.as_ref().and_then(|c| c.large.clone());
        self.site_url = m.site_url.clone();
        if m.id_mal.is_some() {
            self.mal_id = m.id_mal;
        }

        let first_fetch = self.airing.fetched_at.is_none();
        let prev = self.airing.latest_aired;
        let latest = latest_aired(
            m.status.as_deref(),
            m.episodes,
            m.next_airing_episode.as_ref().map(|n| n.episode),
            prev,
        );

        self.airing = AiringInfo {
            media_status: m.status.clone(),
            total_episodes: m.episodes,
            latest_aired: latest,
            next_episode: m.next_airing_episode.as_ref().map(|n| n.episode),
            next_airing_at: m
                .next_airing_episode
                .as_ref()
                .and_then(|n| DateTime::from_timestamp(n.airing_at, 0)),
            start_date: m.start_date.as_ref().and_then(|d| d.to_naive()),
            format: m.format.clone(),
            season: match (&m.season, m.season_year) {
                (Some(s), Some(y)) => Some(format!("{} {}", capitalize(s), y)),
                (None, Some(y)) => Some(y.to_string()),
                _ => None,
            },
            fetched_at: Some(now),
        };

        (!first_fetch && latest > prev).then_some(NewEpisodes {
            from: prev + 1,
            to: latest,
        })
    }

    /// Record that `episode` was watched at `at`. Progress becomes at least `episode`;
    /// every episode up to it without a log entry gets one.
    pub fn log_watch(&mut self, episode: u32, at: DateTime<Utc>) {
        if episode == 0 {
            return;
        }
        if let Some(e) = self.watch_log.iter_mut().find(|e| e.episode == episode) {
            e.watched_at = at;
        } else {
            self.watch_log.push(WatchEvent {
                episode,
                watched_at: at,
            });
        }
        if episode > self.watched {
            self.watched = episode;
        }
        self.watch_log.sort_by_key(|e| e.episode);
        self.after_progress_change();
    }

    /// Set progress directly. Log entries above the new progress are dropped; new
    /// episodes get logged as watched at `at`.
    pub fn set_progress(&mut self, watched: u32, at: DateTime<Utc>) {
        let watched = match self.airing.total_episodes {
            Some(t) if t > 0 => watched.min(t),
            _ => watched,
        };
        self.watch_log.retain(|e| e.episode <= watched);
        for ep in (self.watched + 1)..=watched {
            self.watch_log.push(WatchEvent {
                episode: ep,
                watched_at: at,
            });
        }
        self.watch_log.sort_by_key(|e| e.episode);
        self.watched = watched;
        self.after_progress_change();
    }

    fn after_progress_change(&mut self) {
        if self.watched > 0 && self.status == WatchStatus::Planning {
            self.status = WatchStatus::Watching;
        }
        if let Some(total) = self.airing.total_episodes {
            if total > 0 && self.watched >= total && self.status != WatchStatus::Dropped {
                self.status = WatchStatus::Completed;
            }
        }
    }

    pub fn availability(&self, now: DateTime<Utc>) -> Availability {
        let a = &self.airing;
        if a.latest_aired > self.watched {
            return Availability::Available {
                from: self.watched + 1,
                to: a.latest_aired,
                count: a.latest_aired - self.watched,
            };
        }
        if let (Some(ep), Some(at)) = (a.next_episode, a.next_airing_at) {
            return Availability::Upcoming {
                episode: ep,
                airing_at: at,
                in_seconds: (at - now).num_seconds().max(0),
            };
        }
        let finished = matches!(
            a.media_status.as_deref(),
            Some("FINISHED") | Some("CANCELLED")
        );
        if finished || a.total_episodes.is_some_and(|t| t > 0 && self.watched >= t) {
            return Availability::AllWatched;
        }
        Availability::NoDate {
            start_date: a.start_date,
            hiatus: a.media_status.as_deref() == Some("HIATUS"),
        }
    }
}

/// What the user can do with this anime right now.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Availability {
    /// Unwatched episodes `from..=to` are out.
    Available { from: u32, to: u32, count: u32 },
    /// Caught up; the next episode airs at `airing_at`.
    Upcoming {
        episode: u32,
        airing_at: DateTime<Utc>,
        in_seconds: i64,
    },
    /// Show finished and everything was watched.
    AllWatched,
    /// Nothing available and no exact air date announced.
    NoDate {
        start_date: Option<NaiveDate>,
        hiatus: bool,
    },
}

/// Work out the highest aired episode from AniList data, falling back to what we knew.
pub fn latest_aired(
    status: Option<&str>,
    total: Option<u32>,
    next_episode: Option<u32>,
    prev: u32,
) -> u32 {
    if let Some(next) = next_episode {
        return next.saturating_sub(1);
    }
    match status {
        Some("FINISHED") => total.unwrap_or(prev).max(prev),
        Some("NOT_YET_RELEASED") => 0,
        _ => prev,
    }
}

fn capitalize(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut c = lower.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: u64,
    pub anilist_id: i64,
    pub title: String,
    pub message: String,
    pub cover: Option<String>,
    pub created_at: DateTime<Utc>,
    pub read: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MalTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub username: Option<String>,
}

/// Everything that gets persisted to disk.
#[derive(Debug, Default, Serialize, Deserialize)]
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
        if !a.status.notifies() {
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
            season: None,
            season_year: None,
            start_date: None,
            next_airing_episode: next.map(|(episode, airing_at)| NextAiring { episode, airing_at }),
        }
    }

    #[test]
    fn latest_aired_rules() {
        assert_eq!(latest_aired(Some("RELEASING"), Some(12), Some(5), 0), 4);
        assert_eq!(latest_aired(Some("FINISHED"), Some(12), None, 3), 12);
        assert_eq!(latest_aired(Some("NOT_YET_RELEASED"), None, None, 0), 0);
        assert_eq!(latest_aired(Some("NOT_YET_RELEASED"), None, Some(1), 0), 0);
        assert_eq!(latest_aired(Some("HIATUS"), None, None, 7), 7);
    }

    #[test]
    fn availability_states() {
        let now = Utc::now();
        let at = now.timestamp() + 3600;
        let mut a = Anime::from_media(
            &media("RELEASING", Some(12), Some((5, at))),
            WatchStatus::Planning,
            now,
        );
        assert_eq!(
            a.availability(now),
            Availability::Available {
                from: 1,
                to: 4,
                count: 4
            }
        );

        a.set_progress(4, now);
        assert_eq!(a.status, WatchStatus::Watching);
        match a.availability(now) {
            Availability::Upcoming {
                episode,
                in_seconds,
                ..
            } => {
                assert_eq!(episode, 5);
                assert!((3599..=3600).contains(&in_seconds));
            }
            other => panic!("unexpected {other:?}"),
        }

        let b = Anime::from_media(&media("RELEASING", None, None), WatchStatus::Planning, now);
        assert!(matches!(b.availability(now), Availability::NoDate { .. }));
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
    fn progress_and_log() {
        let now = Utc::now();
        let mut a = Anime::from_media(
            &media("FINISHED", Some(3), None),
            WatchStatus::Planning,
            now,
        );
        a.log_watch(1, now);
        assert_eq!(a.watched, 1);
        assert_eq!(a.status, WatchStatus::Watching);
        a.set_progress(10, now);
        assert_eq!(a.watched, 3, "clamped to total");
        assert_eq!(a.watch_log.len(), 3);
        assert_eq!(a.status, WatchStatus::Completed);
        assert_eq!(a.availability(now), Availability::AllWatched);
        a.status = WatchStatus::Watching;
        a.set_progress(1, now);
        assert_eq!(a.watch_log.len(), 1);
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
}
