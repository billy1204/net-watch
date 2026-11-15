//! The computer's network adapters, through NetworkManager's `nmcli`: what
//! each one is, its settings and the Wi-Fi networks nearby; and changing
//! them, with the old settings put back if the internet doesn't come back.
use crate::monitor::Core;
use crate::store::Time;
use chrono::Local;
use std::io::Write;
use std::net::{Ipv4Addr, TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
    /// Its name to Linux, like "enp6s0".
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
    let found = adapters();
    core.state.lock().unwrap().net.adapters = Some(found);
    core.repaint();
}

fn scan(core: &Core, dev: &str, rescan: bool) {
    if std::mem::replace(&mut core.state.lock().unwrap().net.scanning, true) {
        return;
    }
    core.repaint();
    let saved = saved_wifi();
    let mark = |found: Result<Vec<WifiNet>, String>| {
        found.map(|mut list| {
            for n in &mut list {
                n.saved = saved.iter().find(|(s, _)| *s == n.ssid).map(|(_, u)| u.clone());
            }
            list
        })
    };
    // What it saw last, at once; then a fresh look, which takes a few seconds.
    if rescan && let Ok(quick) = networks(dev, false) {
        core.state.lock().unwrap().net.wifi = Some((dev.to_string(), mark(Ok(quick)), Local::now()));
        core.repaint();
    }
    let found = mark(networks(dev, rescan));
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
        Some(uuid) => nmcli(&["-w", "30", "connection", "up", &uuid, "ifname", &dev]).map(drop),
        None => nmcli(&["device", "disconnect", &dev]).map(drop),
    };
    match what {
        Do::Apply { dev, old, new, active, primary } => {
            let apply = {
                let (dev, new) = (dev.clone(), new.clone());
                move || save_profile(&new, active.then_some(dev.as_str()))
            };
            let undo = move || save_profile(&old, active.then_some(dev.as_str()));
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
                    Some(uuid) => nmcli(&["-w", "30", "connection", "up", &uuid, "ifname", &dev]).map(drop),
                    None => {
                        let uuid = add_wifi(&dev, &ssid, &security, &password, auto, hidden)?;
                        *created.lock().unwrap() = Some(uuid.clone());
                        nmcli(&["-w", "30", "connection", "up", &uuid, "ifname", &dev]).map(drop)
                    }
                }
            };
            let undo = {
                let back = back_to(dev.clone(), prev);
                move || {
                    // A new network that didn't work isn't kept.
                    if let Some(uuid) = created.lock().unwrap().take() {
                        let _ = nmcli(&["connection", "delete", &uuid]);
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
                move || nmcli(&["device", "disconnect", &dev]).map(drop)
            };
            if primary {
                trial(core, format!("turning off {dev}"), apply, back_to(dev, prev));
            } else {
                quietly(core, "Turning it off…", "Turned off.", apply);
            }
        }
        Do::Connect(dev) => quietly(core, "Turning it on…", "Turned on.", move || nmcli(&["-w", "30", "device", "connect", &dev]).map(drop)),
        Do::Forget(uuid, name) => quietly(core, "Forgetting…", &format!("Forgot {name}."), move || nmcli(&["connection", "delete", &uuid]).map(drop)),
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
    primary_dev().as_deref() == Some(dev)
}

/// The adapter the internet goes through: the default route with the
/// lowest metric.
fn primary_dev() -> Option<String> {
    let table = std::fs::read_to_string("/proc/net/route").ok()?;
    table
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() > 7 && f[1] == "00000000" && f[7] == "00000000").then(|| (f[6].parse::<u32>().unwrap_or(u32::MAX), f[0].to_string()))
        })
        .min()
        .map(|(_, dev)| dev)
}

pub fn adapters() -> Result<Vec<Adapter>, String> {
    let list = nmcli(&["-t", "-f", "DEVICE,TYPE,STATE,CON-UUID", "device"])?;
    let primary = primary_dev();
    let mut found = Vec::new();
    for line in list.lines() {
        let f = fields(line);
        if f.len() < 4 {
            continue;
        }
        let kind = match f[1].as_str() {
            "ethernet" => Kind::Ethernet,
            "wifi" => Kind::Wifi,
            _ => continue,
        };
        let dev = f[0].clone();
        let info = keyed(&nmcli(&["-t", "-f", "GENERAL.VENDOR,GENERAL.PRODUCT,GENERAL.HWADDR,IP4.ADDRESS,IP4.GATEWAY,IP4.DNS", "device", "show", &dev]).unwrap_or_default());
        let get = |k: &str| info.iter().find(|(key, _)| key == k || key.starts_with(&format!("{k}["))).map(|(_, v)| v.clone()).filter(|v| !v.is_empty() && v != "--");
        let sys = |what: &str| std::fs::read_to_string(format!("/sys/class/net/{dev}/{what}")).ok().and_then(|s| s.trim().parse::<i64>().ok());
        let active = !f[3].is_empty();
        let profile = if active { profile(&f[3]).ok() } else if kind == Kind::Ethernet { spare_ethernet(&dev) } else { None };
        let wifi = if kind == Kind::Wifi && active { networks(&dev, false).ok().and_then(|n| n.into_iter().find(|n| n.in_use)) } else { None };
        found.push(Adapter {
            kind,
            state: f[2].clone(),
            model: model(get("GENERAL.VENDOR").unwrap_or_default(), get("GENERAL.PRODUCT").unwrap_or_default()),
            mac: get("GENERAL.HWADDR").unwrap_or_default(),
            profile,
            active,
            address: get("IP4.ADDRESS"),
            gateway: get("IP4.GATEWAY"),
            dns: info.iter().filter(|(k, _)| k.starts_with("IP4.DNS")).map(|(_, v)| v.clone()).collect(),
            speed: sys("speed").filter(|s| *s > 0).map(|s| s as u32),
            mtu: sys("mtu").map(|m| m as u32),
            primary: primary.as_deref() == Some(dev.as_str()),
            wifi,
            dev,
        });
    }
    // The one in use first, then Ethernet, then by name.
    found.sort_by_key(|a| (!a.primary, !a.connected(), a.kind != Kind::Ethernet, a.dev.clone()));
    Ok(found)
}

/// "MEDIATEK Corp." and "MT7922 802.11ax PCI Express Wireless Network
/// Adapter" make "MediaTek MT7922".
fn model(vendor: String, product: String) -> String {
    let vendor = vendor.split([' ', ',']).next().unwrap_or("").to_string();
    let vendor = match vendor.to_lowercase().as_str() {
        "mediatek" => "MediaTek".into(),
        "intel" => "Intel".into(),
        "realtek" => "Realtek".into(),
        "qualcomm" => "Qualcomm".into(),
        "broadcom" => "Broadcom".into(),
        _ => vendor,
    };
    let product = product.split_whitespace().next().unwrap_or("");
    let product = product.trim_start_matches(&vendor).trim();
    format!("{vendor} {product}").trim().to_string()
}

/// For an Ethernet adapter with nothing plugged in: the wired settings it
/// would use.
fn spare_ethernet(dev: &str) -> Option<Profile> {
    let list = nmcli(&["-t", "-f", "UUID,TYPE", "connection", "show"]).ok()?;
    list.lines().map(fields).filter(|f| f.len() >= 2 && f[1] == "802-3-ethernet").find_map(|f| {
        let bound = nmcli(&["-g", "connection.interface-name", "connection", "show", &f[0]]).ok()?;
        let bound = bound.trim();
        if bound.is_empty() || bound == dev { profile(&f[0]).ok() } else { None }
    })
}

pub fn profile(uuid: &str) -> Result<Profile, String> {
    let text = nmcli(&[
        "-t",
        "-f",
        "connection.id,connection.uuid,connection.autoconnect,ipv4.method,ipv4.addresses,ipv4.gateway,ipv4.dns,ipv4.ignore-auto-dns,ipv4.route-metric",
        "connection",
        "show",
        uuid,
    ])?;
    let kv = keyed(&text);
    let get = |k: &str| kv.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()).filter(|v| v != "--").unwrap_or_default();
    let list = |k: &str| get(k).split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect::<Vec<_>>();
    Ok(Profile {
        uuid: get("connection.uuid"),
        name: get("connection.id"),
        manual: get("ipv4.method") == "manual",
        address: list("ipv4.addresses").into_iter().next().unwrap_or_default(),
        gateway: get("ipv4.gateway"),
        dns: list("ipv4.dns"),
        ignore_auto_dns: get("ipv4.ignore-auto-dns") == "yes",
        autoconnect: get("connection.autoconnect") != "no",
        metric: get("ipv4.route-metric").parse().unwrap_or(-1),
    })
}

/// Save `p`, and when `dev` is given (it's in use there) bring it in now.
fn save_profile(p: &Profile, dev: Option<&str>) -> Result<(), String> {
    let yes = |b: bool| if b { "yes" } else { "no" };
    let dns = p.dns.join(",");
    let metric = p.metric.to_string();
    let (address, gateway) = if p.manual { (p.address.as_str(), p.gateway.as_str()) } else { ("", "") };
    nmcli(&[
        "connection",
        "modify",
        &p.uuid,
        "ipv4.method",
        if p.manual { "manual" } else { "auto" },
        "ipv4.addresses",
        address,
        "ipv4.gateway",
        gateway,
        "ipv4.dns",
        &dns,
        "ipv4.ignore-auto-dns",
        yes(p.ignore_auto_dns),
        "connection.autoconnect",
        yes(p.autoconnect),
        "ipv4.route-metric",
        &metric,
    ])?;
    if let Some(dev) = dev {
        // Gently if it can; some changes need the connection restarted.
        if nmcli(&["device", "reapply", dev]).is_err() {
            nmcli(&["-w", "30", "connection", "up", &p.uuid, "ifname", dev])?;
        }
    }
    Ok(())
}

/// Save a new Wi-Fi network; the password goes in through the editor's
/// input, so it never shows in the list of running programs. Its id.
fn add_wifi(dev: &str, ssid: &str, security: &str, password: &str, auto: bool, hidden: bool) -> Result<String, String> {
    let key_mgmt = if security.contains("802.1X") {
        return Err("this network needs a user name and certificates; use the desktop's own Wi-Fi settings".into());
    } else if security.contains("WEP") {
        return Err("this network uses old WEP security, which isn't supported".into());
    } else if security.is_empty() && password.is_empty() {
        None
    } else if security.contains("WPA3") && !security.contains("WPA2") {
        Some("sae")
    } else {
        Some("wpa-psk")
    };
    let mut args = vec!["connection", "add", "type", "wifi", "ifname", dev, "con-name", ssid, "ssid", ssid];
    args.extend(["connection.autoconnect", if auto { "yes" } else { "no" }]);
    if hidden {
        args.extend(["802-11-wireless.hidden", "yes"]);
    }
    if let Some(k) = key_mgmt {
        args.extend(["802-11-wireless-security.key-mgmt", k]);
    }
    let out = nmcli(&args)?;
    // "Connection 'Name' (uuid) successfully added."
    let uuid = out.rsplit_once('(').and_then(|(_, r)| r.split_once(')')).map(|(u, _)| u.to_string()).ok_or("couldn't save the network")?;
    if key_mgmt.is_some() {
        if password.contains('\n') {
            let _ = nmcli(&["connection", "delete", &uuid]);
            return Err("the password can't have a line break in it".into());
        }
        let script = format!("set 802-11-wireless-security.psk {password}\nsave persistent\nquit\n");
        if let Err(e) = nmcli_input(&["connection", "edit", &uuid], &script) {
            let _ = nmcli(&["connection", "delete", &uuid]);
            return Err(e);
        }
    }
    Ok(uuid)
}

/// The Wi-Fi networks `dev` can see, strongest first; `rescan` looks again
/// (a few seconds) rather than using what it saw last. Not marked saved.
pub fn networks(dev: &str, rescan: bool) -> Result<Vec<WifiNet>, String> {
    let text = nmcli(&["-t", "-f", "IN-USE,SSID,SECURITY,FREQ,SIGNAL", "device", "wifi", "list", "ifname", dev, "--rescan", if rescan { "yes" } else { "no" }])?;
    let mut found: Vec<WifiNet> = Vec::new();
    for f in text.lines().map(fields) {
        if f.len() < 5 || f[1].is_empty() {
            continue;
        }
        let net = WifiNet {
            ssid: f[1].clone(),
            security: if f[2] == "--" { String::new() } else { f[2].clone() },
            freq: f[3].split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0),
            signal: f[4].parse().unwrap_or(0),
            in_use: f[0] == "*",
            saved: None,
        };
        // One line per name: the strongest, or the one in use.
        match found.iter_mut().find(|n| n.ssid == net.ssid) {
            Some(n) if net.in_use || (!n.in_use && net.signal > n.signal) => *n = net,
            Some(_) => {}
            None => found.push(net),
        }
    }
    found.sort_by_key(|n| (!n.in_use, std::cmp::Reverse(n.signal)));
    Ok(found)
}

/// Saved Wi-Fi networks: name and settings id.
fn saved_wifi() -> Vec<(String, String)> {
    let Ok(list) = nmcli(&["-t", "-f", "UUID,TYPE", "connection", "show"]) else { return Vec::new() };
    list.lines()
        .map(fields)
        .filter(|f| f.len() >= 2 && f[1] == "802-11-wireless")
        .filter_map(|f| {
            let ssid = nmcli(&["-g", "802-11-wireless.ssid", "connection", "show", &f[0]]).ok()?;
            Some((unescape(ssid.trim()), f[0].clone()))
        })
        .collect()
}

fn nmcli(args: &[&str]) -> Result<String, String> {
    nmcli_input(args, "")
}

fn nmcli_input(args: &[&str], input: &str) -> Result<String, String> {
    let mut child = Command::new("nmcli")
        .args(args)
        .env("LC_ALL", "C")
        .stdin(if input.is_empty() { Stdio::null() } else { Stdio::piped() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "needs NetworkManager (nmcli), which isn't here".to_string() } else { e.to_string() })?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    // The editor says what went wrong on its output, and still exits 0.
    let editor_error = !input.is_empty() && stdout.lines().any(|l| l.contains("Error:"));
    if out.status.success() && !editor_error {
        return Ok(stdout);
    }
    let text = String::from_utf8_lossy(&out.stderr).to_string() + &stdout;
    let line = text.lines().find(|l| l.contains("Error:")).or(text.lines().find(|l| !l.trim().is_empty())).unwrap_or("nmcli failed");
    let line = line.split("Error:").last().unwrap_or(line).trim().trim_end_matches('.');
    Err(line.to_string())
}

/// A line of `nmcli -t`: fields split on ':', with "\:" and "\\" kept.
fn fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.last_mut().unwrap().extend(chars.next()),
            ':' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    fields(s).join(":")
}

/// "KEY:value" lines, as `nmcli -t ... show` gives them.
fn keyed(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
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
    fn terse_fields() {
        assert_eq!(fields(r"*:Home\:Net:WPA2:5240 MHz:79"), ["*", "Home:Net", "WPA2", "5240 MHz", "79"]);
        assert_eq!(fields("a::b"), ["a", "", "b"]);
    }

    #[test]
    fn masks() {
        assert_eq!(mask(24), "255.255.255.0");
        assert_eq!(mask(0), "0.0.0.0");
        assert_eq!(prefix("255.255.255.0"), Some(24));
        assert_eq!(prefix("255.255.0.255"), None);
        assert_eq!(prefix("0.0.0.0"), Some(0));
    }

    #[test]
    fn models() {
        assert_eq!(model("MEDIATEK Corp.".into(), "MT7922 802.11ax PCI Express Wireless Network Adapter".into()), "MediaTek MT7922");
        assert_eq!(model("Realtek Semiconductor Co., Ltd.".into(), "RTL8111/8168/8211/8411 PCI Express Gigabit Ethernet Controller".into()), "Realtek RTL8111/8168/8211/8411");
    }
}
