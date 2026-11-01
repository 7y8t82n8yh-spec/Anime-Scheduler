//! Minimal AniList GraphQL client. AniList is used for airing schedules because it
//! exposes `nextAiringEpisode` for every show and needs no API key.

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use std::time::Duration;

const FIELDS: &str =
    "id idMal title { romaji english userPreferred } coverImage { large } siteUrl \
    status episodes format season seasonYear startDate { year month day } \
    nextAiringEpisode { episode airingAt }";

const PER_PAGE: usize = 50;

#[derive(Clone)]
pub struct AniList {
    http: reqwest::Client,
    url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Media {
    pub id: i64,
    pub id_mal: Option<i64>,
    pub title: Title,
    pub cover_image: Option<Cover>,
    pub site_url: Option<String>,
    pub status: Option<String>,
    pub episodes: Option<u32>,
    pub format: Option<String>,
    pub season: Option<String>,
    pub season_year: Option<i32>,
    pub start_date: Option<FuzzyDate>,
    pub next_airing_episode: Option<NextAiring>,
}

impl Media {
    pub fn display_title(&self) -> String {
        self.title
            .user_preferred
            .clone()
            .or_else(|| self.title.romaji.clone())
            .or_else(|| self.title.english.clone())
            .unwrap_or_else(|| format!("AniList #{}", self.id))
    }

    /// e.g. "Fall 2026".
    pub fn season_label(&self) -> Option<String> {
        match (&self.season, self.season_year) {
            (Some(s), Some(y)) => {
                let lower = s.to_lowercase();
                let mut c = lower.chars();
                let cap: String = c
                    .next()
                    .map(|f| f.to_uppercase().chain(c).collect())
                    .unwrap_or_default();
                Some(format!("{cap} {y}"))
            }
            (None, Some(y)) => Some(y.to_string()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Title {
    pub romaji: Option<String>,
    pub english: Option<String>,
    pub user_preferred: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Cover {
    pub large: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FuzzyDate {
    pub year: Option<i32>,
    pub month: Option<u32>,
    pub day: Option<u32>,
}

impl FuzzyDate {
    pub fn to_naive(&self) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(self.year?, self.month.unwrap_or(1), self.day.unwrap_or(1))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextAiring {
    pub episode: u32,
    pub airing_at: i64,
}

#[derive(Deserialize)]
struct GqlResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GqlError>>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Deserialize)]
struct PageData {
    #[serde(rename = "Page")]
    page: Page,
}

#[derive(Deserialize)]
struct Page {
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
    media: Vec<Media>,
}

#[derive(Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
}

impl AniList {
    pub fn new(http: reqwest::Client, url: impl Into<String>) -> Self {
        Self {
            http,
            url: url.into(),
        }
    }

    async fn query<T: DeserializeOwned>(&self, query: &str, variables: Value) -> Result<T> {
        let body = json!({ "query": query, "variables": variables });
        for attempt in 0..3 {
            let resp = self
                .http
                .post(&self.url)
                .json(&body)
                .send()
                .await
                .context("AniList request failed")?;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < 2 {
                let wait = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(30);
                tracing::warn!("AniList rate limit hit, waiting {wait}s");
                tokio::time::sleep(Duration::from_secs(wait.min(120))).await;
                continue;
            }
            let status = resp.status();
            let parsed: GqlResponse<T> = resp
                .json()
                .await
                .with_context(|| format!("invalid AniList response (HTTP {status})"))?;
            if let Some(data) = parsed.data {
                return Ok(data);
            }
            let msg = parsed
                .errors
                .unwrap_or_default()
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            bail!("AniList error (HTTP {status}): {msg}");
        }
        bail!("AniList rate limit exceeded")
    }

    pub async fn search(&self, search: &str) -> Result<Vec<Media>> {
        let q = format!(
            "query ($search: String) {{ Page(page: 1, perPage: 15) {{ pageInfo {{ hasNextPage }} \
             media(search: $search, type: ANIME, sort: SEARCH_MATCH) {{ {FIELDS} }} }} }}"
        );
        let data: PageData = self.query(&q, json!({ "search": search })).await?;
        Ok(data.page.media)
    }

    /// Fetch media by AniList ids.
    pub async fn by_ids(&self, ids: &[i64]) -> Result<Vec<Media>> {
        self.batched("id_in", ids).await
    }

    /// Fetch media by MyAnimeList ids.
    pub async fn by_mal_ids(&self, ids: &[i64]) -> Result<Vec<Media>> {
        self.batched("idMal_in", ids).await
    }

    async fn batched(&self, arg: &str, ids: &[i64]) -> Result<Vec<Media>> {
        let q = format!(
            "query ($ids: [Int], $page: Int) {{ Page(page: $page, perPage: {PER_PAGE}) {{ \
             pageInfo {{ hasNextPage }} media({arg}: $ids, type: ANIME) {{ {FIELDS} }} }} }}"
        );
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(PER_PAGE) {
            let mut page = 1;
            loop {
                let data: PageData = self
                    .query(&q, json!({ "ids": chunk, "page": page }))
                    .await?;
                out.extend(data.page.media);
                if !data.page.page_info.has_next_page || page >= 20 {
                    break;
                }
                page += 1;
            }
        }
        Ok(out)
    }
}
