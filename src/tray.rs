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
    InDraftWatch,
}

pub struct Tray {
    icon: TrayIcon,
    count_item: MenuItem,
    open_folder_item: MenuItem,
    quit_item: MenuItem,
    state: TrayState,
}

impl Tray {
    pub fn new() -> Result<Self> {
        let status_item = MenuItem::new("DraftWatch Monitor", false, None);
        let count_item = MenuItem::new("0 matches recorded", false, None);
        let open_folder_item = MenuItem::new("Open Folder", true, None);
        let quit_item = MenuItem::new("Quit", true, None);

        let menu = Menu::new();
        menu.append(&status_item)?;
        menu.append(&count_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&open_folder_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit_item)?;

        let icon = make_icon(TrayState::WaitingForClient);

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("DraftWatch — waiting for League Client")
            .with_icon(icon)
            .build()?;

        Ok(Self {
            icon: tray,
            count_item,
            open_folder_item,
            quit_item,
            state: TrayState::WaitingForClient,
        })
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

        let tooltip = match state {
            TrayState::WaitingForClient => "DraftWatch — waiting for League Client",
            TrayState::ClientConnected => "DraftWatch — client connected",
            TrayState::InDraftWatch => "DraftWatch — in champion select!",
        };

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
        }
        false
    }
}

/// Solid-colour 16×16 RGBA icon: grey / green / blue by state.
fn make_icon(state: TrayState) -> Icon {
    let color: [u8; 4] = match state {
        TrayState::WaitingForClient => [120, 120, 120, 255],
        TrayState::ClientConnected => [40, 180, 80, 255],
        TrayState::InDraftWatch => [30, 120, 255, 255],
    };
    let pixels: Vec<u8> = (0..16 * 16).flat_map(|_| color).collect();
    Icon::from_rgba(pixels, 16, 16).expect("valid icon")
}
