//! Everything that outlives the window: the checker, the speed test
//! schedule, and the state the window and tray read.
use crate::probe::{self, Check};
use crate::store::{self, Cause, CheckRow, Outage, Prefs, SpeedRow, Time};
use chrono::{Duration as Span, Local};
use eframe::egui;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long history is kept in memory for the graphs.
pub const KEEP_DAYS: i64 = 31;
/// Checks while something looks wrong, so drops are timed closely.
const QUICK_SECS: u64 = 5;
/// Quick checks at most this many times in a row while only some checks
/// fail.
const QUICK_ROUNDS: u32 = 6;
/// Failed checks in a row before it counts as an outage.
const FAILS_FOR_OUTAGE: u32 = 2;
/// After the computer wakes up, Wi-Fi takes a moment to reconnect.
const WAKE_GRACE_SECS: i64 = 60;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status {
    Starting,
    Up,
    /// Up, but some checks failed or it's very slow to answer.
    Shaky,
    Down,
    Paused,
}

/// Asks the main thread, which waits while the window is closed, to act.
pub enum Wake {
    Open,
    Quit,
}

#[derive(Default)]
pub struct Shared {
    pub prefs: Prefs,
    /// The last `KEEP_DAYS` of checks, oldest first.
    pub checks: Vec<CheckRow>,
    /// Finished outages, oldest first.
    pub outages: Vec<Outage>,
    /// The outage going on now.
    pub open: Option<Outage>,
    pub speeds: Vec<SpeedRow>,
    pub last: Option<(Time, Check)>,
    pub up_since: Option<Time>,
    pub next_speed: Option<Time>,
    pub testing: bool,
    /// While testing: how it's going, and when it should end.
    pub live: Option<probe::Live>,
    pub test_ends: Option<Time>,
    /// Asks the window to show Settings (to set up speed tests).
    pub open_settings: bool,
    /// The last firewall check, and whether one is running.
    pub firewall: Option<crate::firewall::Report>,
    pub checking_firewall: bool,
    /// The last "What's wrong?" check, filled in step by step while it runs.
    pub diag: Option<crate::diag::Report>,
    pub diagnosing: bool,
    /// The last "Where's the lag?" trace, filled in as it runs.
    pub route: Option<crate::route::Trace>,
    pub tracing: bool,
    /// The last device scan, and whether one is running.
    pub devices: Option<Result<crate::devices::Scan, String>>,
    pub scanning: bool,
    /// Device watching is on, but this isn't the home network.
    pub away_from_home: bool,
    pub paused_until: Option<Time>,
    /// The network adapters, and a change being tried.
    pub net: crate::netif::Net,
    /// The open window's context; `None` while the window is closed.
    pub ctx: Option<egui::Context>,
    pub quit: bool,
}

impl Shared {
    pub fn status(&self) -> Status {
        if self.paused_until.is_some() {
            return Status::Paused;
        }
        if self.open.is_some() {
            return Status::Down;
        }
        match &self.last {
            None => Status::Starting,
            Some((_, c)) if !c.up() => Status::Shaky,
            Some((_, c)) if c.ok < c.total || c.latency.is_some_and(|l| l > Duration::from_millis(300)) => Status::Shaky,
            Some(_) => Status::Up,
        }
    }

    /// Outages that overlap `from..now`, including the one going on now.
    pub fn outages_since(&self, from: Time) -> impl Iterator<Item = &Outage> {
        self.outages.iter().chain(&self.open).filter(move |o| o.end.is_none_or(|e| e > from))
    }
}

enum Ask {
    Recheck,
    SpeedTest,
    Reschedule,
}

#[derive(Clone)]
pub struct Core {
    pub state: Arc<Mutex<Shared>>,
    wake: Sender<Wake>,
    checker: Sender<Ask>,
    speed: Sender<Ask>,
}

/// A number in 0..1 that differs each call, for spreading out test times.
fn random() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

/// When the next speed test is due after one at `last`: every
/// `hours`, give or take 10%, so tests don't all land on the hour.
fn next_test(last: Option<Time>, hours: u32) -> Option<Time> {
    if hours == 0 {
        return None;
    }
    let now = Local::now();
    let gap = Span::seconds((hours as f64 * 3600.0 * (0.9 + 0.2 * random())) as i64);
    match last.map(|t| t + gap) {
        Some(t) if t > now => Some(t),
        // Overdue or never run: soon after starting, not straight away.
        _ if std::env::var_os("NET_WATCH_TEST_SOON").is_some() => Some(now + Span::seconds(3)),
        _ => Some(now + Span::seconds(60 + (random() * 240.0) as i64)),
    }
}

impl Core {
    /// Load the history and start the checker and speed test threads.
    pub fn start() -> (Self, Receiver<Wake>) {
        let now = Local::now();
        let prefs = Prefs::load();
        let checks = store::load_checks(now - Span::days(KEEP_DAYS));
        let mut outages = store::load_outages();
        // Closed while an outage was going on: it ended no later than the
        // last check we saw (if it's still down, a new one starts).
        if let Some(mut o) = store::load_open_outage() {
            let end = checks.iter().rev().find(|c| c.t >= o.start).map_or(o.start, |c| c.t);
            o.end = Some(end);
            store::save_outage(&o);
            store::save_open_outage(None);
            outages.push(o);
        }
        let speeds = store::load_speeds();
        let next_speed = next_test(speeds.last().map(|s| s.t), prefs.speed_hours);
        let shared = Shared { prefs, checks, outages, speeds, next_speed, ..Default::default() };

        let (wake, wake_rx) = channel();
        let (checker, checker_rx) = channel();
        let (speed, speed_rx) = channel();
        let core = Self { state: Arc::new(Mutex::new(shared)), wake, checker, speed };
        let c = core.clone();
        std::thread::spawn(move || c.watch(checker_rx));
        let c = core.clone();
        std::thread::spawn(move || c.speed_tests(speed_rx));
        let c = core.clone();
        std::thread::spawn(move || c.watch_devices());
        (core, wake_rx)
    }

    pub fn repaint(&self) {
        if let Some(ctx) = &self.state.lock().unwrap().ctx {
            ctx.request_repaint();
        }
    }

    fn changed(&self) {
        crate::refresh_tray();
        self.repaint();
    }

    pub fn window_open(&self) -> bool {
        self.state.lock().unwrap().ctx.is_some()
    }

    pub fn show_window(&self) {
        match self.state.lock().unwrap().ctx.clone() {
            Some(ctx) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            None => {
                let _ = self.wake.send(Wake::Open);
            }
        }
    }

    pub fn toggle_window(&self) {
        match self.state.lock().unwrap().ctx.clone() {
            Some(ctx) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            None => {
                let _ = self.wake.send(Wake::Open);
            }
        }
    }

    pub fn quit(&self) {
        let ctx = {
            let mut st = self.state.lock().unwrap();
            st.quit = true;
            st.ctx.clone()
        };
        match ctx {
            Some(ctx) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            None => {
                let _ = self.wake.send(Wake::Quit);
            }
        }
    }

    pub fn test_speed_now(&self) {
        let _ = self.speed.send(Ask::SpeedTest);
    }

    /// From the tray: run a speed test now, or, before Speedtest is set
    /// up, open Settings where that's done. Not for callers holding the
    /// state lock.
    pub fn test_or_set_up(&self) {
        let mut st = self.state.lock().unwrap();
        if crate::ookla::ready(&st.prefs) {
            drop(st);
            let _ = self.speed.send(Ask::SpeedTest);
        } else {
            st.open_settings = true;
            drop(st);
            self.show_window();
            self.repaint();
        }
    }

    /// Run the firewall check in the background. Not for callers holding
    /// the state lock.
    pub fn check_firewall(&self) {
        let mut st = self.state.lock().unwrap();
        if std::mem::replace(&mut st.checking_firewall, true) {
            return;
        }
        drop(st);
        let c = self.clone();
        std::thread::spawn(move || {
            let report = crate::firewall::check();
            let mut st = c.state.lock().unwrap();
            st.firewall = Some(report);
            st.checking_firewall = false;
            drop(st);
            c.repaint();
        });
        self.repaint();
    }

    /// Run the "What's wrong?" check in the background, showing each step
    /// as it's done. Not for callers holding the state lock.
    pub fn diagnose(&self) {
        let mut st = self.state.lock().unwrap();
        if std::mem::replace(&mut st.diagnosing, true) {
            return;
        }
        st.diag = Some(crate::diag::Report { at: Local::now(), steps: Vec::new() });
        drop(st);
        let c = self.clone();
        std::thread::spawn(move || {
            crate::diag::run(&|step| {
                if let Some(r) = c.state.lock().unwrap().diag.as_mut() {
                    r.steps.push(step);
                }
                c.repaint();
            });
            c.state.lock().unwrap().diagnosing = false;
            c.repaint();
        });
        self.repaint();
    }

    /// Trace the route to `to` in the background. Not for callers holding
    /// the state lock.
    pub fn trace(&self, to: String) {
        let mut st = self.state.lock().unwrap();
        if std::mem::replace(&mut st.tracing, true) {
            return;
        }
        st.route = None;
        drop(st);
        let c = self.clone();
        std::thread::spawn(move || {
            crate::route::run(&to, &|t| {
                c.state.lock().unwrap().route = Some(t.clone());
                c.repaint();
            });
            c.state.lock().unwrap().tracing = false;
            c.repaint();
        });
        self.repaint();
    }

    /// Scan the home network for devices in the background. Not for
    /// callers holding the state lock.
    pub fn scan_devices(&self) {
        let c = self.clone();
        std::thread::spawn(move || c.run_scan());
    }

    /// Scan now, on this thread; skipped if a scan is already running.
    fn run_scan(&self) {
        let mut st = self.state.lock().unwrap();
        if std::mem::replace(&mut st.scanning, true) {
            return;
        }
        drop(st);
        self.repaint();
        let scan = crate::devices::scan();
        let joined: Vec<String> = match &scan {
            Ok(s) => s.devices.iter().filter(|d| d.joined).map(|d| d.label()).collect(),
            Err(_) => Vec::new(),
        };
        let mut st = self.state.lock().unwrap();
        st.devices = Some(scan);
        st.scanning = false;
        let watching = st.prefs.watch_devices;
        drop(st);
        self.repaint();
        if watching && !joined.is_empty() {
            let title = if joined.len() == 1 { "New device on your network".to_string() } else { format!("{} new devices on your network", joined.len()) };
            // Names come from the devices themselves; some pop-up servers
            // read markup.
            let body = joined.join("\n").replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            crate::notify(&title, &body);
        }
    }

    /// With device watching on, scan every `device_mins` while online.
    fn watch_devices(self) {
        let mut last: Option<Instant> = None;
        loop {
            std::thread::sleep(Duration::from_secs(20));
            let st = self.state.lock().unwrap();
            if st.quit {
                return;
            }
            let online = st.open.is_none() && st.paused_until.is_none() && st.last.as_ref().is_some_and(|(_, c)| c.up());
            let every = Duration::from_secs(st.prefs.device_mins * 60);
            let due = st.prefs.watch_devices && online && !st.testing && last.is_none_or(|l| l.elapsed() >= every);
            drop(st);
            if !due {
                continue;
            }
            last = Some(Instant::now());
            // Only on the home network: the one watching was turned on at.
            let Some(here) = crate::devices::current_network() else { continue };
            let mut st = self.state.lock().unwrap();
            if st.prefs.watch_router.is_empty() {
                st.prefs.watch_router = here.clone();
                st.prefs.save();
            }
            let home = st.prefs.watch_router == here;
            st.away_from_home = !home;
            drop(st);
            self.repaint();
            if home {
                self.run_scan();
            }
        }
    }

    /// Check the connection now, without waiting for the next round.
    pub fn check_now(&self) {
        let _ = self.checker.send(Ask::Recheck);
    }

    /// Stop checking until `until`, or resume with `None`. Called from the
    /// tray's menu, which redraws the tray itself after a click; asking it
    /// to from in here would wait on the tray for ever.
    pub fn pause(&self, until: Option<Time>) {
        self.state.lock().unwrap().paused_until = until;
        let _ = self.checker.send(Ask::Recheck);
        self.repaint();
    }

    pub fn set_prefs(&self, mut prefs: Prefs) {
        let (reschedule, recheck) = {
            let mut st = self.state.lock().unwrap();
            let changed = (st.prefs.speed_hours != prefs.speed_hours, st.prefs.check_secs != prefs.check_secs);
            // The window doesn't know the home network: keep it, and forget
            // it when watching is turned on again, so the next scan sets it.
            prefs.watch_router = if prefs.watch_devices && !st.prefs.watch_devices { String::new() } else { st.prefs.watch_router.clone() };
            st.prefs = prefs.clone();
            changed
        };
        prefs.save();
        if reschedule {
            let _ = self.speed.send(Ask::Reschedule);
        }
        // Start the new interval now rather than after the old one.
        if recheck {
            let _ = self.checker.send(Ask::Recheck);
        }
    }

    /// Check, then wait: the usual interval when all is well, a few seconds
    /// when anything failed so a drop's start and end are timed closely.
    fn watch(self, asks: Receiver<Ask>) {
        let mut fails = 0u32;
        let mut first_fail: Option<Time> = None;
        let mut last_check: Option<Time> = None;
        let mut grace_until: Option<Time> = None;
        // Only trust "the router didn't answer" from a router that has
        // answered before: some routers ignore every knock.
        let mut router_answers = false;
        // Checks in a row where some, but maybe not all, failed.
        let mut shaky = 0u32;
        loop {
            let paused = {
                let mut st = self.state.lock().unwrap();
                if st.paused_until.is_some_and(|t| t <= Local::now()) {
                    st.paused_until = None;
                }
                st.paused_until.is_some()
            };
            if paused {
                fails = 0;
                first_fail = None;
                last_check = None;
                match asks.recv_timeout(Duration::from_secs(5)) {
                    Err(RecvTimeoutError::Disconnected) => return,
                    _ => continue,
                }
            }

            let (interval, popups) = {
                let st = self.state.lock().unwrap();
                (st.prefs.check_secs, st.prefs.popups)
            };
            let t = Local::now();
            // A long gap means the computer was asleep, not that the
            // internet was down; give Wi-Fi a moment to come back.
            if let Some(prev) = last_check
                && (t - prev).num_seconds() > interval as i64 * 3 + 30
            {
                grace_until = Some(t + Span::seconds(WAKE_GRACE_SECS));
                fails = 0;
                first_fail = None;
                let mut st = self.state.lock().unwrap();
                if let Some(mut o) = st.open.take() {
                    o.end = Some(prev);
                    store::save_outage(&o);
                    store::save_open_outage(None);
                    st.outages.push(o);
                }
            }

            // Dev aid: a `simulate-down` file in the data folder fakes an
            // outage on the provider's side, to try the outage handling.
            let c = if store::data_dir().is_some_and(|d| d.join("simulate-down").exists()) {
                Check { total: 7, router: Some(true), failed: vec!["simulated".into()], ..Default::default() }
            } else {
                probe::check()
            };
            let t = Local::now();
            last_check = Some(t);
            router_answers |= c.router == Some(true);
            let row = CheckRow { t, latency_ms: c.latency.map(|l| l.as_secs_f32() * 1000.0), ok: c.ok, total: c.total };
            store::save_check(&row);

            let mut note: Option<(String, String)> = None;
            {
                let mut st = self.state.lock().unwrap();
                st.checks.push(row);
                if st.checks.len() % 500 == 0 {
                    let cutoff = t - Span::days(KEEP_DAYS);
                    st.checks.retain(|r| r.t >= cutoff);
                }
                if c.up() {
                    fails = 0;
                    first_fail = None;
                    if let Some(mut o) = st.open.take() {
                        o.end = Some(t);
                        store::save_outage(&o);
                        store::save_open_outage(None);
                        let week = st.outages_since(t - Span::days(7)).count() + 1;
                        note = Some(("Internet is back".into(), format!("It was down for {} · {} this week", crate::span(o.secs(t)), outage_count(week))));
                        st.outages.push(o);
                    }
                    st.up_since.get_or_insert(t);
                } else {
                    fails += 1;
                    let start = *first_fail.get_or_insert(t);
                    let waking = grace_until.is_some_and(|g| t < g);
                    if fails >= FAILS_FOR_OUTAGE && st.open.is_none() && !waking {
                        let (cause, detail) = if c.no_route {
                            (Cause::Local, "No network connection (Wi-Fi off or cable unplugged)".to_string())
                        } else if c.router == Some(true) {
                            (Cause::Provider, "Your router answered; the outside servers didn't".to_string())
                        } else if c.router == Some(false) && router_answers {
                            (Cause::Local, "Couldn't reach your router".to_string())
                        } else {
                            (Cause::Unknown, "None of the outside checks worked".to_string())
                        };
                        let o = Outage { start, end: None, cause, detail: detail.clone() };
                        store::save_open_outage(Some(&o));
                        st.open = Some(o);
                        st.up_since = None;
                        note = Some(("Internet is down".into(), format!("{} · {}", start.format("%-I:%M %P"), detail)));
                    }
                }
                st.last = Some((t, c.clone()));
            }
            self.changed();
            if let Some((title, body)) = note
                && popups
            {
                crate::notify(&title, &body);
            }

            // Quick checks while down, and for a few rounds when only some
            // checks fail; one that always fails (a network that blocks
            // one of the servers) mustn't mean checking every 5 s for ever.
            shaky = if c.ok < c.total { shaky + 1 } else { 0 };
            let wait = if !c.up() || (1..=QUICK_ROUNDS).contains(&shaky) { QUICK_SECS } else { interval };
            match asks.recv_timeout(Duration::from_secs(wait)) {
                Err(RecvTimeoutError::Disconnected) => return,
                _ => continue,
            }
        }
    }

    /// Run the scheduled speed tests, and any asked for, while online.
    fn speed_tests(self, asks: Receiver<Ask>) {
        loop {
            let (due, online) = {
                let st = self.state.lock().unwrap();
                let online = st.open.is_none() && st.paused_until.is_none() && st.last.as_ref().is_some_and(|(_, c)| c.up());
                // Until Speedtest is set up, scheduled tests wait.
                (st.next_speed, online && crate::ookla::ready(&st.prefs))
            };
            let wait = due.map_or(Duration::from_secs(60), |d| (d - Local::now()).to_std().unwrap_or_default().min(Duration::from_secs(60)));
            let run = match asks.recv_timeout(wait.max(Duration::from_secs(1))) {
                Ok(Ask::SpeedTest) => true,
                Ok(Ask::Reschedule) => {
                    let mut st = self.state.lock().unwrap();
                    st.next_speed = next_test(st.speeds.last().map(|s| s.t), st.prefs.speed_hours);
                    drop(st);
                    self.changed();
                    false
                }
                Ok(Ask::Recheck) => false,
                Err(RecvTimeoutError::Timeout) => online && due.is_some_and(|d| d <= Local::now()),
                Err(RecvTimeoutError::Disconnected) => return,
            };
            if run {
                self.run_speed_test();
            }
        }
    }

    fn run_speed_test(&self) {
        let t = Local::now();
        {
            let mut st = self.state.lock().unwrap();
            st.testing = true;
            st.live = Some(probe::Live::default());
            st.test_ends = Some(t + Span::seconds(35));
        }
        self.changed();
        // The window redraws with every report; the tray, which goes over
        // the session bus, when the step changes and once a second.
        let tray_at = std::cell::Cell::new((Instant::now(), ""));
        let report = |live: &probe::Live| {
            let mut st = self.state.lock().unwrap();
            st.live = Some(live.clone());
            // Ookla's test sets its own length; the countdown follows it.
            st.test_ends = Some(t + Span::milliseconds((live.secs * 1000.0) as i64));
            drop(st);
            self.repaint();
            let (at, step) = tray_at.get();
            if step != live.step || at.elapsed() >= Duration::from_secs(1) {
                tray_at.set((Instant::now(), live.step));
                crate::refresh_tray();
            }
        };
        let row = match crate::ookla::speed_test(&report) {
            Ok(s) => SpeedRow {
                t,
                down: s.down_mbps,
                up: s.up_mbps,
                ping: s.ping_ms,
                jitter: s.jitter_ms,
                mb: s.bytes as f64 / 1e6,
                error: None,
                server: s.server,
                link: s.link,
            },
            Err(e) => SpeedRow { t, down: 0.0, up: 0.0, ping: 0.0, jitter: 0.0, mb: 0.0, error: Some(e), server: None, link: None },
        };
        store::save_speed(&row);
        {
            let mut st = self.state.lock().unwrap();
            st.testing = false;
            st.test_ends = None;
            st.live = None;
            // A failed test is tried again in 15 minutes, once.
            let failed_before = st.speeds.last().is_some_and(|s| s.error.is_some());
            st.next_speed = if row.error.is_some() && !failed_before && st.prefs.speed_hours > 0 {
                Some(Local::now() + Span::minutes(15))
            } else {
                next_test(Some(t), st.prefs.speed_hours)
            };
            st.speeds.push(row);
        }
        self.changed();
    }
}

pub fn outage_count(n: usize) -> String {
    match n {
        1 => "first outage".into(),
        n => format!("{n} outages"),
    }
}
