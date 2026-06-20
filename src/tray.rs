use anyhow::Result;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

fn data_dir() -> std::path::PathBuf {
    dirs::data_local_dir()
        .expect("no local data dir")
        .join("DraftWatch")
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrayState {
    WaitingForClient,
    ClientConnected,
    InDraft,
}

pub struct Tray {
    icon: TrayIcon,
    status_item: MenuItem,
    count_item: MenuItem,
    view_history_item: MenuItem,
    open_folder_item: MenuItem,
    quit_item: MenuItem,
    state: TrayState,
    viewer_port: Option<u16>,
}

impl Tray {
    pub fn new() -> Result<Self> {
        let status_item = MenuItem::new("Sleeping", false, None);
        let count_item = MenuItem::new("0 matches recorded", false, None);
        let view_history_item = MenuItem::new("View History", true, None);
        let open_folder_item = MenuItem::new("Open Folder", true, None);
        let quit_item = MenuItem::new("Quit", true, None);

        let menu = Menu::new();
        menu.append(&status_item)?;
        menu.append(&count_item)?;
        menu.append(&view_history_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&open_folder_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit_item)?;

        let icon = make_icon(TrayState::WaitingForClient);

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("DraftWatch — Sleeping")
            .with_icon(icon)
            .build()?;

        Ok(Self {
            icon: tray,
            status_item,
            count_item,
            view_history_item,
            open_folder_item,
            quit_item,
            state: TrayState::WaitingForClient,
            viewer_port: None,
        })
    }

    pub fn set_viewer_port(&mut self, port: u16) {
        self.viewer_port = Some(port);
    }

    pub fn set_session_count(&self, n: usize) {
        let label = if n == 1 {
            "1 match recorded".to_string()
        } else {
            format!("{n} matches recorded")
        };
        self.count_item.set_text(label);
    }

    pub fn set_state(&mut self, state: TrayState) {
        if self.state == state {
            return;
        }
        self.state = state;

        let (status, tooltip) = match state {
            TrayState::WaitingForClient => ("Sleeping", "DraftWatch — Sleeping"),
            TrayState::ClientConnected  => ("Watching client", "DraftWatch — Watching client"),
            TrayState::InDraft          => ("Recording draft", "DraftWatch — Recording draft"),
        };

        self.status_item.set_text(status);
        let _ = self.icon.set_icon(Some(make_icon(state)));
        let _ = self.icon.set_tooltip(Some(tooltip));
    }

    pub fn check_quit(&self) -> bool {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == *self.quit_item.id() {
                return true;
            }
            if event.id == *self.open_folder_item.id() {
                let dir = data_dir();
                std::fs::create_dir_all(&dir).ok();
                let _ = std::process::Command::new("explorer").arg(&dir).spawn();
            }
            if event.id == *self.view_history_item.id() {
                if let Some(port) = self.viewer_port {
                    let url = format!("http://127.0.0.1:{port}/");
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", &url])
                        .spawn();
                }
            }
        }
        false
    }
}

static ICON_Z: &[u8] = include_bytes!("../assets/Icon_Z.png");
static ICON_W: &[u8] = include_bytes!("../assets/Icon_W.png");
static ICON_D: &[u8] = include_bytes!("../assets/Icon_D.png");

fn make_icon(state: TrayState) -> Icon {
    let data = match state {
        TrayState::WaitingForClient => ICON_Z,
        TrayState::ClientConnected => ICON_W,
        TrayState::InDraft => ICON_D,
    };
    let img = image::load_from_memory(data).expect("valid png").to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).expect("valid icon")
}
