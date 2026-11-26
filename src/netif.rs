//! The computer's network adapters: what each one is, its settings and the
//! Wi-Fi networks nearby; and changing them, with the old settings put back
//! if the internet doesn't come back. Through NetworkManager's `nmcli` on
//! Linux (`netif_nm.rs`), and IP Helper, the WLAN API and PowerShell on
//! Windows (`netif_win.rs`).
use crate::monitor::Core;
use crate::store::Time;
use chrono::Local;
use std::net::{Ipv4Addr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
#[path = "netif_nm.rs"]
mod backend;
#[cfg(windows)]
#[path = "netif_win.rs"]
mod backend;

/// The route metric "Prefer this connection" sets: below what the system
/// gives adapters of its own accord.
#[cfg(target_os = "linux")]
pub const PREFER_METRIC: i64 = 50;
#[cfg(windows)]
pub const PREFER_METRIC: i64 = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Ethernet,
    Wifi,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Ethernet => "Ethernet",
            Kind::Wifi => "Wi-Fi",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Adapter {
    /// Its name to the system, like "enp6s0" or "Wi-Fi".
    pub dev: String,
    pub kind: Kind,
    /// NetworkManager's word: "connected", "disconnected", "unavailable"
    /// (no cable, or switched off) and so on.
    pub state: String,
    /// Maker and model, like "Realtek RTL8125".
    pub model: String,
    pub mac: String,
    /// The settings it uses; for unplugged Ethernet, the ones it would use.
    pub profile: Option<Profile>,
    /// `profile` is in use on it now.
    pub active: bool,
    /// "192.168.1.42/24".
    pub address: Option<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    /// Link speed in Mbps, for Ethernet.
    pub speed: Option<u32>,
    pub mtu: Option<u32>,
    /// The internet goes through it.
    pub primary: bool,
    /// The Wi-Fi network it's on.
    pub wifi: Option<WifiNet>,
}

impl Adapter {
    pub fn connected(&self) -> bool {
        self.state == "connected"
    }

    /// No cable, or the radio is off: it can't be turned on from here.
    pub fn unavailable(&self) -> bool {
        self.state == "unavailable"
    }
}

/// The settings NetworkManager keeps for a connection (IPv4 only).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    pub uuid: String,
    pub name: String,
    /// A fixed address rather than one from the router.
    pub manual: bool,
    /// "192.168.1.60/24"; the first, when there are several.
    pub address: String,
    pub gateway: String,
    pub dns: Vec<String>,
    /// Use only `dns`, not the router's.
    pub ignore_auto_dns: bool,
    pub autoconnect: bool,
    /// Route metric: lower wins when two adapters are connected; -1 is
    /// NetworkManager's default (100 for Ethernet, 600 for Wi-Fi).
    pub metric: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WifiNet {
    pub ssid: String,
    /// "WPA2", "WPA1 WPA2", "WPA3", "" for none.
    pub security: String,
    pub freq: u32,
    pub signal: u8,
    pub in_use: bool,
    /// Its saved settings, when it's been joined before.
    pub saved: Option<String>,
}

impl WifiNet {
    pub fn band(&self) -> &'static str {
        match self.freq {
            f if f >= 5900 => "6 GHz",
            f if f >= 4900 => "5 GHz",
            _ => "2.4 GHz",
        }
    }

    pub fn open(&self) -> bool {
        self.security.is_empty()
    }

    /// Needs a user name and certificates, as at work or school.
    pub fn enterprise(&self) -> bool {
        self.security.contains("802.1X")
    }
}

/// Where the adapters stand, in the shared state.
#[derive(Default)]
pub struct Net {
    pub adapters: Option<Result<Vec<Adapter>, String>>,
    /// The last Wi-Fi scan: which adapter, what it found and when.
    pub wifi: Option<(String, Result<Vec<WifiNet>, String>, Time)>,
    pub scanning: bool,
    pub trial: Option<Trial>,
    /// A change that needs no undo, while it's being made.
    pub working: Option<String>,
    /// How the last such change went, and whether it's a problem.
    pub note: Option<(String, bool)>,
}

/// A change being tried: the old settings come back unless the internet
/// answers within `secs`.
pub struct Trial {
    pub what: String,
    /// When the waiting started.
    pub since: Time,
    pub secs: u64,
    pub stage: Stage,
    /// From the window: `true` keeps it now, `false` undoes it now.
    pub decision: Option<bool>,
}

pub enum Stage {
    Applying,
    Waiting,
    Kept(String),
    Undone(String),
    Failed(String),
}

impl Trial {
    pub fn running(&self) -> bool {
        matches!(self.stage, Stage::Applying | Stage::Waiting)
    }
}

/// What the window asks for.
pub enum Do {
    Scan(String),
    Apply { dev: String, old: Profile, new: Profile, active: bool, primary: bool },
    /// Join a Wi-Fi network: one saved before (`saved`), or a new one.
    Join { dev: String, ssid: String, security: String, password: String, auto: bool, hidden: bool, saved: Option<String> },
    Disconnect(String),
    Connect(String),
    Forget(String, String),
    Keep,
    Undo,
    /// Clear a finished trial or note.
    Dismiss,
}

/// Look again every so often, for Home's adapter line.
pub fn watch(core: Core) {
    loop {
        refresh(&core);
        if core.state.lock().unwrap().quit {
            return;
        }
        std::thread::sleep(Duration::from_secs(15));
    }
}

pub fn run(core: &Core, what: Do) {
    let core = core.clone();
    match what {
        Do::Keep | Do::Undo => {
            if let Some(t) = core.state.lock().unwrap().net.trial.as_mut() {
                t.decision = Some(matches!(what, Do::Keep));
            }
        }
        Do::Dismiss => {
            let mut st = core.state.lock().unwrap();
            if st.net.trial.as_ref().is_some_and(|t| !t.running()) {
                st.net.trial = None;
            }
            st.net.note = None;
        }
        Do::Scan(dev) => {
            std::thread::spawn(move || scan(&core, &dev, true));
        }
        what => {
            std::thread::spawn(move || change(&core, what));
        }
    }
}

fn refresh(core: &Core) {
    let found = backend::adapters();
    core.state.lock().unwrap().net.adapters = Some(found);
    core.repaint();
}

fn scan(core: &Core, dev: &str, rescan: bool) {
    if std::mem::replace(&mut core.state.lock().unwrap().net.scanning, true) {
        return;
    }
    core.repaint();
    let saved = backend::saved_wifi();
    let mark = |found: Result<Vec<WifiNet>, String>| {
        found.map(|mut list| {
            for n in &mut list {
                n.saved = saved.iter().find(|(s, _)| *s == n.ssid).map(|(_, u)| u.clone());
            }
            list
        })
    };
    // What it saw last, at once; then a fresh look, which takes a few seconds.
    if rescan && let Ok(quick) = backend::networks(dev, false) {
        core.state.lock().unwrap().net.wifi = Some((dev.to_string(), mark(Ok(quick)), Local::now()));
        core.repaint();
    }
    let found = mark(backend::networks(dev, rescan));
    let mut st = core.state.lock().unwrap();
    st.net.wifi = Some((dev.to_string(), found, Local::now()));
    st.net.scanning = false;
    drop(st);
    core.repaint();
}

fn change(core: &Core, what: Do) {
    {
        let st = core.state.lock().unwrap();
        if st.net.trial.as_ref().is_some_and(Trial::running) || st.net.working.is_some() {
            return;
        }
    }
    let previous = |dev: &str| -> Option<String> {
        let st = core.state.lock().unwrap();
        let Some(Ok(list)) = &st.net.adapters else { return None };
        list.iter().find(|a| a.dev == dev && a.active).and_then(|a| a.profile.as_ref()).map(|p| p.uuid.clone())
    };
    let back_to = |dev: String, prev: Option<String>| move || match prev {
        Some(uuid) => backend::up(&uuid, &dev),
        None => backend::disconnect(&dev),
    };
    match what {
        Do::Apply { dev, old, new, active, primary } => {
            let apply = {
                let (dev, new) = (dev.clone(), new.clone());
                move || backend::save_profile(&new, active.then_some(dev.as_str()))
            };
            let undo = move || backend::save_profile(&old, active.then_some(dev.as_str()));
            if active && primary {
                trial(core, format!("the new settings for {}", new.name), apply, undo);
            } else {
                quietly(core, "Saving the settings…", "Settings saved.", apply);
            }
        }
        Do::Join { dev, ssid, security, password, auto, hidden, saved } => {
            let prev = previous(&dev);
            let created = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
            let apply = {
                let (dev, created) = (dev.clone(), created.clone());
                let ssid = ssid.clone();
                move || match saved {
                    Some(uuid) => backend::up(&uuid, &dev),
                    None => {
                        let uuid = backend::add_wifi(&dev, &ssid, &security, &password, auto, hidden)?;
                        *created.lock().unwrap() = Some(uuid.clone());
                        backend::up(&uuid, &dev)
                    }
                }
            };
            let undo = {
                let back = back_to(dev.clone(), prev);
                move || {
                    // A new network that didn't work isn't kept.
                    if let Some(uuid) = created.lock().unwrap().take() {
                        let _ = backend::delete(&uuid);
                    }
                    back()
                }
            };
            trial(core, format!("joining {ssid}"), apply, undo);
        }
        Do::Disconnect(dev) => {
            let primary = is_primary(&dev);
            let prev = previous(&dev);
            let apply = {
                let dev = dev.clone();
                move || backend::disconnect(&dev)
            };
            if primary {
                trial(core, format!("turning off {dev}"), apply, back_to(dev, prev));
            } else {
                quietly(core, "Turning it off…", "Turned off.", apply);
            }
        }
        Do::Connect(dev) => quietly(core, "Turning it on…", "Turned on.", move || backend::connect(&dev)),
        Do::Forget(uuid, name) => quietly(core, "Forgetting…", &format!("Forgot {name}."), move || backend::delete(&uuid)),
        _ => {}
    }
    refresh(core);
    let wifi_dev = core.state.lock().unwrap().net.wifi.as_ref().map(|w| w.0.clone());
    if let Some(dev) = wifi_dev {
        scan(core, &dev, false);
    }
}

/// A change that can't cut this computer off: just make it.
fn quietly(core: &Core, doing: &str, done: &str, apply: impl FnOnce() -> Result<(), String>) {
    core.state.lock().unwrap().net.working = Some(doing.into());
    core.repaint();
    let result = apply();
    let mut st = core.state.lock().unwrap();
    st.net.working = None;
    st.net.note = Some(match result {
        Ok(()) => (done.into(), false),
        Err(e) => (format!("Couldn't do that: {e}"), true),
    });
}

/// Make a change, then wait for the internet to answer; without an answer
/// in time (or when asked), put things back.
fn trial(core: &Core, what: String, apply: impl FnOnce() -> Result<(), String>, undo: impl FnOnce() -> Result<(), String>) {
    let secs = core.state.lock().unwrap().prefs.undo_secs;
    let set = |stage: Stage| {
        let mut st = core.state.lock().unwrap();
        if let Some(t) = st.net.trial.as_mut() {
            if matches!(stage, Stage::Waiting) {
                t.since = Local::now();
            }
            t.stage = stage;
        }
        drop(st);
        core.repaint();
    };
    core.state.lock().unwrap().net.trial = Some(Trial { what: what.clone(), since: Local::now(), secs, stage: Stage::Applying, decision: None });
    core.repaint();
    if let Err(e) = apply() {
        let back = match undo() {
            Ok(()) => "The old settings are back.".to_string(),
            Err(u) => format!("Putting the old settings back didn't work either: {u}"),
        };
        set(Stage::Failed(format!("Couldn't apply {what}: {e}. {back}")));
        return;
    }
    set(Stage::Waiting);
    let start = Instant::now();
    let stage = loop {
        match core.state.lock().unwrap().net.trial.as_ref().and_then(|t| t.decision) {
            Some(true) => break Stage::Kept("Kept. The change is saved.".into()),
            Some(false) => {
                break match undo() {
                    Ok(()) => Stage::Undone("You chose Undo now, so the old settings are back.".into()),
                    Err(e) => Stage::Failed(format!("Couldn't put the old settings back: {e}")),
                };
            }
            None => {}
        }
        if let Some(ms) = online() {
            break Stage::Kept(format!("The internet answered in {ms} ms. The change is saved."));
        }
        if start.elapsed() >= Duration::from_secs(secs) {
            break match undo() {
                Ok(()) => Stage::Undone(format!("The internet didn't come back within {}, so the old settings are back.", crate::span(secs as i64))),
                Err(e) => Stage::Failed(format!("The internet didn't come back, and putting the old settings back didn't work: {e}")),
            };
        }
        std::thread::sleep(Duration::from_millis(700));
    };
    set(stage);
}

/// Whether the internet answers, by name: a lookup and a connection, in
/// a few seconds at most. The time taken, in ms.
fn online() -> Option<u64> {
    let start = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let found = ["one.one.one.one:443", "dns.google:443"].iter().find_map(|h| h.to_socket_addrs().ok().and_then(|mut a| a.find(|a| a.is_ipv4())));
        let _ = tx.send(found);
    });
    let addr = rx.recv_timeout(Duration::from_secs(3)).ok().flatten()?;
    TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    Some(start.elapsed().as_millis() as u64)
}

fn is_primary(dev: &str) -> bool {
    backend::primary_dev().as_deref() == Some(dev)
}

/// "255.255.255.0" for 24.
pub fn mask(prefix: u8) -> String {
    let bits = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32) as u32) };
    Ipv4Addr::from(bits).to_string()
}

/// 24 for "255.255.255.0"; `None` if it isn't a mask.
pub fn prefix(mask: &str) -> Option<u8> {
    let bits = u32::from(mask.trim().parse::<Ipv4Addr>().ok()?);
    let n = bits.leading_ones();
    (bits.checked_shl(n).unwrap_or(0) == 0).then_some(n as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks() {
        assert_eq!(mask(24), "255.255.255.0");
        assert_eq!(mask(0), "0.0.0.0");
        assert_eq!(prefix("255.255.255.0"), Some(24));
        assert_eq!(prefix("255.255.0.255"), None);
        assert_eq!(prefix("0.0.0.0"), Some(0));
    }
}
