//! System tray icon: tray-icon on Windows/macOS, StatusNotifierItem (ksni) on Linux.
use crate::state::AppState;
use std::sync::Arc;

#[cfg(any(windows, target_os = "macos"))]
pub struct Tray {
    _icon: tray_icon::TrayIcon,
}

#[cfg(any(windows, target_os = "macos"))]
pub fn create(app: Arc<AppState>) -> Option<Tray> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let menu = Menu::new();
    let open = MenuItem::new("Open Edgehop", true, None);
    let restart = MenuItem::new("Restart Edgehop", true, None);
    let quit = MenuItem::new("Quit Edgehop", true, None);
    menu.append(&open).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    menu.append(&restart).ok()?;
    menu.append(&quit).ok()?;
    let (open_id, quit_id, restart_id) = (open.id().clone(), quit.id().clone(), restart.id().clone());

    let a = app.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if e.id == open_id {
            crate::platform::show_window(&a);
        } else if e.id == restart_id {
            a.stop_engine();
            crate::recovery::restart_self("restart requested from the tray");
        } else if e.id == quit_id {
            a.quit();
        }
    }));
    let a = app.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            crate::platform::show_window(&a);
        }
    }));

    let icon = Icon::from_rgba(crate::icon::rgba(32), 32, 32).ok()?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Edgehop")
        .with_icon(icon)
        .build()
        .map_err(|e| log::warn!("tray icon unavailable: {e}"))
        .ok()?;
    Some(Tray { _icon: tray })
}

#[cfg(target_os = "linux")]
pub struct Tray;

#[cfg(target_os = "linux")]
struct KTray {
    app: Arc<AppState>,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for KTray {
    fn id(&self) -> String {
        "Edgehop".into()
    }
    fn title(&self) -> String {
        "Edgehop".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let rgba = crate::icon::rgba(32);
        let argb = rgba.chunks(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
        vec![ksni::Icon { width: 32, height: 32, data: argb }]
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        crate::platform::show_window(&self.app);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            StandardItem {
                label: "Open Edgehop".into(),
                activate: Box::new(|t: &mut Self| crate::platform::show_window(&t.app)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Restart Edgehop".into(),
                activate: Box::new(|t: &mut Self| {
                    t.app.stop_engine();
                    crate::recovery::restart_self("restart requested from the tray")
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit Edgehop".into(),
                activate: Box::new(|t: &mut Self| t.app.quit()),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(target_os = "linux")]
pub fn create(app: Arc<AppState>) -> Option<Tray> {
    let service = ksni::TrayService::new(KTray { app });
    service.spawn();
    Some(Tray)
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub struct Tray;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn create(_app: Arc<AppState>) -> Option<Tray> {
    None
}
