//! Push notifications through [ntfy](https://ntfy.sh): the server publishes each
//! new-episode notification to a topic, and the ntfy app on your phone shows it.

use anyhow::{Context, Result};
use serde_json::json;

use crate::model::Notification;

#[derive(Clone)]
pub struct Ntfy {
    http: reqwest::Client,
    /// Server base URL, e.g. `https://ntfy.sh`.
    server: String,
    topic: String,
    /// Access token for protected topics.
    token: Option<String>,
    /// Link to this app, added as a button when it is reachable from other devices.
    app_url: Option<String>,
}

impl Ntfy {
    pub fn new(
        http: reqwest::Client,
        server: &str,
        topic: String,
        token: Option<String>,
        public_url: &str,
    ) -> Self {
        Self {
            http,
            server: server.trim_end_matches('/').to_string(),
            topic,
            token,
            app_url: reachable_elsewhere(public_url).then(|| public_url.to_string()),
        }
    }

    /// Publish one notification. Uses ntfy's JSON API, so titles with non-ASCII
    /// characters arrive intact (HTTP headers can't carry them reliably).
    ///
    /// # Errors
    /// Fails if the ntfy server can't be reached or rejects the message.
    pub async fn send(&self, n: &Notification) -> Result<()> {
        let mut actions = Vec::new();
        if let Some(watch) = &n.watch {
            actions.push(json!({ "action": "view", "label": format!("Watch on {}", watch.site), "url": watch.url }));
        }
        if let Some(app) = &self.app_url {
            actions.push(json!({ "action": "view", "label": "Open list", "url": app }));
        }
        let click = n
            .watch
            .as_ref()
            .map(|w| w.url.as_str())
            .or(self.app_url.as_deref());
        let body = json!({
            "topic": self.topic,
            "title": n.title,
            "message": n.message,
            "tags": ["tv"],
            "click": click,
            "icon": n.cover,
            "actions": actions,
        });

        let mut req = self.http.post(&self.server).json(&body);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.context("ntfy request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("ntfy rejected the notification (HTTP {})", resp.status());
        }
        Ok(())
    }
}

/// Whether a URL points somewhere a phone could reach (not this machine's loopback).
fn reachable_elsewhere(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        !matches!(
            u.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "0.0.0.0") | None
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_urls_are_not_linked() {
        assert!(!reachable_elsewhere("http://127.0.0.1:3000"));
        assert!(!reachable_elsewhere("http://localhost:3000"));
        assert!(!reachable_elsewhere("http://[::1]:3000"));
        assert!(reachable_elsewhere("http://192.168.1.20:3000"));
        assert!(reachable_elsewhere("https://anime.example.com"));
    }
}
