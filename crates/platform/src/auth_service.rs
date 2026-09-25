//! Internal Better Auth client. Errors deliberately exclude credentials and response bodies.
use crate::Error;
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone)]
pub struct AuthService {
    http: Client,
    base_url: Url,
    token: String,
}
impl AuthService {
    pub fn new(base_url: Url, token: String) -> Result<Self, Error> {
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| unavailable())?;
        Ok(Self {
            http,
            base_url,
            token,
        })
    }
    async fn post(&self, path: &str, body: Value) -> Result<reqwest::Response, Error> {
        let url = self.base_url.join(path).map_err(|_| unavailable())?;
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(unavailable());
        }
        Ok(response)
    }
    pub async fn ensure_user(&self, email: &str, name: &str) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct User {
            id: String,
        }
        let user: User = self
            .post("internal/users", json!({"email": email, "name": name}))
            .await?
            .json()
            .await
            .map_err(|_| unavailable())?;
        if user.id.trim().is_empty() {
            return Err(unavailable());
        }
        Ok(user.id)
    }
    /// A staff sign-in link (15 minutes, single use) for the platform to email itself.
    pub async fn invite_link(&self, email: &str, callback_url: &str) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct Link {
            url: String,
        }
        let link: Link = self
            .post(
                "internal/users/invite",
                json!({"email": email, "callback_url": callback_url, "deliver": false}),
            )
            .await?
            .json()
            .await
            .map_err(|_| unavailable())?;
        if !(link.url.starts_with("http://") || link.url.starts_with("https://")) {
            return Err(unavailable());
        }
        Ok(link.url)
    }

    pub async fn invite(&self, email: &str, callback_url: &str) -> Result<(), Error> {
        self.post(
            "internal/users/invite",
            json!({"email": email, "callback_url": callback_url}),
        )
        .await?;
        Ok(())
    }
}
fn unavailable() -> Error {
    Error::Unavailable("auth service request failed".into())
}
