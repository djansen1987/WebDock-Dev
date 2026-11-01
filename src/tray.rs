//! Windows tray icon + "start with Windows" toggle (HKCU Run key, no admin rights needed).

use std::sync::mpsc::Sender;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
use winreg::RegKey;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "WebDockDev";

pub fn autostart_enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, KEY_READ)
        .and_then(|k| k.get_value::<String, _>(RUN_NAME))
        .is_ok()
}

pub fn set_autostart(on: bool) -> std::io::Result<()> {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_WRITE)?;
    if on {
        let exe = std::env::current_exe()?;
        key.set_value(RUN_NAME, &format!("\"{}\" --background", exe.display()))
    } else {
        match key.delete_value(RUN_NAME) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

fn icon() -> Icon {
    let img = include_bytes!("../assets/tray.rgba");
    Icon::from_rgba(img.to_vec(), 32, 32).expect("tray icon")
}

/// Runs the tray event loop on the calling (main) thread. `shutdown` is signalled on "Quit".
pub fn run(port: u16, shutdown: Sender<()>) -> ! {
    let url = format!("http://localhost:{port}");
    let event_loop = EventLoopBuilder::new().build();

    let menu = Menu::new();
    let open = MenuItem::new("Open dashboard", true, None);
    let auto = CheckMenuItem::new("Start with Windows", true, autostart_enabled(), None);
    let quit = MenuItem::new("Quit", true, None);
    let _ = menu.append_items(&[&open, &auto, &PredefinedMenuItem::separator(), &quit]);
    let (open_id, auto_id, quit_id) = (open.id().clone(), auto.id().clone(), quit.id().clone());

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("WebDock Dev")
        .with_icon(icon())
        .build()
        .expect("tray icon");

    let menu_rx = MenuEvent::receiver();
    event_loop.run(move |_event: Event<()>, _, flow| {
        *flow = ControlFlow::WaitUntil(std::time::Instant::now() + std::time::Duration::from_millis(200));
        while let Ok(ev) = menu_rx.try_recv() {
            if ev.id == open_id {
                crate::proc::open_url(&url);
            } else if ev.id == auto_id {
                if let Err(e) = set_autostart(auto.is_checked()) {
                    tracing::error!("autostart: {e}");
                    auto.set_checked(autostart_enabled());
                }
            } else if ev.id == quit_id {
                let _ = shutdown.send(());
                *flow = ControlFlow::Exit;
            }
        }
    })
}
