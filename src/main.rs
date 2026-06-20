// Remove this attribute when building a release binary (hides the console window):
// #![windows_subsystem = "windows"]

use std::sync::mpsc;
use std::time::Duration;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod lcu;
mod storage;
mod tray;
mod uploader;
mod viewer;

use config::Config;
use lcu::LcuEvent;
use tray::{Tray, TrayState};

enum AppMsg {
    Lcu(LcuEvent),
    SessionsRecorded(usize),
    ViewerReady(u16),
}

fn main() {
    let log_dir = dirs::data_local_dir()
        .expect("no local data dir")
        .join("DraftWatch");
    std::fs::create_dir_all(&log_dir).expect("create log dir");

    let file_appender = tracing_appender::rolling::never(&log_dir, "app.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let filter = tracing_subscriber::EnvFilter::new("draft_watch=info");
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(non_blocking))
        .init();

    let config = Config::load();
    info!("Config: {:?}", config);

    let (app_tx, app_rx) = mpsc::channel::<AppMsg>();

    // Background thread owns the async runtime + storage + uploader.
    std::thread::spawn({
        let config = config.clone();
        move || {
            let lcu_config = config.clone();
            let uploader = uploader::Uploader::new(config).expect("create uploader");

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");

            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let port = viewer::start().await;
                let _ = app_tx.send(AppMsg::ViewerReady(port));

                let (lcu_tx, mut lcu_rx) = tokio::sync::mpsc::channel::<LcuEvent>(64);

                tokio::task::spawn_local(lcu::monitor(lcu_tx, lcu_config));

                retry_pending_uploads(&uploader).await;

                let _ = app_tx.send(AppMsg::SessionsRecorded(storage::session_count()));

                let mut session_buffer = storage::SessionBuffer::new();

                while let Some(event) = lcu_rx.recv().await {
                    // Forward tray state hints to the main thread
                    let hint = match &event {
                        LcuEvent::Connected => Some(LcuEvent::Connected),
                        LcuEvent::Disconnected => Some(LcuEvent::Disconnected),
                        LcuEvent::SessionUpdate(_) => Some(LcuEvent::SessionUpdate(serde_json::Value::Null)),
                        LcuEvent::SessionDeleted => Some(LcuEvent::SessionDeleted),
                    };
                    if let Some(h) = hint {
                        let _ = app_tx.send(AppMsg::Lcu(h));
                    }

                    match event {
                        LcuEvent::SessionUpdate(data) => {
                            let phase = data
                                .get("timer")
                                .and_then(|t| t.get("phase"))
                                .and_then(|p| p.as_str())
                                .unwrap_or("");

                            session_buffer.push(&data);

                            if phase == "GAME_STARTING" {
                                commit_session(&mut session_buffer, &uploader, &app_tx).await;
                            }
                        }
                        LcuEvent::SessionDeleted => {
                            if !session_buffer.is_empty() {
                                commit_session(&mut session_buffer, &uploader, &app_tx).await;
                            }
                        }
                        _ => {}
                    }
                }
            });
        }
    });

    // Main thread: Windows message loop + tray icon.
    let mut tray = Tray::new().expect("create tray icon");

    loop {
        pump_messages();

        if tray.check_quit() {
            info!("Quit requested");
            break;
        }

        while let Ok(msg) = app_rx.try_recv() {
            match msg {
                AppMsg::Lcu(LcuEvent::Connected) => tray.set_state(TrayState::ClientConnected),
                AppMsg::Lcu(LcuEvent::Disconnected) => tray.set_state(TrayState::WaitingForClient),
                AppMsg::Lcu(LcuEvent::SessionUpdate(_)) => tray.set_state(TrayState::InDraft),
                AppMsg::Lcu(LcuEvent::SessionDeleted) => tray.set_state(TrayState::ClientConnected),
                AppMsg::SessionsRecorded(n) => tray.set_session_count(n),
                AppMsg::ViewerReady(port) => tray.set_viewer_port(port),
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

async fn commit_session(
    buffer: &mut storage::SessionBuffer,
    uploader: &uploader::Uploader,
    tx: &mpsc::Sender<AppMsg>,
) {
    let path = match buffer.flush() {
        Ok(p) => p,
        Err(e) => {
            error!("Storage error: {e}");
            return;
        }
    };

    let _ = tx.send(AppMsg::SessionsRecorded(storage::session_count()));

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    let jsonl = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            error!("Read error for {filename}: {e}");
            return;
        }
    };

    match uploader.upload(&jsonl).await {
        Ok(true) => {
            if let Err(e) = storage::mark_uploaded(&filename) {
                error!("Failed to mark {filename} uploaded: {e}");
            }
        }
        Ok(false) => {
            info!("No upload URL configured — session stored locally only");
        }
        Err(e) => {
            error!("Upload error: {e}");
        }
    }
}

async fn retry_pending_uploads(uploader: &uploader::Uploader) {
    let files = match storage::pending_files() {
        Ok(f) => f,
        Err(e) => {
            error!("Failed to scan pending sessions: {e}");
            return;
        }
    };

    if files.is_empty() {
        return;
    }

    info!("Retrying {} pending upload(s)", files.len());

    for path in files {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        let jsonl = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to read {filename}: {e}");
                continue;
            }
        };

        match uploader.upload(&jsonl).await {
            Ok(true) => {
                if let Err(e) = storage::mark_uploaded(&filename) {
                    error!("Failed to mark {filename} uploaded: {e}");
                }
                info!("Retry uploaded: {filename}");
            }
            Ok(false) => break, // no URL configured, no point continuing
            Err(e) => {
                error!("Retry upload failed for {filename}: {e}");
            }
        }
    }
}

fn pump_messages() {
    #[cfg(windows)]
    unsafe {
        use winapi::um::winuser::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
