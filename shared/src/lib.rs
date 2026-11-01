//! Types and pure logic shared by the server and the WebAssembly frontend.
//!
//! Everything here compiles for both native targets and `wasm32-unknown-unknown`,
//! so the browser computes "what can I watch now / when is the next episode" with
//! exactly the same code as the server.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// Where an anime sits in *your* list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchStatus {
    Watching,
    Planning,
    Paused,
    Completed,
    Dropped,
}

impl WatchStatus {
    pub const ALL: [WatchStatus; 5] = [
        Self::Watching,
        Self::Planning,
        Self::Paused,
        Self::Completed,
        Self::Dropped,
    ];

    /// Statuses that make up the "up next" view and generate new-episode notifications.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Watching | Self::Planning | Self::Paused)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Watching => "Watching",
            Self::Planning => "Plan to watch",
            Self::Paused => "Paused",
            Self::Completed => "Completed",
            Self::Dropped => "Dropped",
        }
    }

    /// The snake_case name used in JSON and form values.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Watching => "watching",
            Self::Planning => "planning",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Dropped => "dropped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|st| st.as_str() == s)
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchEvent {
    pub episode: u32,
    pub watched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

impl Anime {
    /// Record that `episode` was watched at `at`. Progress becomes at least `episode`.
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

    /// Set progress directly. Log entries above the new progress are dropped; newly
    /// covered episodes get logged as watched at `at`.
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

/// What can be watched right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

impl Availability {
    /// Sort order for the "up next" list: available first (most episodes first), then
    /// soonest upcoming, then undated, then finished.
    pub fn sort_key(&self) -> (u8, i64) {
        match self {
            Self::Available { count, .. } => (0, -(*count as i64)),
            Self::Upcoming { airing_at, .. } => (1, airing_at.timestamp()),
            Self::NoDate { start_date, .. } => (
                2,
                start_date
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|d| d.and_utc().timestamp())
                    .unwrap_or(i64::MAX),
            ),
            Self::AllWatched => (3, 0),
        }
    }
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub id: u64,
    pub anilist_id: i64,
    pub title: String,
    pub message: String,
    pub cover: Option<String>,
    pub created_at: DateTime<Utc>,
    pub read: bool,
}

// ---------------------------------------------------------------------------
// HTTP API payloads
// ---------------------------------------------------------------------------

/// An anime as returned by the API, with its availability at response time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimeView {
    #[serde(flatten)]
    pub anime: Anime,
    pub availability: Availability,
}

impl AnimeView {
    pub fn new(anime: Anime, now: DateTime<Utc>) -> Self {
        let availability = anime.availability(now);
        Self {
            anime,
            availability,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddReq {
    pub anilist_id: i64,
    #[serde(default = "default_status")]
    pub status: WatchStatus,
}

fn default_status() -> WatchStatus {
    WatchStatus::Planning
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateReq {
    pub status: Option<WatchStatus>,
    pub watched: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatchReq {
    /// Defaults to the next unwatched episode.
    pub episode: Option<u32>,
    /// Defaults to now.
    pub watched_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateResp {
    pub anime: AnimeView,
    /// Set when the change could not be pushed to MyAnimeList.
    pub mal_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub anilist_id: i64,
    pub title: String,
    pub title_english: Option<String>,
    pub cover: Option<String>,
    pub format: Option<String>,
    pub season: Option<String>,
    pub media_status: Option<String>,
    pub episodes: Option<u32>,
    pub in_list: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshResp {
    pub new_notifications: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationsResp {
    pub unread: usize,
    /// Newest first.
    pub notifications: Vec<Notification>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadReq {
    /// Omit to mark all as read.
    pub ids: Option<Vec<u64>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MalStatus {
    pub configured: bool,
    pub connected: bool,
    pub username: Option<String>,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportReq {
    /// MAL username; defaults to the connected account.
    pub username: Option<String>,
    /// Which list statuses to import (default: watching, planning, paused).
    pub statuses: Option<Vec<WatchStatus>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResp {
    pub added: usize,
    pub updated: usize,
    /// Titles that could not be matched to AniList.
    pub unmatched: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResp {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anime(status: &str, total: Option<u32>, latest: u32, next: Option<(u32, i64)>) -> Anime {
        Anime {
            anilist_id: 1,
            mal_id: None,
            title: "Test".into(),
            title_english: None,
            cover: None,
            site_url: None,
            status: WatchStatus::Planning,
            watched: 0,
            watch_log: vec![],
            airing: AiringInfo {
                media_status: Some(status.into()),
                total_episodes: total,
                latest_aired: latest,
                next_episode: next.map(|n| n.0),
                next_airing_at: next.and_then(|n| DateTime::from_timestamp(n.1, 0)),
                fetched_at: Some(Utc::now()),
                ..Default::default()
            },
            added_at: Utc::now(),
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
        let mut a = anime("RELEASING", Some(12), 4, Some((5, at)));
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
        let b = anime("RELEASING", None, 0, None);
        assert!(matches!(b.availability(now), Availability::NoDate { .. }));
    }

    #[test]
    fn progress_and_log() {
        let now = Utc::now();
        let mut a = anime("FINISHED", Some(3), 3, None);
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
    fn sort_order() {
        let now = Utc::now();
        let avail = anime("RELEASING", None, 3, Some((4, now.timestamp() + 10)));
        let soon = anime("RELEASING", None, 0, Some((1, now.timestamp() + 10)));
        let later = anime("RELEASING", None, 0, Some((1, now.timestamp() + 1000)));
        let done = anime("FINISHED", Some(1), 0, None);
        let mut v = [&done, &later, &avail, &soon];
        v.sort_by_key(|a| a.availability(now).sort_key());
        assert_eq!(
            v.map(|a| a.airing.next_airing_at),
            [&avail, &soon, &later, &done].map(|a| a.airing.next_airing_at)
        );
    }

    #[test]
    fn view_roundtrip() {
        let v = AnimeView::new(
            anime("RELEASING", Some(12), 4, Some((5, 1_900_000_000))),
            Utc::now(),
        );
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["availability"]["kind"], "available");
        assert_eq!(json["anilist_id"], 1, "anime fields are flattened");
        let back: AnimeView = serde_json::from_value(json).unwrap();
        assert_eq!(back, v);
    }
}
