#[cfg(not(target_os = "linux"))]
compile_error!("Net Watch is a Linux app: it reads Linux's own network files.");

mod charts;
mod conns;
mod devices;
mod diag;
mod dns;
mod firewall;
mod icon;
mod look;
mod monitor;
mod netif;
mod ookla;
mod probe;
mod route;
mod store;
mod tools;
mod tray;

use chrono::{Duration as Span, Local};
use eframe::egui::{self, Color32, vec2};
use look::{bad, good, warn, pal};
use monitor::{Core, Shared, Status, Wake};
use tools::{Ask, View};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use store::{Cause, Time};

/// Redraw the tray from the shared state (no-op without a tray).
pub fn refresh_tray() {
    tray::refresh();
}

/// A desktop pop-up.
pub fn notify(title: &str, body: &str) {
    tray::notify(title, body);
}

/// "4 min 12 s", "47 min", "3 h 12 min", "2 days 3 h".
pub fn span(secs: i64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d} day{} {h} h", if d == 1 { "" } else { "s" })
    } else if h > 0 {
        format!("{h} h {m} min")
    } else if m >= 10 {
        format!("{m} min")
    } else if m > 0 && s > 0 {
        format!("{m} min {s} s")
    } else if m > 0 {
        format!("{m} min")
    } else {
        format!("{s} s")
    }
}

/// "Today", "Yesterday", "Wed" this week, else "Nov 3".
pub fn day_word(t: Time) -> String {
    match (Local::now().date_naive() - t.date_naive()).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        -1 => "Tomorrow".into(),
        2..=6 => t.format("%a").to_string(),
        _ => t.format("%b %-d").to_string(),
    }
}

/// "Today 2:14 pm".
pub fn when(t: Time) -> String {
    format!("{} {}", day_word(t), t.format("%-I:%M %P"))
}

/// Time left in the running speed test, like "0:42".
pub fn test_left(st: &Shared) -> String {
    let left = st.test_ends.map_or(0, |t| (t - Local::now()).num_seconds().max(0));
    format!("{}:{:02}", left / 60, left % 60)
}

/// Megabytes as "750 MB" or "2.3 GB".
fn data(mb: f64) -> String {
    if mb < 1000.0 { format!("{mb:.0} MB") } else { format!("{:.1} GB", mb / 1000.0) }
}

/// One line on how things are: the tray's first line and tooltip.
pub fn headline(st: &Shared) -> String {
    let now = Local::now();
    match st.status() {
        Status::Starting => "Checking…".into(),
        Status::Paused => format!("Paused until {}", st.paused_until.map_or(String::new(), |t| t.format("%-I:%M %P").to_string())),
        Status::Down => format!("Offline for {}", span(st.open.as_ref().map_or(0, |o| o.secs(now)))),
        s => {
            let lat = st.last.as_ref().and_then(|(_, c)| c.latency).map(|l| format!(" · {:.0} ms", l.as_secs_f64() * 1000.0)).unwrap_or_default();
            let word = if s == Status::Shaky { "Online, some trouble" } else { "Online" };
            format!("{word}{lat}")
        }
    }
}

const RANGES: [(&str, i64); 3] = [("24 h", 1), ("7 days", 7), ("30 days", 30)];
/// Window widths where the layout spreads out.
const WIDE: f32 = 900.0;
const MEDIUM: f32 = 640.0;

struct App {
    core: Core,
    tools: Vec<Box<dyn tools::Tool>>,
    /// The tool on screen; `None` is Home.
    page: Option<usize>,
    range: usize,
    /// A note under the status row: text, whether it's a problem, and a
    /// file it's about.
    message: Option<(String, bool, Option<PathBuf>)>,
    /// Without a tray, closing minimizes so watching carries on.
    has_tray: bool,
    /// Window title last set, so it's only sent when it changes.
    title: String,
    /// Dev aid: save a screenshot here once the window has settled, then quit.
    screenshot: Option<(PathBuf, u32)>,
    /// What the tools asked for, done once the shared state is free.
    asks: Vec<Ask>,
    /// The page on screen and when it came up (the input clock), for
    /// fading it in.
    shown: (Option<usize>, f64),
    /// "Check now" was clicked when the last check was this one; it says
    /// "Checking…" until a newer one is in.
    check_asked: Option<Option<Time>>,
    /// When the check "Check now" asked for came in, for a moment of "Checked".
    checked_at: f64,
    /// Which way a new page slides in: 1 from the right, -1 from the left.
    slide: f32,
}

impl App {
    fn new(core: Core, has_tray: bool) -> Self {
        let tools = tools::all(&core.state.lock().unwrap());
        let mut asks = Vec::new();
        if std::env::var_os("NET_WATCH_FIREWALL").is_some() {
            asks.push(Ask::CheckFirewall);
        }
        if std::env::var_os("NET_WATCH_SCAN").is_some() {
            asks.push(Ask::ScanDevices);
        }
        if std::env::var_os("NET_WATCH_DIAGNOSE").is_some() {
            asks.push(Ask::Diagnose);
        }
        if std::env::var_os("NET_WATCH_DNS").is_some() {
            asks.push(Ask::CheckDns);
        }
        if let Some(to) = std::env::var("NET_WATCH_ROUTE").ok() {
            asks.push(Ask::Trace(to));
        }
        // Dev aids: NET_WATCH_TOOL=Devices opens a tool, NET_WATCH_SETTINGS=1
        // opens Settings.
        let open = std::env::var("NET_WATCH_TOOL").ok().or_else(|| std::env::var_os("NET_WATCH_SETTINGS").map(|_| tools::SETTINGS.into()));
        let page = open.and_then(|name| tools.iter().position(|t| t.name().eq_ignore_ascii_case(&name)));
        Self {
            core,
            tools,
            page,
            range: std::env::var("NET_WATCH_RANGE").ok().and_then(|r| r.parse().ok()).unwrap_or(1usize).min(2),
            message: None,
            has_tray,
            title: String::new(),
            screenshot: std::env::var_os("NET_WATCH_SCREENSHOT").map(|p| (PathBuf::from(p), 0)),
            asks,
            shown: (None, f64::NEG_INFINITY),
            check_asked: None,
            checked_at: f64::NEG_INFINITY,
            slide: 1.0,
        }
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.tools.iter().position(|t| t.name() == name)
    }

    fn status_row(&mut self, ui: &mut egui::Ui, st: &Shared, wide: bool) {
        let now = Local::now();
        let status = st.status();
        let (color, text) = match status {
            Status::Up | Status::Shaky => {
                let text = match st.up_since {
                    Some(t) if (now - t).num_seconds() >= 60 => format!("Online · up for {}", span((now - t).num_seconds())),
                    _ => "Online".into(),
                };
                if status == Status::Up { (good(), text) } else { (warn(), format!("{text} · some checks failing")) }
            }
            Status::Down => (bad(), headline(st)),
            Status::Starting | Status::Paused => (pal().weak, headline(st)),
        };
        let settings = self.find(tools::SETTINGS);
        let on_speed = self.page.is_some() && self.page == self.find(tools::SPEED);
        // Narrow: the pill on a line of its own, the buttons under it.
        if !wide {
            pill(ui, color, &text, st);
        }
        ui.horizontal(|ui| {
            if wide {
                pill(ui, color, &text, st);
                ui.label(egui::RichText::new(self.subline(st, on_speed)).small().color(pal().weak));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let in_settings = self.page.is_some() && self.page == settings;
                if look::button(ui, "⚙", in_settings, true).on_hover_text("Settings").clicked() {
                    self.page = if in_settings { None } else { settings };
                }
                let export_label = if wide { "Export report" } else { "Export" };
                if look::button(ui, export_label, false, true).on_hover_text("Save a report of this period to Downloads").clicked() {
                    self.message = Some(match export(st, RANGES[self.range].1) {
                        Ok(path) => (format!("Report saved to {}", show_path(&path)), false, Some(path)),
                        Err(e) => (format!("Couldn't save the report: {e}"), true, None),
                    });
                }
                // The speed test button only on the Speed test page.
                if !on_speed {
                    return;
                }
                let ready = ookla::ready(&st.prefs);
                let label = match (st.testing, ready, wide) {
                    (true, ..) => format!("Testing… {}", test_left(st)),
                    (false, false, _) => "⚡ Set up speed tests".into(),
                    (false, true, true) => "⚡ Test speed now".into(),
                    (false, true, false) => "⚡ Test speed".into(),
                };
                let can = !st.testing && (!ready || (status != Status::Paused && status != Status::Down));
                if look::button(ui, &label, true, can).clicked() {
                    if ready {
                        self.asks.push(Ask::TestSpeed);
                    } else {
                        self.page = settings;
                    }
                }
            });
        });
        if !wide {
            ui.label(egui::RichText::new(self.subline(st, on_speed)).small().color(pal().weak));
        }
    }

    /// "Checked 12 s ago", and on the Speed test page "· next speed test 4:40 pm".
    fn subline(&self, st: &Shared, speed: bool) -> String {
        let now = Local::now();
        let mut parts = Vec::new();
        if let Some((t, _)) = &st.last {
            parts.push(format!("Checked {} ago", span((now - *t).num_seconds())));
        }
        match (st.testing, st.next_speed) {
            _ if !speed => {}
            (true, _) => parts.push("speed test running".into()),
            _ if !ookla::ready(&st.prefs) => parts.push("speed tests not set up".into()),
            (false, Some(t)) => {
                let at = if t.date_naive() == now.date_naive() { t.format("%-I:%M %P").to_string() } else { when(t) };
                parts.push(format!("next speed test {at}"));
            }
            (false, None) => parts.push("scheduled speed tests off".into()),
        }
        parts.join(" · ")
    }

    fn message_row(&mut self, ui: &mut egui::Ui) {
        let Some((text, error, path)) = self.message.clone() else { return };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&text).small().color(if error { warn() } else { good() }));
            if let Some(path) = path
                && ui.small_button("Open").clicked()
            {
                open_path(&path);
            }
            if ui.small_button("✕").clicked() {
                self.message = None;
            }
        });
    }

    /// Home: how things are, then a card for each tool. Clicking a card
    /// opens its tool; a button on one acts straight away.
    fn home(&mut self, ui: &mut egui::Ui, st: &Shared) {
        self.home_header(ui, st);
        self.message_row(ui);
        ui.horizontal(|ui| {
            ui.label(look::caption("Tools"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let keys = format!("Choose a tool to see more, or press 1–{}", self.tools.len());
                ui.label(egui::RichText::new(keys).small().color(pal().weak));
            });
        });
        let cols = if ui.available_width() >= MEDIUM { 2 } else { 1 };
        ui.spacing_mut().item_spacing.x = 14.0;
        let (mut open, mut act) = (None, None);
        let since = self.shown.1;
        for (r, row) in self.tools.chunks(cols).enumerate() {
            ui.columns(cols, |c| {
                for (j, (tool, ui)) in row.iter().zip(c).enumerate() {
                    // The cards come in one after another.
                    let t = look::progress(ui.ctx(), since + 0.06 * (r * cols + j) as f64, 0.4);
                    ui.multiply_opacity(t);
                    look::set_appear(ui.ctx(), t);
                    // And rise into place.
                    let rise = egui::emath::TSTransform::from_translation(vec2(0.0, 16.0 * (1.0 - t)));
                    match ui.with_visual_transform(rise, |ui| tool_card(ui, tool.as_ref(), st)).inner {
                        Card::Open => open = Some(r * cols + j),
                        Card::Act(ask) => act = Some(ask),
                        Card::Idle => {}
                    }
                }
            });
        }
        look::set_appear(ui.ctx(), 1.0);
        match act {
            Some(ask) => self.asks.push(ask),
            None if open.is_some() => self.page = open,
            None => {}
        }
    }

    /// The top of Home: the status with its latency and a line of recent
    /// latency, when it last checked, and the report button.
    fn home_header(&mut self, ui: &mut egui::Ui, st: &Shared) {
        let now = Local::now();
        let status = st.status();
        let online = matches!(status, Status::Up | Status::Shaky);
        let (color, word) = match status {
            Status::Up => (good(), "Online".to_string()),
            Status::Shaky => (warn(), "Online, some trouble".into()),
            Status::Down => (bad(), format!("Offline for {}", span(st.open.as_ref().map_or(0, |o| o.secs(now))))),
            Status::Starting => (pal().weak, "Checking…".into()),
            Status::Paused => (pal().weak, headline(st)),
        };
        let narrow = ui.available_width() < MEDIUM;
        let export = |app: &mut Self, ui: &mut egui::Ui| {
            if look::button(ui, "Export report", true, true).on_hover_text("Save a report of this period to Downloads").clicked() {
                app.message = Some(match export(st, RANGES[app.range].1) {
                    Ok(path) => (format!("Report saved to {}", show_path(&path)), false, Some(path)),
                    Err(e) => (format!("Couldn't save the report: {e}"), true, None),
                });
            }
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            status_mark(ui, color, status, st);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&word).size(21.0).strong());
                    if let Some(l) = st.last.as_ref().and_then(|(_, c)| c.latency).filter(|_| online) {
                        // Counts its way to each new reading.
                        let ms = look::glide(ui.ctx(), egui::Id::new("latency"), l.as_secs_f64() as f32 * 1000.0, 0.6);
                        tools::pill(ui, &format!("{ms:.0} ms"), color);
                    }
                    if !narrow {
                        sparkline(ui, st, color, look::progress(ui.ctx(), self.shown.1, 0.7));
                        if tools::network::chip(ui, st) {
                            self.page = self.find(tools::NETWORK);
                        }
                    }
                });
                if narrow {
                    ui.horizontal(|ui| {
                        if tools::network::chip(ui, st) {
                            self.page = self.find(tools::NETWORK);
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let weak = |t: &str| egui::RichText::new(t).small().color(pal().weak);
                    match &st.last {
                        Some((t, _)) => {
                            ui.label(weak("Checked"));
                            ui.label(egui::RichText::new(format!("{} ago", span((now - *t).num_seconds()))).small().strong());
                        }
                        None => {
                            ui.label(weak("Not checked yet"));
                        }
                    }
                    ui.label(weak(if self.has_tray { "· watching quietly in the tray" } else { "· watching" }));
                    ui.add_space(4.0);
                    let last = st.last.as_ref().map(|(t, _)| *t);
                    if self.check_asked.is_some_and(|asked| asked != last) {
                        self.check_asked = None;
                        self.checked_at = ui.input(|i| i.time);
                    }
                    let done = look::playing(ui.ctx(), self.checked_at, 1.6);
                    let (label, can) = match (self.check_asked, done) {
                        (Some(_), _) => ("Checking…", false),
                        (None, Some(_)) => ("✔ Checked", true),
                        _ => ("Check now", status != Status::Paused),
                    };
                    let chip = look::chip(ui, label, can, true);
                    if let Some(t) = done {
                        // A ring spreading from the chip as the answer comes in.
                        let k = egui::emath::easing::cubic_out(t);
                        let ring = good().gamma_multiply(0.7 * (1.0 - k));
                        ui.painter().rect_stroke(chip.rect.expand(6.0 * k), 7.0 + 6.0 * k, egui::Stroke::new(2.0 * (1.0 - k) + 0.5, ring), egui::StrokeKind::Outside);
                    }
                    if chip.on_hover_text("Check the connection now (F5)").clicked() {
                        self.check_asked = Some(last);
                        self.asks.push(Ask::CheckNow);
                    }
                    // When it's down or shaky, the way to find out why.
                    if matches!(status, Status::Down | Status::Shaky)
                        && look::chip(ui, "What's wrong?", !st.diagnosing, true).on_hover_text("Check step by step where it breaks").clicked()
                    {
                        self.asks.push(Ask::Open(tools::DIAGNOSE));
                        self.asks.push(Ask::Diagnose);
                    }
                });
            });
            if !narrow {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| export(self, ui));
            }
        });
        if narrow {
            export(self, ui);
        }
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
        ui.painter().rect_filled(line, 0.0, pal().edge);
    }

    /// Esc, Alt+Left or the mouse's back button go back to Home; 1–9 on
    /// Home open a tool; F5 checks now.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let (back, number, check) = ctx.input(|i| {
            let back = i.key_pressed(egui::Key::Escape)
                || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft))
                || i.pointer.button_pressed(egui::PointerButton::Extra1);
            let keys = [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4, egui::Key::Num5, egui::Key::Num6, egui::Key::Num7, egui::Key::Num8, egui::Key::Num9];
            (back, keys.iter().position(|k| i.key_pressed(*k)), i.key_pressed(egui::Key::F5))
        });
        if back {
            self.page = None;
        } else if let Some(n) = number.filter(|&n| self.page.is_none() && n < self.tools.len()) {
            self.page = Some(n);
        }
        if check && self.check_asked.is_none() {
            self.check_asked = Some(self.core.state.lock().unwrap().last.as_ref().map(|(t, _)| *t));
            self.asks.push(Ask::CheckNow);
        }
    }

    fn take_screenshot(&mut self, ui: &egui::Ui) {
        let Some((path, frames)) = &mut self.screenshot else { return };
        if *frames == u32::MAX {
            // Take the image out first: sending a viewport command while
            // holding the input lock deadlocks.
            let image = ui.input(|i| {
                i.raw.events.iter().find_map(|ev| match ev {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = image {
                let mut ppm = format!("P6 {} {} 255\n", image.size[0], image.size[1]).into_bytes();
                for px in &image.pixels {
                    ppm.extend_from_slice(&[px.r(), px.g(), px.b()]);
                }
                let _ = std::fs::write(&*path, ppm);
                self.core.quit();
            }
            return;
        }
        *frames += 1;
        ui.ctx().request_repaint();
        // Wait for the first check, then a few frames for animations; with
        // NET_WATCH_SHOT_AT=secs, until that long after starting.
        static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let started = STARTED.get_or_init(std::time::Instant::now);
        let at = std::env::var("NET_WATCH_SHOT_AT").ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        if started.elapsed().as_secs_f64() < at {
            return;
        }
        if (*frames > 60 && self.core.state.lock().unwrap().last.is_some()) || *frames > 900 {
            *frames = u32::MAX;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // "Checked 12 s ago" keeps counting.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        self.take_screenshot(ui);
        if !self.has_tray && ctx.input(|i| i.viewport().close_requested()) && !self.core.state.lock().unwrap().quit {
            // No tray to come back from: keep watching, minimized.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        self.shortcuts(&ctx);
        if self.shown.0 != self.page || self.shown.1 == f64::NEG_INFINITY {
            // A tool comes in from the right; Home from the left.
            self.slide = if self.page.is_none() && self.shown.0.is_some() { -1.0 } else { 1.0 };
            self.shown = (self.page, ctx.input(|i| i.time));
        }
        let state = self.core.state.clone();
        let mut st = state.lock().unwrap();
        if std::mem::take(&mut st.open_settings) {
            self.page = self.find(tools::SETTINGS);
        }
        let title = if st.status() == Status::Down { format!("Net Watch · {}", headline(&st)) } else { "Net Watch".into() };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        let frame = egui::Frame::new().fill(pal().bg).inner_margin(egui::Margin::same(16));
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            let mut area = egui::ScrollArea::vertical().auto_shrink(false).id_salt(self.page);
            // Dev aid: start scrolled down, for screenshots.
            if let Some(y) = std::env::var("NET_WATCH_SCROLL").ok().and_then(|y| y.parse().ok()) {
                area = area.vertical_scroll_offset(y);
            }
            let fade = look::progress(ui.ctx(), self.shown.1, 0.2);
            let slide = egui::emath::TSTransform::from_translation(vec2(self.slide * 18.0 * (1.0 - fade), 0.0));
            area.show(ui, |ui| ui.with_visual_transform(slide, |ui| {
                ui.multiply_opacity(fade);
                ui.spacing_mut().item_spacing.y = 12.0;
                if let Some(i) = self.page {
                    ui.horizontal(|ui| {
                        if look::button(ui, "‹  All tools", false, true).on_hover_text("Back to all the tools (Esc)").clicked() {
                            self.page = None;
                        }
                        ui.heading(self.tools[i].name());
                    });
                    let medium = ui.available_width() >= MEDIUM;
                    self.status_row(ui, &st, medium);
                    self.message_row(ui);
                }
                match self.page {
                    Some(i) => {
                        let tool = &mut self.tools[i];
                        // In its own column, as the rows above are already full width.
                        ui.vertical(|ui| {
                            ui.set_max_width(tool.max_width().min(ui.available_width()));
                            tool.show(ui, &mut View { st: &st, range: &mut self.range, asks: &mut self.asks });
                        });
                    }
                    None => self.home(ui, &st),
                }
            }));
        });
        drop(st);
        for ask in std::mem::take(&mut self.asks) {
            match ask {
                Ask::CheckFirewall => self.core.check_firewall(),
                Ask::Diagnose => self.core.diagnose(),
                Ask::Trace(to) => self.core.trace(to),
                Ask::CheckDns => self.core.check_dns(),
                Ask::LookConns => self.core.look_conns(),
                Ask::SignIn => open_path(std::path::Path::new("http://cp.cloudflare.com/generate_204")),
                Ask::ScanDevices => self.core.scan_devices(),
                Ask::TestSpeed => self.core.test_speed_now(),
                Ask::CheckNow => self.core.check_now(),
                Ask::Open(name) => self.page = self.find(name),
                Ask::Note(text, error, path) => self.message = Some((text, error, path)),
                Ask::SetPrefs(p) => self.core.set_prefs(p),
                Ask::Net(what) => netif::run(&self.core, what),
                Ask::Quit => self.core.quit(),
            }
        }
    }
}

/// What happened to a card on Home this frame.
enum Card {
    Idle,
    Open,
    /// Its button was clicked.
    Act(Ask),
}

/// A tool's card on Home: its name, how things stand with a small picture,
/// and a line at the foot. The whole card opens the tool.
fn tool_card(ui: &mut egui::Ui, tool: &dyn tools::Tool, st: &Shared) -> Card {
    let mut button: Option<(egui::Rect, Option<Ask>)> = None;
    look::take_tip(ui.ctx());
    // The icon hops when the pointer comes onto the card.
    let id = ui.id().with(tool.name());
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let came = look::changed_at(ui.ctx(), id.with("hop"), hovered as u64);
    let hop = match look::playing(ui.ctx(), came, 0.45) {
        Some(t) if hovered => -5.0 * (t * std::f32::consts::PI).sin(),
        _ => 0.0,
    };
    let resp = look::tile(ui, tool.name(), |ui| {
        ui.spacing_mut().item_spacing.y = 10.0;
        // Rows of text, not buttons: no room kept for clicking.
        ui.spacing_mut().interact_size.y = 18.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let hop = egui::emath::TSTransform::from_translation(vec2(0.0, hop));
            ui.with_visual_transform(hop, |ui| match tool.icon() {
                // A plain dot, drawn: not every font has one.
                "●" => {
                    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 5.0, tool.icon_color(st));
                }
                icon => {
                    ui.label(egui::RichText::new(icon).size(15.0).color(tool.icon_color(st)));
                }
            });
            ui.label(egui::RichText::new(tool.name()).size(16.5).strong());
            if let Some((text, color)) = tool.badge(st) {
                tools::pill(ui, &text, color);
            }
            if let Some((label, ask)) = tool.action(st) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Drawn only: the card is on top and takes the click.
                    let rect = look::chip(ui, &label, ask.is_some(), false).rect;
                    button = Some((rect, ask));
                });
            }
        });
        let top = ui.cursor().top();
        let glance = tool.glance_width();
        ui.horizontal_top(|ui| {
            let body = (ui.available_width() - if glance > 0.0 { glance + 16.0 } else { 0.0 }).max(80.0);
            ui.allocate_ui_with_layout(vec2(body, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(body);
                ui.spacing_mut().item_spacing.y = 6.0;
                tool.card(ui, st);
            });
            if glance > 0.0 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| tool.glance(ui, st));
            }
        });
        // The same height for every card, so a row lines up.
        let used = ui.cursor().top() - top;
        if used < 70.0 {
            ui.add_space(70.0 - used);
        }
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
        ui.painter().rect_filled(line, 0.0, pal().edge);
        ui.horizontal(|ui| {
            let room = ui.available_width() - 24.0;
            ui.allocate_ui_with_layout(vec2(room, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_width(room);
                ui.add(egui::Label::new(egui::RichText::new(tool.footer(st)).small().color(pal().weak)).truncate());
            });
            ui.label(egui::RichText::new("›").size(18.0).color(pal().accent));
        });
    });
    let resp = match look::take_tip(ui.ctx()) {
        Some(tip) => resp.on_hover_text_at_pointer(tip),
        None => resp,
    };
    if !resp.clicked() {
        return Card::Idle;
    }
    let at = ui.ctx().pointer_interact_pos();
    match button {
        Some((rect, ask)) if at.is_some_and(|p| rect.contains(p)) => ask.map_or(Card::Idle, Card::Act),
        _ => Card::Open,
    }
}

/// How far through the ripple for the latest check (0 to 1), while it plays
/// and the window is the one in use. Rippling once per check, rather than
/// looping, keeps an open window nearly idle between checks.
fn check_ripple(ui: &egui::Ui, st: &Shared, id: &str, secs: f64) -> Option<f32> {
    let newest = st.last.as_ref().map_or(0, |(t, _)| t.timestamp_millis() as u64);
    let came = look::changed_at(ui.ctx(), egui::Id::new(id), newest);
    if !ui.input(|i| i.viewport().focused.unwrap_or(true)) {
        return None;
    }
    look::playing(ui.ctx(), came, secs)
}

/// The status as a rounded square in its color. A ring spreads from the
/// dot as each check comes in while online, a hollow one with each try
/// while offline. It pops when the status changes, with a shake when the
/// connection drops.
fn status_mark(ui: &mut egui::Ui, color: Color32, status: Status, st: &Shared) {
    let (rect, _) = ui.allocate_exact_size(vec2(46.0, 46.0), egui::Sense::hover());
    let ctx = ui.ctx().clone();
    let changed = look::changed_at(&ctx, egui::Id::new("status mark"), status as u64);
    let (scale, shake) = match look::playing(&ctx, changed, 0.7) {
        Some(t) => {
            let shake = if status == Status::Down { 5.0 * (t * std::f32::consts::TAU * 3.0).sin() * (1.0 - t) } else { 0.0 };
            (0.7 + 0.3 * egui::emath::easing::back_out(t), shake)
        }
        None => (1.0, 0.0),
    };
    let rect = egui::Rect::from_center_size(rect.center() + vec2(shake, 0.0), rect.size() * scale);
    let p = ui.painter();
    p.rect_filled(rect, 15.0 * scale, color.gamma_multiply(0.14));
    p.rect_stroke(rect, 15.0 * scale, egui::Stroke::new(1.0, color.gamma_multiply(0.3)), egui::StrokeKind::Inside);
    let c = rect.center();
    let dot = 6.0 * scale;
    let ripple = check_ripple(ui, st, "status ripple", 2.2).map(egui::emath::easing::cubic_out);
    let focused = ui.input(|i| i.viewport().focused.unwrap_or(true));
    if look::animated() {
        let now = ui.input(|i| i.time);
        match status {
            Status::Up | Status::Shaky => {
                if let Some(k) = ripple {
                    p.circle_filled(c, dot + 9.0 * k, color.gamma_multiply(0.45 * (1.0 - k)));
                }
            }
            Status::Down => {
                if let Some(k) = ripple {
                    p.circle_stroke(c, dot + 12.0 * k, egui::Stroke::new(1.5, color.gamma_multiply(0.6 * (1.0 - k))));
                }
            }
            // Three dots taking turns while the first check is out (a few
            // seconds at most), when the window is in use.
            Status::Starting if focused => {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
                for i in 0..3 {
                    let up = ((now * 4.0 - i as f64 * 0.9).sin() * 0.5 + 0.5) as f32;
                    p.circle_filled(c + vec2((i as f32 - 1.0) * 9.0, -3.0 * up), 3.0, color.gamma_multiply(0.5 + 0.5 * up));
                }
                return;
            }
            _ => {}
        }
    }
    p.circle_filled(c, dot, color);
}

/// Latency over the last half hour or so, as a small line in a box. It
/// draws itself in from the left as Home comes up (`grow` 0 to 1), and the
/// newest point ripples when a check comes in.
fn sparkline(ui: &mut egui::Ui, st: &Shared, color: Color32, grow: f32) {
    let points: Vec<f32> = st.checks.iter().rev().take(40).filter_map(|c| c.latency_ms).collect();
    if points.len() < 2 {
        return;
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(112.0, 26.0), egui::Sense::hover());
    let (lo, hi) = points.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    resp.on_hover_text(format!("Latency of the last {} checks: {lo:.0} to {hi:.0} ms", points.len()));
    let p = ui.painter();
    p.rect_filled(rect, 8.0, pal().body);
    p.rect_stroke(rect, 8.0, egui::Stroke::new(1.0, pal().edge), egui::StrokeKind::Inside);
    let inner = rect.shrink2(vec2(8.0, 6.0));
    let span = (hi - lo).max(4.0);
    let n = points.len() - 1;
    let line: Vec<egui::Pos2> = points
        .iter()
        .rev()
        .enumerate()
        .map(|(i, v)| egui::pos2(inner.left() + inner.width() * i as f32 / n as f32, inner.bottom() - inner.height() * (v - lo) / span))
        .collect();
    let shown = ((line.len() as f32 * grow).ceil() as usize).clamp(2, line.len());
    let end = line[shown - 1];
    p.add(egui::Shape::line(line[..shown].to_vec(), egui::Stroke::new(1.6, color)));
    let newest = st.last.as_ref().map_or(0, |(t, _)| t.timestamp_millis() as u64);
    let came = look::changed_at(ui.ctx(), egui::Id::new("sparkline newest"), newest);
    if let Some(t) = look::playing(ui.ctx(), came, 0.9) {
        let k = egui::emath::easing::cubic_out(t);
        p.circle_filled(end, 2.5 + 6.0 * k, color.gamma_multiply(0.4 * (1.0 - k)));
    }
    p.circle_filled(end, 2.5, color);
}

/// The status pill: a dot that takes a breath as each check comes in.
fn pill(ui: &mut egui::Ui, color: Color32, text: &str, st: &Shared) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(16.0), color);
    let size = vec2(galley.size().x + 52.0, 36.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 18.0, color.gamma_multiply(0.14));
    let c = egui::pos2(rect.left() + 20.0, rect.center().y);
    let breathe = check_ripple(ui, st, "pill breath", 2.2).map_or(0.5, |t| 0.5 + 0.5 * (t * std::f32::consts::TAU).sin());
    p.circle_filled(c, 6.0 + 4.0 * breathe, color.gamma_multiply(0.3 * (1.0 - breathe) + 0.08));
    p.circle_filled(c, 6.0, color);
    p.galley(egui::pos2(rect.left() + 36.0, rect.center().y - galley.size().y / 2.0), galley, color);
}

fn big(value: String, color: Option<Color32>) -> egui::RichText {
    egui::RichText::new(value).size(28.0).strong().color(color.unwrap_or(pal().text))
}

/// "412 down  98 up" with the numbers big; smaller in a narrow tile, so it
/// stays on one line.
fn speed_value(ui: &mut egui::Ui, down: f64, up: f64) {
    let line = |scale: f32| {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        let fmt = |size: f32, color: Color32| egui::TextFormat {
            font_id: egui::FontId::proportional(size * scale),
            color,
            valign: egui::Align::BOTTOM,
            ..Default::default()
        };
        job.append(&format!("{down:.0}"), 0.0, fmt(28.0, pal().text));
        job.append(" down   ", 0.0, fmt(14.0, pal().weak));
        job.append(&format!("{up:.0}"), 0.0, fmt(28.0, pal().text));
        job.append(" up", 0.0, fmt(14.0, pal().weak));
        ui.painter().layout_job(job)
    };
    let mut galley = line(1.0);
    let room = ui.available_width();
    if galley.size().x > room {
        galley = line((room / galley.size().x).max(0.5));
    }
    ui.label(galley);
}

/// A small colored label with a tinted background.
fn tag(ui: &mut egui::Ui, text: &str, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::TextStyle::Small.resolve(ui.style()), color);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(16.0, 6.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 6.0, color.gamma_multiply(0.16));
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
}

/// A failed speed test's error, in plain words.
pub fn speed_error(e: &str) -> &'static str {
    match e {
        e if e.contains("isn't installed") => "Speedtest isn't installed (see Settings)",
        e if e.contains("icense") || e.contains("gdpr") => "Ookla's license needs accepting (see Settings)",
        e if e.contains("429") || e.contains("403") => "the test server asked us to slow down",
        e if e.contains("was stopped") => "the test hung and was stopped after 2 minutes",
        e if e.contains("timeout") => "the test server stopped answering",
        e if e.contains("didn't answer") || e.contains("onnect") || e.contains("resolve") || e.contains("dns") => {
            "couldn't reach the test server"
        }
        _ => "the test server sent an error",
    }
}

fn theme_index(name: &str) -> usize {
    look::THEMES.iter().position(|(n, _)| *n == name).unwrap_or(0)
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// A path as people know it: `~/Downloads/x` rather than `/home/me/Downloads/x`.
fn show_path(path: &std::path::Path) -> String {
    match path.strip_prefix(home()) {
        Ok(rel) => format!("~{}{}", std::path::MAIN_SEPARATOR, rel.display()),
        Err(_) => path.display().to_string(),
    }
}

fn open_path(path: &std::path::Path) {
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Write a plain-text report of the last `days` to the Downloads folder:
/// the summary, every outage and every speed test.
/// Percent of the watched time online. Checks carry on during an outage, so
/// the watched time already includes it; an outage can also run on while
/// the app was closed, so it's capped at the watched time.
fn uptime(watched: i64, off: i64) -> f64 {
    100.0 * (1.0 - off.min(watched) as f64 / watched.max(1) as f64)
}

/// Seconds watched and seconds offline since `from`.
fn watched_off(st: &Shared, from: Time) -> (i64, i64) {
    let now = Local::now();
    let checks = charts::checks_in(&st.checks, from, now + Span::seconds(1));
    let watched = charts::watched_secs(checks, st.prefs.check_secs as i64 * 3 + 30);
    (watched, charts::offline_secs(st.outages_since(from), from, now, now))
}

fn export(st: &Shared, days: i64) -> std::io::Result<PathBuf> {
    use std::fmt::Write;
    let now = Local::now();
    let from = now - Span::days(days);
    let (watched, off) = watched_off(st, from);
    let outages: Vec<_> = st.outages_since(from).collect();
    let long = "%a %b %-d %Y, %-I:%M %P";
    let stamp = "%a %b %-d, %-I:%M:%S %P";
    let mut r = String::new();
    let _ = writeln!(r, "Internet connection report");
    let _ = writeln!(r, "From {} to {}", from.format(long), now.format(long));
    let _ = writeln!(r, "Recorded by Net Watch {}, checking the connection every {} s.\n", env!("CARGO_PKG_VERSION"), st.prefs.check_secs);
    let _ = writeln!(r, "Time watched:  {}", span(watched));
    let _ = writeln!(r, "Time offline:  {}", span(off));
    let _ = writeln!(r, "Uptime:        {:.2} %", uptime(watched, off));
    let _ = writeln!(r, "Outages:       {}", outages.len());
    let provider = outages.iter().filter(|o| o.cause == Cause::Provider).count();
    let _ = writeln!(r, "  {provider} of them with the home router still answering (the problem was past the router)");
    let _ = writeln!(r, "\nOUTAGES\n{:<28}{:<28}{:<14}What happened", "Started", "Ended", "Lasted");
    for o in &outages {
        let end = o.end.map_or("still down".into(), |e| e.format(stamp).to_string());
        let _ = writeln!(r, "{:<28}{:<28}{:<14}{}", o.start.format(stamp).to_string(), end, span(o.secs(now)), o.detail);
    }
    let _ = writeln!(r, "\nSPEED TESTS");
    if st.prefs.plan_down > 0.0 {
        let _ = writeln!(r, "Plan: {:.0} Mbps down / {:.0} Mbps up", st.prefs.plan_down, st.prefs.plan_up);
    }
    let _ = writeln!(r, "{:<28}{:>10}{:>10}{:>10}   Server", "Time", "Down Mbps", "Up Mbps", "Ping ms");
    for s in st.speeds.iter().filter(|s| s.t >= from) {
        let t = s.t.format(stamp).to_string();
        let _ = match &s.error {
            Some(e) => writeln!(r, "{t:<28}failed: {e}"),
            None => writeln!(
                r,
                "{t:<28}{:>10.1}{:>10.1}{:>10.1}   {}{}",
                s.down,
                s.up,
                s.ping,
                s.server.as_deref().unwrap_or(""),
                s.link.as_deref().map(|l| format!(" · {l}")).unwrap_or_default()
            ),
        };
    }
    if let Some(f) = &st.firewall {
        let _ = writeln!(r, "\nFIREWALL CHECK ({})", f.at.format(stamp));
        let _ = writeln!(r, "This computer: {}", f.computer);
        for l in &f.lines {
            let (word, detail) = firewall::verdict(&l.outcome);
            let _ = writeln!(r, "{:<26}{:<26}{:<10}{detail}", l.name, l.what, word);
        }
    }
    // The device list stays out: the report is for sending to the provider,
    // and what's in someone's home is none of its business.
    let downloads = home().join("Downloads");
    let dir = if downloads.is_dir() { downloads } else { home() };
    let path = dir.join(format!("Net Watch report {}.txt", now.format("%Y-%m-%d")));
    std::fs::write(&path, r)?;
    Ok(path)
}

/// The window's view of the long-lived `App`, which survives the window
/// closing to the tray and reopening.
struct Window(Rc<RefCell<App>>);

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.borrow_mut().ui(ui);
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--probe") => {
            probe::print_once();
            return Ok(());
        }
        Some("--conns") => {
            conns::print_once();
            return Ok(());
        }
        Some("--dns") => {
            dns::print_once();
            return Ok(());
        }
        Some("--route") => {
            route::print_once(args.get(1).map_or("Cloudflare", String::as_str));
            return Ok(());
        }
        Some("--diagnose") => {
            diag::print_once();
            return Ok(());
        }
        Some("--scan") => {
            devices::print_once();
            return Ok(());
        }
        Some("--write-icon") => {
            let path = args.get(1).map_or("icon.png", String::as_str);
            let size = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(256);
            if let Err(e) = icon::write_png(path, size) {
                eprintln!("net-watch: {e}");
            }
            return Ok(());
        }
        _ => {}
    }
    if !store::single_instance() {
        eprintln!("net-watch: already running (look for it in the tray)");
        return Ok(());
    }
    // Dev aid: open at another size, e.g. NET_WATCH_SIZE=1000x700.
    let size = std::env::var("NET_WATCH_SIZE")
        .ok()
        .and_then(|s| s.split_once('x').and_then(|(w, h)| Some([w.parse().ok()?, h.parse().ok()?])))
        .unwrap_or([1100.0, 860.0]);
    let (core, wake) = Core::start();
    {
        let core = core.clone();
        std::thread::spawn(move || netif::watch(core));
    }
    let has_tray = tray::start(core.clone());

    let app = Rc::new(RefCell::new(App::new(core.clone(), has_tray)));
    // Started at login: stay in the tray until asked.
    if has_tray && args.iter().any(|a| a == "--hidden") {
        match wake.recv() {
            Ok(Wake::Open) => {}
            Ok(Wake::Quit) | Err(_) => std::process::exit(0),
        }
    }
    loop {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size(size)
                .with_min_inner_size([380.0, 520.0])
                .with_icon(egui::IconData { rgba: icon::app_rgba(128), width: 128, height: 128 })
                .with_app_id("net-watch"),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        };
        let (window, state) = (app.clone(), core.state.clone());
        eframe::run_native(
            "Net Watch",
            options,
            Box::new(move |cc| {
                let prefs = state.lock().unwrap().prefs.clone();
                look::set_animated(prefs.animate);
                look::set_theme(&cc.egui_ctx, theme_index(&prefs.theme));
                state.lock().unwrap().ctx = Some(cc.egui_ctx.clone());
                window.borrow_mut().title.clear();
                refresh_tray();
                Ok(Box::new(Window(window)))
            }),
        )?;
        core.state.lock().unwrap().ctx = None;
        refresh_tray();
        // Closing the window hides it to the tray; the tray reopens it.
        if !has_tray || core.state.lock().unwrap().quit {
            break;
        }
        match wake.recv() {
            Ok(Wake::Open) => continue,
            Ok(Wake::Quit) | Err(_) => break,
        }
    }
    // The checker and tray threads don't need to wind down.
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn uptime() {
        // 100 h watched, 10 of them down: 90 %, not 90.9 %.
        assert_eq!(super::uptime(100 * 3600, 10 * 3600), 90.0);
        assert_eq!(super::uptime(3600, 0), 100.0);
        // An outage that ran on while the app was closed.
        assert_eq!(super::uptime(3600, 7200), 0.0);
        assert_eq!(super::uptime(0, 0), 100.0);
    }
}
