use anyhow::Result;
use dirs::config_dir;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Config {
    /// Full URL to POST completed sessions to, e.g. https://mysite.com/api/sessions
    pub upload_url: Option<String>,
    /// Sent as X-Api-Key header if set
    pub upload_api_key: Option<String>,
    /// Override League install directory (auto-detected if unset)
    pub league_path: Option<String>,
}

impl Config {
    pub fn load() -> Self {
        match Self::try_load() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Could not load config: {e}. Using defaults.");
                Self::default()
            }
        }
    }

    fn try_load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            let defaults = Self::default();
            defaults.save_if_missing()?;
            return Ok(defaults);
        }
        let text = fs::read_to_string(&path)?;
        Ok(toml::from_str(&text)?)
    }

    fn save_if_missing(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = format!(
            "# DraftWatch config\n\
             # upload_url = \"https://your-site.com/api/sessions\"\n\
             # upload_api_key = \"your-key\"\n\
             # league_path = \"C:\\\\Riot Games\\\\League of Legends\"\n"
        );
        fs::write(&path, text)?;
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("DraftWatch")
        .join("config.toml")
}
