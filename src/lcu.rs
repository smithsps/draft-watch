use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use native_tls::TlsConnector;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::client::IntoClientRequest,
    Connector,
};
use tracing::{info, warn};

#[derive(Debug)]
pub enum LcuEvent {
    Connected,
    Disconnected,
    SessionUpdate(Value),
    SessionDeleted,
}

pub struct Credentials {
    pub port: u16,
    pub token: String,
}

pub async fn monitor(tx: mpsc::Sender<LcuEvent>) {
    loop {
        let creds = loop {
            match find_credentials() {
                Ok(c) => break c,
                Err(_) => tokio::time::sleep(Duration::from_secs(5)).await,
            }
        };

        info!("League Client found on port {}", creds.port);
        let _ = tx.send(LcuEvent::Connected).await;

        if let Err(e) = run_websocket(&creds, &tx).await {
            warn!("WebSocket error: {e}");
        }

        let _ = tx.send(LcuEvent::Disconnected).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn run_websocket(creds: &Credentials, tx: &mpsc::Sender<LcuEvent>) -> Result<()> {
    let auth = STANDARD.encode(format!("riot:{}", creds.token));

    let tls = TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .build()?;

    let mut request = format!("wss://127.0.0.1:{}/", creds.port).into_client_request()?;
    request.headers_mut().insert(
        "Authorization",
        format!("Basic {auth}").parse()?,
    );

    let (mut ws, _) =
        connect_async_tls_with_config(request, None, false, Some(Connector::NativeTls(tls)))
            .await?;

    // Subscribe to champ select session events
    let sub = serde_json::json!([5, "OnJsonApiEvent_lol-champ-select_v1_session"]);
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        sub.to_string(),
    ))
    .await?;

    info!("Subscribed to champ select events");

    while let Some(msg) = ws.next().await {
        let text = match msg? {
            tokio_tungstenite::tungstenite::Message::Text(t) => t,
            tokio_tungstenite::tungstenite::Message::Close(_) => break,
            _ => continue,
        };

        let Ok(arr) = serde_json::from_str::<Value>(&text) else {
            continue;
        };

        // LCU WAMP-like events: [opcode, topic, data]
        // opcode 8 = event
        let opcode = arr.get(0).and_then(|v| v.as_u64()).unwrap_or(0);
        if opcode != 8 {
            continue;
        }

        let Some(payload) = arr.get(2) else { continue };

        let event_type = payload
            .get("eventType")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match event_type {
            "Delete" => {
                let _ = tx.send(LcuEvent::SessionDeleted).await;
            }
            "Create" | "Update" => {
                if let Some(data) = payload.get("data") {
                    let _ = tx.send(LcuEvent::SessionUpdate(data.clone())).await;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn find_credentials() -> Result<Credentials> {
    find_lockfile().and_then(|p| parse_lockfile(&p))
}

fn find_lockfile() -> Result<PathBuf> {
    use sysinfo::System;

    // Try to find by process first
    let sys = System::new_all();
    for (_, proc) in sys.processes() {
        let name = proc.name().to_string_lossy().to_lowercase();
        if name.starts_with("leagueclient") && !name.contains("helper") {
            if let Some(exe) = proc.exe() {
                if let Some(dir) = exe.parent() {
                    let lf = dir.join("lockfile");
                    if lf.exists() {
                        return Ok(lf);
                    }
                }
            }
        }
    }

    // Fallback: common install paths
    let candidates = [
        r"C:\Riot Games\League of Legends\lockfile",
        r"D:\Riot Games\League of Legends\lockfile",
        r"C:\Games\Riot Games\League of Legends\lockfile",
        r"C:\Program Files\Riot Games\League of Legends\lockfile",
    ];
    for path in candidates {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }

    Err(anyhow!("League Client not running (lockfile not found)"))
}

fn parse_lockfile(path: &PathBuf) -> Result<Credentials> {
    // Format: name:pid:port:password:protocol
    let text = std::fs::read_to_string(path)?;
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() < 4 {
        return Err(anyhow!("Unexpected lockfile format"));
    }
    Ok(Credentials {
        port: parts[2].parse()?,
        token: parts[3].to_string(),
    })
}
