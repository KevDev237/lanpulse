//! Icône de la zone de notification (Windows, Linux) et de la barre de menus
//! (macOS), avec le débit en temps réel.

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::state::Snapshot;

pub struct TrayItems {
    pub rate: MenuItem<Wry>,
    pub network: MenuItem<Wry>,
}

pub fn human_rate(bps: f64) -> String {
    const U: [&str; 4] = ["o/s", "Ko/s", "Mo/s", "Go/s"];
    let mut v = bps;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} {}", U[i])
    } else if v < 10.0 {
        format!("{v:.1} {}", U[i])
    } else {
        format!("{v:.0} {}", U[i])
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let rate = MenuItem::with_id(app, "rate", "↓ 0 o/s   ↑ 0 o/s", false, None::<&str>)?;
    let network = MenuItem::with_id(app, "net", "Réseau : détection…", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Ouvrir LanPulse", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quitter", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&rate, &network, &sep, &open, &quit])?;

    // macOS : icône monochrome « template » qui s'adapte au thème de la barre.
    #[cfg(target_os = "macos")]
    let icon = Image::from_bytes(include_bytes!("../icons/tray-template.png"))?;
    #[cfg(not(target_os = "macos"))]
    let icon = Image::from_bytes(include_bytes!("../icons/tray.png"))?;

    let builder = TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("LanPulse")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => {
                if let Some(state) = app.try_state::<std::sync::Arc<crate::state::AppState>>() {
                    crate::monitor::flush(&state);
                }
                app.exit(0)
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    #[cfg(target_os = "macos")]
    let builder = builder.icon_as_template(true);
    builder.build(app)?;

    app.manage(TrayItems { rate, network });
    Ok(())
}

pub fn update(app: &AppHandle, s: &Snapshot) {
    let down = human_rate(s.rate_rx);
    let up = human_rate(s.rate_tx);
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.rate.set_text(format!("↓ {down}   ↑ {up}"));
        let net = match &s.network {
            Some(n) => format!(
                "{} · {} pair{} en ligne",
                n.name,
                s.peers_online,
                if s.peers_online > 1 { "s" } else { "" }
            ),
            None => "Hors ligne".to_string(),
        };
        let _ = items.network.set_text(net);
    }
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(format!("LanPulse\n↓ {down}  ↑ {up}")));
        // Le débit est affiché directement dans la barre de menus macOS.
        #[cfg(target_os = "macos")]
        let _ = tray.set_title(Some(format!("↓{down} ↑{up}")));
    }
}
