//! Where results live: plain CSV files in the app's data folder, so they can
//! be opened in a spreadsheet and sent as proof. Settings live beside them.
use chrono::{DateTime, Local};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

pub type Time = DateTime<Local>;

/// One round of checks, as kept.
#[derive(Clone, Copy, Debug)]
pub struct CheckRow {
    pub t: Time,
    pub latency_ms: Option<f32>,
    pub ok: u8,
    pub total: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cause {
    /// The router answered but nothing past it did.
    Provider,
    /// The router didn't answer, or there was no network at all.
    Local,
    /// Can't tell (the router isn't checked on this system).
    Unknown,
}

impl Cause {
    pub fn word(self) -> &'static str {
        match self {
            Cause::Provider => "provider",
            Cause::Local => "local",
            Cause::Unknown => "unknown",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "provider" => Cause::Provider,
            "local" => Cause::Local,
            _ => Cause::Unknown,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Outage {
    pub start: Time,
    /// `None` while it's still going on.
    pub end: Option<Time>,
    pub cause: Cause,
    pub detail: String,
}

impl Outage {
    pub fn secs(&self, now: Time) -> i64 {
        (self.end.unwrap_or(now) - self.start).num_seconds().max(0)
    }
}

#[derive(Clone, Debug)]
pub struct SpeedRow {
    pub t: Time,
    pub down: f64,
    pub up: f64,
    pub ping: f64,
    pub jitter: f64,
    pub mb: f64,
    pub error: Option<String>,
    /// Which server measured it, and its page on the service's site.
    pub server: Option<String>,
    pub link: Option<String>,
}

#[cfg(not(windows))]
fn base(var: &str, home_rel: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var).map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(home_rel)))?;
    Some(base.join("net-watch"))
}

/// Results: ~/.local/share/net-watch.
#[cfg(not(windows))]
pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("NET_WATCH_DATA").map(PathBuf::from).or_else(|| base("XDG_DATA_HOME", ".local/share"))
}

#[cfg(not(windows))]
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("NET_WATCH_DATA").map(PathBuf::from).or_else(|| base("XDG_CONFIG_HOME", ".config"))
}

/// Results: %LOCALAPPDATA%\net-watch.
#[cfg(windows)]
pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("NET_WATCH_DATA").map(PathBuf::from).or_else(|| Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("net-watch")))
}

/// Settings: %APPDATA%\net-watch.
#[cfg(windows)]
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("NET_WATCH_DATA").map(PathBuf::from).or_else(|| Some(PathBuf::from(std::env::var_os("APPDATA")?).join("net-watch")))
}

fn data_file(name: &str) -> Option<PathBuf> {
    Some(data_dir()?.join(name))
}

fn month_file(t: Time) -> Option<PathBuf> {
    data_file(&t.format("checks-%Y-%m.csv").to_string())
}

pub fn fmt_time(t: Time) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

fn parse_time(s: &str) -> Option<Time> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Local))
}

/// Append one line, writing `header` first if the file is new.
fn append(path: Option<PathBuf>, header: &str, line: &str) {
    let Some(path) = path else { return };
    let _ = fs::create_dir_all(path.parent().unwrap());
    let new = !path.exists();
    // A file from before a column was added gets the new header line.
    let mut first = String::new();
    if !new
        && File::open(&path).and_then(|f| BufReader::new(f).read_line(&mut first)).is_ok()
        && first.trim_end() != header
        && header.starts_with(first.trim_end())
        && let Ok(text) = fs::read_to_string(&path)
    {
        let _ = fs::write(&path, format!("{header}\n{}", &text[first.len()..]));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = if new { writeln!(f, "{header}\n{line}") } else { writeln!(f, "{line}") };
    }
}

/// CSV fields are ours except error text, which loses its commas.
fn clean(s: &str) -> String {
    s.replace(", ", " ").replace([',', '\n', '\r', '"'], " ")
}

fn rows(path: Option<PathBuf>) -> impl Iterator<Item = Vec<String>> {
    path.and_then(|p| File::open(p).ok())
        .into_iter()
        .flat_map(|f| BufReader::new(f).lines().skip(1).map_while(Result::ok))
        .map(|l| l.split(',').map(str::to_string).collect())
}

const CHECK_HEADER: &str = "time,latency_ms,ok,total";

pub fn save_check(c: &CheckRow) {
    let lat = c.latency_ms.map(|l| format!("{l:.1}")).unwrap_or_default();
    append(month_file(c.t), CHECK_HEADER, &format!("{},{lat},{},{}", fmt_time(c.t), c.ok, c.total));
}

/// Checks since `from`, oldest first.
pub fn load_checks(from: Time) -> Vec<CheckRow> {
    use chrono::Datelike;
    let mut out = Vec::new();
    let now = Local::now();
    let (mut y, mut m) = (from.year(), from.month());
    while (y, m) <= (now.year(), now.month()) {
        let name = format!("checks-{y:04}-{m:02}.csv");
        (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        for r in rows(data_file(&name)) {
            let (Some(t), Some(ok), Some(total)) = (r.first().and_then(|s| parse_time(s)), r.get(2).and_then(|s| s.parse().ok()), r.get(3).and_then(|s| s.parse().ok())) else { continue };
            if t >= from {
                out.push(CheckRow { t, latency_ms: r.get(1).and_then(|s| s.parse().ok()), ok, total });
            }
        }
    }
    out
}

const OUTAGE_HEADER: &str = "start,end,seconds,cause,detail";

pub fn save_outage(o: &Outage) {
    let Some(end) = o.end else { return };
    append(
        data_file("outages.csv"),
        OUTAGE_HEADER,
        &format!("{},{},{},{},{}", fmt_time(o.start), fmt_time(end), o.secs(end), o.cause.word(), clean(&o.detail)),
    );
}

pub fn load_outages() -> Vec<Outage> {
    rows(data_file("outages.csv"))
        .filter_map(|r| {
            Some(Outage {
                start: parse_time(r.first()?)?,
                end: Some(parse_time(r.get(1)?)?),
                cause: Cause::parse(r.get(3)?),
                detail: r.get(4).cloned().unwrap_or_default(),
            })
        })
        .collect()
}

/// An outage still going on, kept in its own file so it survives the app
/// closing or the computer shutting down in the middle of it.
pub fn save_open_outage(o: Option<&Outage>) {
    let Some(path) = data_file("outage-now.csv") else { return };
    match o {
        Some(o) => {
            let _ = fs::create_dir_all(path.parent().unwrap());
            let _ = fs::write(path, format!("{},{},{}\n", fmt_time(o.start), o.cause.word(), clean(&o.detail)));
        }
        None => {
            let _ = fs::remove_file(path);
        }
    }
}

pub fn load_open_outage() -> Option<Outage> {
    let s = fs::read_to_string(data_file("outage-now.csv")?).ok()?;
    let f: Vec<&str> = s.trim().splitn(3, ',').collect();
    Some(Outage { start: parse_time(f.first()?)?, end: None, cause: Cause::parse(f.get(1)?), detail: f.get(2).unwrap_or(&"").to_string() })
}

const SPEED_HEADER: &str = "time,download_mbps,upload_mbps,ping_ms,jitter_ms,mb_used,error,server,result_link";

pub fn save_speed(s: &SpeedRow) {
    append(
        data_file("speed.csv"),
        SPEED_HEADER,
        &format!(
            "{},{:.1},{:.1},{:.1},{:.1},{:.1},{},{},{}",
            fmt_time(s.t),
            s.down,
            s.up,
            s.ping,
            s.jitter,
            s.mb,
            s.error.as_deref().map(clean).unwrap_or_default(),
            s.server.as_deref().map(clean).unwrap_or_default(),
            s.link.as_deref().map(clean).unwrap_or_default()
        ),
    );
}

pub fn load_speeds() -> Vec<SpeedRow> {
    rows(data_file("speed.csv"))
        .filter_map(|r| {
            let num = |i: usize| r.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            Some(SpeedRow {
                t: parse_time(r.first()?)?,
                down: num(1),
                up: num(2),
                ping: num(3),
                jitter: num(4),
                mb: num(5),
                error: r.get(6).filter(|s| !s.is_empty()).cloned(),
                server: r.get(7).filter(|s| !s.is_empty()).cloned(),
                link: r.get(8).filter(|s| !s.is_empty()).cloned(),
            })
        })
        .collect()
}

/// What the user can change in Settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub check_secs: u64,
    /// Hours between speed tests; 0 turns them off.
    pub speed_hours: u32,
    /// The user accepted Ookla's license, terms and privacy policy.
    pub ookla_accepted: bool,
    /// What the plan promises, in Mbps; 0 when not set.
    pub plan_down: f64,
    pub plan_up: f64,
    pub popups: bool,
    /// Scan the home network now and then, with a pop-up when a new device
    /// joins; every `device_mins` minutes.
    pub watch_devices: bool,
    pub device_mins: u64,
    /// The home network's router (hardware address): watching only scans
    /// there. Set by the first scan after watching is turned on.
    pub watch_router: String,
    pub theme: String,
    pub animate: bool,
    /// After a network change: seconds to wait for the internet before
    /// putting the old settings back.
    pub undo_secs: u64,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            check_secs: 30,
            speed_hours: 4,
            ookla_accepted: false,
            plan_down: 0.0,
            plan_up: 0.0,
            popups: true,
            watch_devices: false,
            device_mins: 15,
            watch_router: String::new(),
            theme: "Clean light".into(),
            animate: true,
            undo_secs: 30,
        }
    }
}

impl Prefs {
    pub fn load() -> Self {
        let mut p = Self::default();
        let Some(text) = config_dir().and_then(|d| fs::read_to_string(d.join("settings")).ok()) else { return p };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "check_secs" => p.check_secs = v.parse().unwrap_or(p.check_secs).clamp(10, 600),
                "speed_hours" => p.speed_hours = v.parse().unwrap_or(p.speed_hours).min(24),
                "ookla_accepted" => p.ookla_accepted = v == "on",
                "plan_down" => p.plan_down = v.parse().unwrap_or(0.0),
                "plan_up" => p.plan_up = v.parse().unwrap_or(0.0),
                "popups" => p.popups = v != "off",
                "watch_devices" => p.watch_devices = v == "on",
                "device_mins" => p.device_mins = v.parse().unwrap_or(p.device_mins).clamp(5, 240),
                "watch_router" => p.watch_router = v.into(),
                "theme" => p.theme = v.into(),
                "animate" => p.animate = v != "off",
                "undo_secs" => p.undo_secs = v.parse().unwrap_or(p.undo_secs).clamp(10, 300),
                _ => {}
            }
        }
        p
    }

    /// Write atomically, so a crash mid-save keeps the old settings.
    pub fn save(&self) {
        let Some(dir) = config_dir() else { return };
        let _ = fs::create_dir_all(&dir);
        let onoff = |b: bool| if b { "on" } else { "off" };
        let text = format!(
            "check_secs={}\nspeed_hours={}\nookla_accepted={}\nplan_down={}\nplan_up={}\npopups={}\nwatch_devices={}\ndevice_mins={}\nwatch_router={}\ntheme={}\nanimate={}\nundo_secs={}\n",
            self.check_secs,
            self.speed_hours,
            onoff(self.ookla_accepted),
            self.plan_down,
            self.plan_up,
            onoff(self.popups),
            onoff(self.watch_devices),
            self.device_mins,
            self.watch_router,
            self.theme,
            onoff(self.animate),
            self.undo_secs
        );
        let tmp = dir.join("settings.tmp");
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(tmp, dir.join("settings"));
        }
    }
}

/// Only one copy runs at a time: holds a lock on a file in the data folder
/// for as long as the process lives. False when another copy holds it.
pub fn single_instance() -> bool {
    let Some(path) = data_file("running.lock") else { return true };
    let _ = fs::create_dir_all(path.parent().unwrap());
    let Ok(file) = OpenOptions::new().create(true).truncate(false).write(true).open(path) else { return true };
    match file.try_lock() {
        Ok(()) => {
            std::mem::forget(file);
            true
        }
        Err(fs::TryLockError::WouldBlock) => false,
        Err(_) => true,
    }
}
