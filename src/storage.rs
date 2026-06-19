use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use dirs::data_local_dir;
use serde_json::Value;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub struct SessionBuffer {
    lines: Vec<String>,
    seq: u32,
    first_ts: Option<String>,
    last_game_id: Option<i64>,
}

impl SessionBuffer {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            seq: 0,
            first_ts: None,
            last_game_id: None,
        }
    }

    pub fn push(&mut self, event: &Value) {
        let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
        if self.first_ts.is_none() {
            self.first_ts = Some(ts.clone());
        }
        if let Some(id) = event.get("gameId").and_then(|v| v.as_i64()) {
            if id != 0 {
                self.last_game_id = Some(id);
            }
        }
        let line = serde_json::json!({"seq": self.seq, "ts": ts, "event": event});
        self.lines.push(line.to_string());
        self.seq += 1;
    }

    pub fn flush(&mut self) -> Result<PathBuf> {
        let dir = sessions_dir();
        fs::create_dir_all(&dir)?;

        let filename = match self.last_game_id {
            Some(id) => format!("{id}.jsonl"),
            None => {
                let ts = self.first_ts.as_deref().unwrap_or("unknown");
                let safe: String = ts
                    .chars()
                    .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
                    .collect();
                format!("{safe}.jsonl")
            }
        };

        let path = dir.join(&filename);
        let mut content = self.lines.join("\n");
        content.push('\n');
        fs::write(&path, content)?;

        self.lines.clear();
        self.seq = 0;
        self.first_ts = None;
        self.last_game_id = None;

        tracing::info!("Session written to {}", path.display());
        Ok(path)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

pub fn mark_uploaded(filename: &str) -> Result<()> {
    let path = sessions_dir().join("uploaded.txt");
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{filename}")?;
    Ok(())
}

pub fn session_count() -> usize {
    let dir = sessions_dir();
    if !dir.exists() {
        return 0;
    }
    fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
                .count()
        })
        .unwrap_or(0)
}

pub fn pending_files() -> Result<Vec<PathBuf>> {
    let dir = sessions_dir();
    if !dir.exists() {
        return Ok(vec![]);
    }

    let uploaded = uploaded_set()?;
    let mut files = vec![];

    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if !uploaded.contains(&name) {
            files.push(path);
        }
    }

    Ok(files)
}

fn uploaded_set() -> Result<HashSet<String>> {
    let path = sessions_dir().join("uploaded.txt");
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let content = fs::read_to_string(path)?;
    Ok(content.lines().map(str::to_string).collect())
}

fn sessions_dir() -> PathBuf {
    data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("DraftWatch")
        .join("sessions")
}
