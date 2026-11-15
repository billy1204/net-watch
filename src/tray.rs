//! System tray icon (StatusNotifierItem) whose color shows the connection,
//! desktop pop-ups, and starting at login.
use crate::monitor::{Core, Status};
use chrono::{Duration as Span, Local};
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{Icon, MenuItem, ToolTip};
use std::path::PathBuf;
use std::sync::OnceLock;

static HANDLE: OnceLock<ksni::blocking::Handle<Tray>> = OnceLock::new();

pub struct Tray {
    core: Core,
}

/// Show the tray icon. False when the desktop has no tray to show it in.
pub fn start(core: Core) -> bool {
    use ksni::blocking::TrayMethods;
    match (Tray { core }).spawn() {
        Ok(handle) => HANDLE.set(handle).is_ok(),
        Err(e) => {
            eprintln!("net-watch: no system tray available ({e}); closing the window minimizes it");
            false
        }
    }
}

/// Redraw the icon, tooltip and menu from the shared state.
pub fn refresh() {
    if let Some(handle) = HANDLE.get() {
        handle.update(|_| {});
    }
}

fn icons(status: Status) -> Vec<Icon> {
    [22u32, 32, 64]
        .into_iter()
        .map(|size| {
            let rgba = crate::icon::status_rgba(size, status);
            // RGBA -> ARGB (network byte order) as StatusNotifierItem expects.
            let data = rgba.chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
            Icon { width: size as i32, height: size as i32, data }
        })
        .collect()
}

fn item(label: &str, icon: &str, enabled: bool, activate: impl Fn(&mut Tray) + Send + 'static) -> MenuItem<Tray> {
    StandardItem { label: label.into(), icon_name: icon.into(), enabled, activate: Box::new(activate), ..Default::default() }.into()
}

fn info(label: String) -> MenuItem<Tray> {
    StandardItem { label, enabled: false, ..Default::default() }.into()
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "net-watch".into()
    }
    fn title(&self) -> String {
        "Net Watch".into()
    }
    fn icon_pixmap(&self) -> Vec<Icon> {
        let status = self.core.state.lock().unwrap().status();
        static CACHE: OnceLock<Vec<(Status, Vec<Icon>)>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| {
            [Status::Starting, Status::Up, Status::Shaky, Status::Down, Status::Paused].into_iter().map(|s| (s, icons(s))).collect()
        });
        cache.iter().find(|(s, _)| *s == status).map(|(_, i)| i.clone()).unwrap_or_default()
    }
    fn tool_tip(&self) -> ToolTip {
        let st = self.core.state.lock().unwrap();
        let mut description = crate::headline(&st);
        if let Some(line) = live_line(&st) {
            description += &format!("\n{line}");
        }
        ToolTip { title: "Net Watch".into(), description, ..Default::default() }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        self.core.toggle_window();
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let st = self.core.state.lock().unwrap();
        let mut items = vec![info(crate::headline(&st))];
        if let Some(line) = live_line(&st) {
            items.push(info(line));
        }
        match st.speeds.last() {
            Some(s) if s.error.is_some() => {
                items.push(info(format!("Last speed test failed ({})", crate::when(s.t))));
            }
            Some(s) => items.push(info(format!("Last speed test: {:.0} ↓ / {:.0} ↑ Mbps", s.down, s.up))),
            None => {}
        }
        if let Some(o) = st.outages.last() {
            items.push(info(format!("Last outage: {} ({})", crate::when(o.start), crate::span(o.secs(Local::now())))));
        }
        let paused = st.paused_until.is_some();
        let testing = st.testing;
        let ready = crate::ookla::ready(&st.prefs);
        drop(st);
        items.extend([
            MenuItem::Separator,
            if self.core.window_open() {
                item("Hide Net Watch", "window-close", true, |t| t.core.toggle_window())
            } else {
                item("Open Net Watch", "window-new", true, |t| t.core.show_window())
            },
            match (testing, ready) {
                (true, _) => item("Testing speed…", "network-transmit-receive", false, |_| {}),
                (false, true) => item("Test Speed Now", "network-transmit-receive", !paused, |t| t.core.test_or_set_up()),
                (false, false) => item("Set Up Speed Tests…", "network-transmit-receive", true, |t| t.core.test_or_set_up()),
            },
            if paused {
                item("Resume Checks", "media-playback-start", true, |t| t.core.pause(None))
            } else {
                item("Pause Checks for 1 Hour", "media-playback-pause", true, |t| t.core.pause(Some(Local::now() + Span::hours(1))))
            },
            MenuItem::Separator,
            CheckmarkItem {
                label: "Start When I Log In".into(),
                checked: autostart(),
                activate: Box::new(|_: &mut Self| {
                    let _ = set_autostart(!autostart());
                }),
                ..Default::default()
            }
            .into(),
            item("Quit", "application-exit", true, |t| t.core.quit()),
        ]);
        items
    }
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        // Keep the item; it reappears when the panel comes back.
        true
    }
}

/// "Speed test: download 812 Mbps · 0:12 left" while one runs.
fn live_line(st: &crate::monitor::Shared) -> Option<String> {
    let live = st.live.as_ref().filter(|_| st.testing)?;
    let now = match live.step {
        "download" => live.down.last().map(|s| format!("download {:.0} Mbps", s.1)),
        "upload" => live.up.last().map(|s| format!("upload {:.0} Mbps", s.1)),
        _ => None,
    };
    Some(format!("Speed test: {} · {} left", now.unwrap_or_else(|| "starting".into()), crate::test_left(st)))
}

/// A desktop pop-up through the notification service.
pub fn notify(title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    // Off the caller's thread: the session bus can be slow to answer.
    std::thread::spawn(move || {
        let Ok(conn) = zbus::blocking::Connection::session() else { return };
        let hints: std::collections::HashMap<&str, zbus::zvariant::Value> =
            [("desktop-entry", zbus::zvariant::Value::from("net-watch"))].into_iter().collect();
        let _ = conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("Net Watch", 0u32, "network-wireless", title.as_str(), body.as_str(), Vec::<&str>::new(), hints, -1i32),
        );
    });
}

fn autostart_file() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("autostart").join("net-watch.desktop"))
}

pub fn autostart() -> bool {
    autostart_file().is_some_and(|p| p.exists())
}

/// Add or remove the login entry. It starts the AppImage when running from
/// one (its own path changes with every mount), else this program.
pub fn set_autostart(on: bool) -> std::io::Result<()> {
    let path = autostart_file().ok_or_else(|| std::io::Error::other("no home folder"))?;
    if !on {
        return std::fs::remove_file(path).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) });
    }
    let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).map_or_else(std::env::current_exe, Ok)?;
    let exe = exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(
        path,
        format!(
            "[Desktop Entry]\nType=Application\nName=Net Watch\nComment=Keeps a record of internet outages and speed\n\
             Exec=\"{exe}\" --hidden\nIcon=net-watch\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        ),
    )
}
