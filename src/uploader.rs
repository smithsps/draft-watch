use anyhow::Result;
use reqwest::Client;
use std::time::Duration;

use crate::config::Config;

pub struct Uploader {
    client: Client,
    config: Config,
}

impl Uploader {
    pub fn new(config: Config) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { client, config })
    }

    pub async fn upload(&self, ndjson: &str) -> Result<bool> {
        let url = match self.config.upload_url.as_deref().filter(|s| !s.is_empty()) {
            Some(u) => u.to_string(),
            None => return Ok(false),
        };

        let mut req = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-ndjson")
            .body(ndjson.to_string());

        if let Some(key) = self.config.upload_api_key.as_deref().filter(|s| !s.is_empty()) {
            req = req.header("X-Api-Key", key);
        }

        let resp = req.send().await?;
        resp.error_for_status()?;

        tracing::info!("Session uploaded to {url}");
        Ok(true)
    }
}
