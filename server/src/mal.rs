//! MyAnimeList API v2 client: reading a user's list, OAuth2 (PKCE) login and
//! pushing watch progress back.

use anyhow::{bail, Context, Result};
use chrono::{Duration, Utc};
use rand::{distributions::Alphanumeric, Rng};
use serde::Deserialize;

use crate::model::{MalTokens, WatchStatus};

#[derive(Clone)]
pub struct Mal {
    http: reqwest::Client,
    client_id: String,
    client_secret: Option<String>,
    api_base: String,
    auth_base: String,
}

#[derive(Debug, Clone)]
pub struct MalListEntry {
    pub mal_id: i64,
    pub title: String,
    pub status: Option<WatchStatus>,
    pub watched: u32,
}

#[derive(Deserialize)]
struct ListResponse {
    data: Vec<ListItem>,
    paging: Option<Paging>,
}

#[derive(Deserialize)]
struct Paging {
    next: Option<String>,
}

#[derive(Deserialize)]
struct ListItem {
    node: Node,
    list_status: Option<ListStatus>,
}

#[derive(Deserialize)]
struct Node {
    id: i64,
    title: String,
}

#[derive(Deserialize)]
struct ListStatus {
    status: Option<String>,
    num_episodes_watched: Option<u32>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

#[derive(Deserialize)]
struct User {
    name: String,
}

/// Turn an unsuccessful MAL response into an error. The response body goes to the log
/// only; the returned message is safe to show to the user.
async fn failure(resp: reqwest::Response, what: &str) -> anyhow::Error {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    tracing::warn!("{what} failed (HTTP {status}): {body}");
    anyhow::anyhow!("{what} failed (HTTP {status})")
}

/// A random alphanumeric string of `len` characters (for OAuth state and PKCE).
pub fn random_token(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

/// A random PKCE code verifier (MAL only supports the `plain` challenge method,
/// so the challenge is the verifier itself).
pub fn new_code_verifier() -> String {
    random_token(96)
}

impl Mal {
    pub fn new(
        http: reqwest::Client,
        client_id: String,
        client_secret: Option<String>,
        api_base: String,
        auth_base: String,
    ) -> Self {
        Self {
            http,
            client_id,
            client_secret,
            api_base,
            auth_base,
        }
    }

    pub fn authorize_url(&self, verifier: &str, state: &str, redirect_uri: &str) -> Result<String> {
        let url = reqwest::Url::parse_with_params(
            &format!("{}/v1/oauth2/authorize", self.auth_base),
            &[
                ("response_type", "code"),
                ("client_id", self.client_id.as_str()),
                ("code_challenge", verifier),
                ("code_challenge_method", "plain"),
                ("state", state),
                ("redirect_uri", redirect_uri),
            ],
        )?;
        Ok(url.into())
    }

    async fn token_request(&self, mut form: Vec<(&str, String)>) -> Result<MalTokens> {
        form.push(("client_id", self.client_id.clone()));
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        let resp = self
            .http
            .post(format!("{}/v1/oauth2/token", self.auth_base))
            .form(&form)
            .send()
            .await
            .context("MAL token request failed")?;
        if !resp.status().is_success() {
            return Err(failure(resp, "MyAnimeList login refresh").await);
        }
        let t: TokenResponse = resp.json().await?;
        Ok(MalTokens {
            access_token: t.access_token,
            refresh_token: t.refresh_token,
            expires_at: Utc::now() + Duration::seconds(t.expires_in),
            username: None,
        })
    }

    pub async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<MalTokens> {
        self.token_request(vec![
            ("grant_type", "authorization_code".into()),
            ("code", code.into()),
            ("code_verifier", verifier.into()),
            ("redirect_uri", redirect_uri.into()),
        ])
        .await
    }

    pub async fn refresh(&self, tokens: &MalTokens) -> Result<MalTokens> {
        let mut t = self
            .token_request(vec![
                ("grant_type", "refresh_token".into()),
                ("refresh_token", tokens.refresh_token.clone()),
            ])
            .await?;
        t.username = tokens.username.clone();
        Ok(t)
    }

    fn auth(&self, req: reqwest::RequestBuilder, token: Option<&str>) -> reqwest::RequestBuilder {
        match token {
            Some(t) => req.bearer_auth(t),
            None => req.header("X-MAL-CLIENT-ID", &self.client_id),
        }
    }

    pub async fn me(&self, token: &str) -> Result<String> {
        let resp = self
            .auth(
                self.http.get(format!("{}/v2/users/@me", self.api_base)),
                Some(token),
            )
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json::<User>().await?.name)
    }

    /// Fetch a user's anime list. `user` is a username or `@me` (requires a token).
    /// Public lists can be read with just the client id.
    pub async fn user_list(&self, user: &str, token: Option<&str>) -> Result<Vec<MalListEntry>> {
        let first = self.list_url(user)?;
        let origin = first.origin();
        let mut url = first.to_string();
        let mut out = Vec::new();
        for _ in 0..50 {
            let resp = self
                .auth(self.http.get(&url), token)
                .send()
                .await
                .context("MAL request failed")?;
            if !resp.status().is_success() {
                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    bail!("MyAnimeList user not found, or their list is private");
                }
                return Err(failure(resp, "MyAnimeList list request").await);
            }
            let page: ListResponse = resp.json().await.context("invalid MAL list response")?;
            out.extend(page.data.into_iter().map(|i| {
                let ls = i.list_status;
                MalListEntry {
                    mal_id: i.node.id,
                    title: i.node.title,
                    status: ls
                        .as_ref()
                        .and_then(|s| s.status.as_deref())
                        .and_then(WatchStatus::from_mal),
                    watched: ls.and_then(|s| s.num_episodes_watched).unwrap_or(0),
                }
            }));
            match page.paging.and_then(|p| p.next) {
                // Only follow pagination links back to the MAL API (they carry our token).
                Some(next) if reqwest::Url::parse(&next).is_ok_and(|u| u.origin() == origin) => {
                    url = next;
                }
                Some(next) => {
                    bail!("refusing to follow MAL pagination link to another host: {next}")
                }
                None => break,
            }
        }
        Ok(out)
    }

    /// `{api_base}/v2/users/{user}/animelist?...` with `user` encoded as a single path
    /// segment, so names containing `/`, `?`, `#` or `..` can't change the request.
    pub fn list_url(&self, user: &str) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.api_base)?;
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("invalid MAL API base URL"))?
            .pop_if_empty()
            .extend(["v2", "users", user, "animelist"]);
        url.query_pairs_mut()
            .append_pair("fields", "list_status")
            .append_pair("limit", "1000")
            .append_pair("nsfw", "true");
        Ok(url)
    }

    /// The signed-in user's list entry for one anime, if it is on their list.
    pub async fn my_list_status(
        &self,
        token: &str,
        mal_id: i64,
    ) -> Result<Option<(Option<WatchStatus>, u32)>> {
        #[derive(Deserialize)]
        struct AnimeDetails {
            my_list_status: Option<ListStatus>,
        }
        let resp = self
            .http
            .get(format!(
                "{}/v2/anime/{mal_id}?fields=my_list_status",
                self.api_base
            ))
            .bearer_auth(token)
            .send()
            .await
            .context("MAL request failed")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            bail!("MAL anime lookup failed (HTTP {})", resp.status());
        }
        let details: AnimeDetails = resp.json().await.context("invalid MAL anime response")?;
        Ok(details.my_list_status.map(|s| {
            (
                s.status.as_deref().and_then(WatchStatus::from_mal),
                s.num_episodes_watched.unwrap_or(0),
            )
        }))
    }

    pub async fn update_progress(
        &self,
        token: &str,
        mal_id: i64,
        watched: u32,
        status: WatchStatus,
    ) -> Result<()> {
        let resp = self
            .http
            .patch(format!(
                "{}/v2/anime/{}/my_list_status",
                self.api_base, mal_id
            ))
            .bearer_auth(token)
            .form(&[
                ("num_watched_episodes", watched.to_string()),
                ("status", status.to_mal().to_string()),
            ])
            .send()
            .await
            .context("MAL update failed")?;
        if !resp.status().is_success() {
            return Err(failure(resp, "MyAnimeList update").await);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_is_one_path_segment() {
        let mal = Mal::new(
            reqwest::Client::new(),
            "id".into(),
            None,
            "https://api.myanimelist.net".into(),
            "https://myanimelist.net".into(),
        );
        let url = mal.list_url("../anime/1/my_list_status#x?y").unwrap();
        assert_eq!(url.host_str(), Some("api.myanimelist.net"));
        assert_eq!(
            url.path(),
            "/v2/users/..%2Fanime%2F1%2Fmy_list_status%23x%3Fy/animelist"
        );
        assert!(url.query().unwrap().contains("limit=1000"));
        assert_eq!(
            mal.list_url("@me").unwrap().path(),
            "/v2/users/@me/animelist"
        );
    }
}
