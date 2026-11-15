//! The network adapters: which one the internet goes through, each one's
//! settings, and joining Wi-Fi networks. Changes to the one in use are
//! undone if the internet doesn't come back.
use super::{Ask, Tool, View};
use crate::look::{self, bad, good, warn, pal};
use crate::monitor::Shared;
use crate::netif::{Adapter, Do, Kind, Profile, Stage, Trial, WifiNet};
use crate::tag;
use eframe::egui::{self, Color32, Stroke, vec2};
use std::net::Ipv4Addr;

pub const NAME: &str = "Network";

const DNS: [(&str, &[&str]); 5] = [
    ("Router's", &[]),
    ("Cloudflare", &["1.1.1.1", "1.0.0.1"]),
    ("Google", &["8.8.8.8", "8.8.4.4"]),
    ("Quad9", &["9.9.9.9", "149.112.112.112"]),
    ("Custom", &[]),
];
const UNDO: [(u64, &str); 4] = [(15, "15 s"), (30, "30 s"), (60, "1 min"), (120, "2 min")];

pub struct Network {
    /// The adapter picked at the top.
    chosen: Option<String>,
    /// The settings as they were, and as being edited.
    edit: Option<(Edit, Edit)>,
    /// The Wi-Fi list is open.
    picking: bool,
    /// The network opened in the list; "" for a hidden one.
    open: Option<String>,
    password: String,
    show_password: bool,
    auto_join: bool,
    hidden_name: String,
    /// The adapter last scanned for when the list opened.
    scanned_for: Option<String>,
}

impl Network {
    pub fn new() -> Self {
        Self {
            chosen: None,
            edit: None,
            // Dev aid: NET_WATCH_PICK=1 opens the Wi-Fi list, for screenshots.
            picking: std::env::var_os("NET_WATCH_PICK").is_some(),
            open: None,
            password: String::new(),
            show_password: false,
            auto_join: true,
            hidden_name: String::new(),
            scanned_for: None,
        }
    }
}

/// A profile as the screen shows it: text boxes and choices.
#[derive(Clone, PartialEq)]
struct Edit {
    dev: String,
    base: Profile,
    manual: bool,
    address: String,
    mask: String,
    gateway: String,
    dns: usize,
    custom: String,
    auto: bool,
    prefer: bool,
}

impl Edit {
    fn new(a: &Adapter, p: &Profile) -> Self {
        // Manual starts from the address in use, to change from there.
        let (addr, prefix) = match (p.manual, a.address.as_deref()) {
            (true, _) => split(&p.address),
            (false, Some(now)) => split(now),
            _ => (String::new(), 24),
        };
        let gateway = if p.manual { p.gateway.clone() } else { a.gateway.clone().unwrap_or_default() };
        let dns = if !p.ignore_auto_dns && (p.dns.is_empty() || (p.manual && p.dns == [p.gateway.clone()])) {
            0
        } else {
            DNS.iter().position(|(_, list)| !list.is_empty() && p.dns == *list).unwrap_or(4)
        };
        Self {
            dev: a.dev.clone(),
            base: p.clone(),
            manual: p.manual,
            address: addr,
            mask: crate::netif::mask(prefix),
            gateway,
            dns,
            custom: if dns == 4 { p.dns.join(", ") } else { String::new() },
            auto: p.autoconnect,
            prefer: (0..100).contains(&p.metric),
        }
    }

    /// The profile these choices make, or what's wrong with them.
    fn profile(&self) -> Result<Profile, String> {
        let mut p = self.base.clone();
        p.manual = self.manual;
        p.autoconnect = self.auto;
        if self.manual {
            let addr: Ipv4Addr = self.address.trim().parse().map_err(|_| "The address isn't right; it looks like 192.168.1.60".to_string())?;
            let prefix = crate::netif::prefix(&self.mask).ok_or("The mask isn't right; it's usually 255.255.255.0")?;
            let gw: Ipv4Addr = self.gateway.trim().parse().map_err(|_| "The router's address isn't right; it looks like 192.168.1.1".to_string())?;
            p.address = format!("{addr}/{prefix}");
            p.gateway = gw.to_string();
        }
        (p.dns, p.ignore_auto_dns) = match self.dns {
            // With a fixed address nothing comes from the router: ask it by name.
            0 if self.manual => (vec![p.gateway.clone()], false),
            0 => (Vec::new(), false),
            4 => {
                let list: Result<Vec<String>, _> =
                    self.custom.split([',', ' ']).filter(|s| !s.is_empty()).map(|s| s.parse::<Ipv4Addr>().map(|a| a.to_string())).collect();
                match list {
                    Ok(l) if !l.is_empty() => (l, true),
                    _ => return Err("Custom DNS needs addresses like 1.1.1.1, 9.9.9.9".into()),
                }
            }
            i => (DNS[i].1.iter().map(|s| s.to_string()).collect(), true),
        };
        if self.prefer != (0..100).contains(&self.base.metric) {
            p.metric = if self.prefer { 50 } else { -1 };
        }
        Ok(p)
    }
}

/// "192.168.1.42/24" into the address and 24.
fn split(cidr: &str) -> (String, u8) {
    match cidr.split_once('/') {
        Some((a, n)) => (a.to_string(), n.parse().unwrap_or(24)),
        None => (cidr.to_string(), 24),
    }
}

impl Tool for Network {
    fn name(&self) -> &'static str {
        NAME
    }

    fn icon(&self) -> &'static str {
        "🖧"
    }

    fn icon_color(&self, st: &Shared) -> Color32 {
        match &st.net.adapters {
            Some(Ok(list)) if list.iter().any(|a| a.primary) => good(),
            Some(Ok(_)) => bad(),
            _ => pal().weak,
        }
    }

    fn about(&self) -> &'static str {
        "Ethernet and Wi-Fi: each adapter's settings, and joining Wi-Fi networks"
    }

    fn card(&self, ui: &mut egui::Ui, st: &Shared) {
        match &st.net.adapters {
            Some(Ok(list)) => match list.iter().find(|a| a.primary) {
                Some(a) => {
                    super::dot_line(ui, good(), format!("{} · {}", a.kind.word(), detail(a)));
                    let standby = list.iter().filter(|o| !o.primary && o.connected()).count();
                    if standby > 0 {
                        super::pill(ui, &format!("{standby} more connected, on standby"), pal().accent);
                    }
                }
                None => super::dot_line(ui, bad(), format!("No network · {}", why_none(list))),
            },
            Some(Err(e)) => {
                ui.label(egui::RichText::new(format!("Can't read the adapters: {e}")).color(pal().weak));
            }
            None => {
                ui.label(egui::RichText::new("Looking…").color(pal().weak));
            }
        }
    }

    fn footer(&self, st: &Shared) -> String {
        match &st.net.adapters {
            Some(Ok(list)) => {
                let n = if list.len() == 1 { "1 adapter".into() } else { format!("{} adapters", list.len()) };
                format!("{n} · settings and Wi-Fi networks")
            }
            _ => self.about().into(),
        }
    }

    fn max_width(&self) -> f32 {
        860.0
    }

    fn show(&mut self, ui: &mut egui::Ui, v: &mut View) {
        let st = v.st;
        trial_banner(ui, st, v.asks);
        if let Some(doing) = &st.net.working {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(doing);
            });
        }
        if let Some((text, problem)) = &st.net.note {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(text).color(if *problem { warn() } else { good() }));
                if ui.small_button("✕").clicked() {
                    v.asks.push(Ask::Net(Do::Dismiss));
                }
            });
        }
        let list = match &st.net.adapters {
            Some(Ok(list)) if !list.is_empty() => list,
            other => {
                let text = match other {
                    None => "Looking at this computer's network adapters…".to_string(),
                    Some(Err(e)) => format!("Couldn't read the network adapters: {e}."),
                    Some(Ok(_)) => "This computer has no Ethernet or Wi-Fi adapters NetworkManager can use.".into(),
                };
                look::card(ui, |ui| ui.label(egui::RichText::new(text).color(pal().weak)));
                return;
            }
        };
        let dev = self.chosen.clone().filter(|d| list.iter().any(|a| &a.dev == d)).unwrap_or_else(|| list[0].dev.clone());
        self.chosen = Some(dev.clone());
        let busy = st.net.trial.as_ref().is_some_and(Trial::running) || st.net.working.is_some();

        look::card(ui, |ui| {
            ui.label(look::caption("Interface"));
            let cols = if ui.available_width() >= 560.0 { list.len().min(3) } else { 1 };
            for row in list.chunks(cols) {
                ui.columns(cols, |c| {
                    for (a, ui) in row.iter().zip(c) {
                        if adapter_box(ui, a, a.dev == dev).clicked() && a.dev != dev {
                            self.chosen = Some(a.dev.clone());
                            self.picking = false;
                            self.open = None;
                        }
                    }
                });
            }
        });
        let Some(a) = list.iter().find(|a| a.dev == self.chosen.clone().unwrap_or_default()) else { return };
        self.settings(ui, st, a, busy, v.asks);
        if a.kind == Kind::Wifi && !a.unavailable() && (self.picking || !a.connected()) {
            self.networks(ui, st, a, busy, v.asks);
        }
    }
}

impl Network {
    fn settings(&mut self, ui: &mut egui::Ui, st: &Shared, a: &Adapter, busy: bool, asks: &mut Vec<Ask>) {
        let weak = |t: &str| egui::RichText::new(t).small().color(pal().weak);
        // Start again from what's saved when another adapter is picked, or
        // when the saved settings change and nothing's been edited.
        match (&self.edit, &a.profile) {
            (_, None) => self.edit = None,
            (Some((was, now)), Some(p)) if was.dev == a.dev && (was.base == *p || was.profile() != now.profile()) => {}
            (_, Some(p)) => {
                let e = Edit::new(a, p);
                self.edit = Some((e.clone(), e));
            }
        }
        look::card(ui, |ui| {
            ui.label(look::caption(&format!("{} · {}", a.dev, a.kind.word())));
            row(ui, "Use this adapter", &power_note(a, st.prefs.undo_secs), &mut |ui| {
                if a.unavailable() {
                    return;
                }
                let mut on = a.connected() || a.state.starts_with("connecting");
                ui.add_enabled_ui(!busy, |ui| {
                    if look::toggle(ui, &mut on).changed() {
                        asks.push(Ask::Net(if on { Do::Connect(a.dev.clone()) } else { Do::Disconnect(a.dev.clone()) }));
                    }
                });
            });
            if a.kind == Kind::Wifi && !a.unavailable() {
                let now = match &a.wifi {
                    Some(w) => format!("{} · {} · {} · signal {} %", w.ssid, security_word(w), w.band(), w.signal),
                    None => "Not connected".into(),
                };
                let picking = self.picking || !a.connected();
                row(ui, "Network", &now, &mut |ui| {
                    if a.connected() && look::button(ui, if picking { "Hide networks" } else { "Choose network" }, false, true).clicked() {
                        self.picking = !self.picking;
                    }
                });
            }
            let Some((was, edit)) = &mut self.edit else {
                let text = match a.kind {
                    Kind::Wifi => "Join a network below; its settings show here after.",
                    Kind::Ethernet => "No wired settings saved for this adapter yet. Plugging in a cable makes them.",
                };
                ui.label(weak(text));
                return;
            };
            ui.add_enabled_ui(!busy, |ui| {
                let mut i = edit.manual as usize;
                row(ui, "Address", "Automatic asks your router for one", &mut |ui| {
                    if look::tabs(ui, &format!("addr-{}", a.dev), &mut i, &["Automatic", "Manual"]) {
                        edit.manual = i == 1;
                    }
                });
                if edit.manual {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        for (label, text, width) in [("Address", &mut edit.address, 130.0), ("Mask", &mut edit.mask, 120.0), ("Router", &mut edit.gateway, 120.0)] {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(weak(label));
                                ui.add(egui::TextEdit::singleline(text).desired_width(width).font(egui::TextStyle::Monospace));
                            });
                        }
                    });
                    ui.add_space(4.0);
                } else {
                    let mono = |t: String| egui::RichText::new(t).monospace().color(pal().weak);
                    row(ui, "This computer's address", "", &mut |ui| {
                        ui.label(mono(a.address.clone().unwrap_or("—".into())));
                    });
                    row(ui, "Router", "", &mut |ui| {
                        ui.label(mono(a.gateway.clone().unwrap_or("—".into())));
                    });
                }
                let now = if a.dns.is_empty() { "Who turns names like google.com into addresses".to_string() } else { format!("Now: {}", a.dns.join(", ")) };
                let labels: Vec<&str> = DNS.iter().map(|d| d.0).collect();
                row(ui, "DNS (name lookup)", &now, &mut |ui| {
                    look::tabs(ui, &format!("dns-{}", a.dev), &mut edit.dns, &labels);
                });
                if edit.dns == 4 {
                    ui.horizontal(|ui| {
                        ui.label(weak("Addresses"));
                        ui.add(egui::TextEdit::singleline(&mut edit.custom).hint_text("1.1.1.1, 9.9.9.9").desired_width(260.0).font(egui::TextStyle::Monospace));
                    });
                    ui.add_space(4.0);
                }
                let auto_note = match (&a.kind, &a.wifi) {
                    (Kind::Wifi, Some(w)) => format!("Joins {} whenever it's in range", w.ssid),
                    (Kind::Wifi, None) => "Joins this network whenever it's in range".into(),
                    (Kind::Ethernet, _) => "Connects when a cable is plugged in".into(),
                };
                row(ui, "Connect on its own", &auto_note, &mut |ui| {
                    look::toggle(ui, &mut edit.auto);
                });
                row(ui, "Prefer this connection", "When cable and Wi-Fi are both connected, the internet goes this way", &mut |ui| {
                    look::toggle(ui, &mut edit.prefer);
                });
                if let Some(mtu) = a.mtu {
                    row(ui, "Packet size (MTU)", "Fine as it is for nearly every network", &mut |ui| {
                        ui.label(egui::RichText::new(mtu.to_string()).monospace().color(pal().weak));
                    });
                }
            });
            // The safety net.
            egui::Frame::new().fill(pal().bg).corner_radius(10).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut i = UNDO.iter().position(|u| u.0 == st.prefs.undo_secs).unwrap_or(1);
                let labels: Vec<&str> = UNDO.iter().map(|u| u.1).collect();
                row(
                    ui,
                    "Undo if the internet doesn't come back",
                    "After a change to the adapter in use, Net Watch checks the internet. No answer in time, and the old settings come back on their own.",
                    &mut |ui| {
                        if look::tabs(ui, "undo-secs", &mut i, &labels) {
                            let mut p = st.prefs.clone();
                            p.undo_secs = UNDO[i].0;
                            asks.push(Ask::SetPrefs(p));
                        }
                    },
                );
            });
            let made = edit.profile();
            let changed = made != was.profile();
            ui.horizontal(|ui| {
                let (text, color) = match &made {
                    Err(e) if changed => (e.clone(), warn()),
                    _ if changed => ("Changes not applied yet".into(), pal().accent),
                    _ => ("No changes".into(), pal().weak),
                };
                ui.label(egui::RichText::new(text).small().color(color));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let apply = look::button(ui, "Apply", true, changed && made.is_ok() && !busy);
                    if apply.clicked()
                        && let Ok(new) = &made
                    {
                        asks.push(Ask::Net(Do::Apply { dev: a.dev.clone(), old: was.base.clone(), new: new.clone(), active: a.active, primary: a.primary }));
                        // Shown as saved while it's tried; undone, the refresh brings the old back.
                        edit.base = new.clone();
                        *was = edit.clone();
                    }
                    if look::button(ui, "Reset", false, changed).clicked() {
                        *edit = was.clone();
                    }
                });
            });
        });
    }

    fn networks(&mut self, ui: &mut egui::Ui, st: &Shared, a: &Adapter, busy: bool, asks: &mut Vec<Ask>) {
        let weak = |t: &str| egui::RichText::new(t).small().color(pal().weak);
        if self.scanned_for.as_deref() != Some(a.dev.as_str()) {
            self.scanned_for = Some(a.dev.clone());
            asks.push(Ask::Net(Do::Scan(a.dev.clone())));
        }
        let scan = st.net.wifi.as_ref().filter(|w| w.0 == a.dev);
        look::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(look::caption("Choose a network"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if look::button(ui, "Scan again", false, !st.net.scanning).clicked() {
                        asks.push(Ask::Net(Do::Scan(a.dev.clone())));
                    }
                    if st.net.scanning {
                        ui.spinner();
                    } else if let Some((_, _, at)) = scan {
                        ui.label(weak(&format!("Scanned {} ago", crate::span((chrono::Local::now() - *at).num_seconds()))));
                    }
                });
            });
            let nets = match scan.map(|s| &s.1) {
                Some(Ok(n)) => n,
                Some(Err(e)) => {
                    ui.label(egui::RichText::new(format!("Couldn't look for networks: {e}")).color(warn()));
                    return;
                }
                None => {
                    look::changed_at(ui.ctx(), egui::Id::new("wifi list"), 0);
                    ui.label(weak("Looking for networks…"));
                    return;
                }
            };
            if nets.is_empty() {
                ui.label(weak("No networks in range."));
            }
            ui.spacing_mut().item_spacing.y = 2.0;
            // Each new list drops in a row at a time.
            let key = scan.map_or(0, |s| s.2.timestamp_millis() as u64);
            let came = look::changed_at(ui.ctx(), egui::Id::new("wifi list"), key);
            for (i, n) in nets.iter().enumerate() {
                let open = self.open.as_deref() == Some(n.ssid.as_str());
                let t = look::progress(ui.ctx(), came + 0.035 * i as f64, 0.3);
                let drop = egui::emath::TSTransform::from_translation(vec2(0.0, -8.0 * (1.0 - t)));
                let row = ui.scope(|ui| {
                    ui.multiply_opacity(t);
                    ui.with_visual_transform(drop, |ui| network_row(ui, n, open)).inner
                });
                if row.inner.clicked() {
                    self.open = if open { None } else { Some(n.ssid.clone()) };
                    self.password.clear();
                    self.show_password = false;
                    self.auto_join = true;
                }
                if open {
                    ui.add_enabled_ui(!busy, |ui| self.join(ui, a, n, asks));
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                if look::chip(ui, "+  Hidden network…", true, true).on_hover_text("A network that doesn't show its name").clicked() {
                    self.open = if self.open.as_deref() == Some("") { None } else { Some(String::new()) };
                    self.password.clear();
                    self.hidden_name.clear();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let saved = nets.iter().filter(|n| n.saved.is_some()).count();
                    let count = if nets.len() == 1 { "1 network".into() } else { format!("{} networks", nets.len()) };
                    ui.label(weak(&format!("{count} · strongest first · {saved} saved")));
                });
            });
            if self.open.as_deref() == Some("") {
                ui.add_enabled_ui(!busy, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(weak("Name"));
                        ui.add(egui::TextEdit::singleline(&mut self.hidden_name).desired_width(180.0));
                    });
                    let hidden = WifiNet { ssid: self.hidden_name.clone(), security: "WPA2".into(), freq: 0, signal: 0, in_use: false, saved: None };
                    self.join(ui, a, &hidden, asks);
                });
            }
        });
    }

    /// What opening a network shows: connect, password, forget.
    fn join(&mut self, ui: &mut egui::Ui, a: &Adapter, n: &WifiNet, asks: &mut Vec<Ask>) {
        let weak = |t: &str| egui::RichText::new(t).small().color(pal().weak);
        let hidden = n.signal == 0 && n.freq == 0;
        ui.indent(("join", &n.ssid), |ui| {
            let join = |password: String, auto: bool| {
                Ask::Net(Do::Join { dev: a.dev.clone(), ssid: n.ssid.clone(), security: n.security.clone(), password, auto, hidden, saved: n.saved.clone() })
            };
            if n.in_use {
                ui.horizontal(|ui| {
                    if look::button(ui, "Disconnect", false, true).clicked() {
                        asks.push(Ask::Net(Do::Disconnect(a.dev.clone())));
                    }
                    look::button(ui, "Forget", false, false).on_disabled_hover_text("Disconnect first");
                    let auto = a.profile.as_ref().is_some_and(|p| p.autoconnect);
                    ui.label(weak(if auto { "Saved · joins on its own" } else { "Saved" }));
                });
            } else if let Some(uuid) = &n.saved {
                ui.horizontal(|ui| {
                    if look::button(ui, "Connect", true, true).clicked() {
                        asks.push(join(String::new(), true));
                    }
                    if look::button(ui, "Forget", false, true).on_hover_text("Delete its saved password and settings").clicked() {
                        asks.push(Ask::Net(Do::Forget(uuid.clone(), n.ssid.clone())));
                        self.open = None;
                    }
                    ui.label(weak("Saved: no password needed"));
                });
            } else if n.enterprise() {
                ui.label(weak("Needs a user name and certificates, as at work or school. Use the desktop's own Wi-Fi settings for this one."));
            } else if n.open() {
                ui.horizontal(|ui| {
                    if look::button(ui, "Connect", true, true).clicked() {
                        asks.push(join(String::new(), self.auto_join));
                    }
                    tag(ui, "No password: others nearby can see what's sent", warn());
                });
                ui.checkbox(&mut self.auto_join, "Join on its own next time");
            } else {
                let long_enough = self.password.chars().count() >= 8;
                let named = !hidden || !self.hidden_name.trim().is_empty();
                let mut go = false;
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut self.password).password(!self.show_password).hint_text("Network password").desired_width(220.0));
                    go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if look::button(ui, if self.show_password { "Hide" } else { "Show" }, false, true).clicked() {
                        self.show_password = !self.show_password;
                    }
                    go |= look::button(ui, "Connect", true, long_enough && named).clicked();
                });
                if !long_enough && !self.password.is_empty() {
                    ui.label(weak("Wi-Fi passwords are at least 8 characters."));
                }
                ui.checkbox(&mut self.auto_join, "Join on its own next time");
                if go && long_enough && named {
                    asks.push(join(std::mem::take(&mut self.password), self.auto_join));
                    self.open = None;
                }
            }
        });
        ui.add_space(4.0);
    }
}

/// A setting's name and a line on it on the left, its control on the
/// right (or below, when narrow).
fn row(ui: &mut egui::Ui, title: &str, sub: &str, add: &mut dyn FnMut(&mut egui::Ui)) {
    let weak = |t: &str| egui::RichText::new(t).small().color(pal().weak);
    let text = |ui: &mut egui::Ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(title);
            if !sub.is_empty() {
                ui.add(egui::Label::new(weak(sub)).wrap());
            }
        });
    };
    if ui.available_width() < 600.0 {
        text(ui);
        ui.horizontal_wrapped(|ui| add(ui));
    } else {
        ui.horizontal(|ui| {
            let right = 330.0;
            ui.allocate_ui_with_layout(vec2(ui.available_width() - right, 0.0), egui::Layout::top_down(egui::Align::Min), text);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| add(ui));
        });
    }
    ui.add_space(4.0);
}

fn power_note(a: &Adapter, undo: u64) -> String {
    match (a.kind, a.unavailable()) {
        (Kind::Ethernet, true) => "No cable plugged in".into(),
        (Kind::Wifi, true) => "Wi-Fi is switched off on this computer".into(),
        _ if a.primary => format!("The internet goes through this one: turning it off is undone after {} unless another takes over", crate::span(undo as i64)),
        _ => "Turning it off disconnects it".into(),
    }
}

/// A box for an adapter at the top; the chosen one in the accent.
fn adapter_box(ui: &mut egui::Ui, a: &Adapter, chosen: bool) -> egui::Response {
    let id = ui.id().with(("adapter", &a.dev));
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let edge = if chosen { pal().accent } else if hovered { pal().edge.lerp_to_gamma(pal().accent, 0.5) } else { pal().edge };
    let rect = egui::Frame::new()
        .fill(if chosen { pal().accent.gamma_multiply(0.08) } else { pal().bg })
        .stroke(Stroke::new(if chosen { 2.0 } else { 1.0 }, edge))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                let color = if a.primary { good() } else if a.connected() { pal().accent } else { pal().weak };
                icon(ui, Some(a.kind), color, 16.0);
                ui.label(egui::RichText::new(a.kind.word()).strong());
                if a.primary {
                    tag(ui, "in use", good());
                }
            });
            let state = match (a.kind, a.state.as_str()) {
                _ if a.primary => detail_tail(a),
                (_, "connected") => "connected, on standby".into(),
                (Kind::Ethernet, "unavailable") => "no cable".into(),
                (Kind::Wifi, "unavailable") => "switched off".into(),
                (_, s) if s.starts_with("connecting") => "connecting…".into(),
                _ => "not connected".into(),
            };
            let line = [a.dev.as_str(), a.model.as_str(), state.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
            ui.add(egui::Label::new(egui::RichText::new(&line).small().color(pal().weak)).truncate());
            line
        });
    let line = rect.inner;
    ui.interact(rect.response.rect, id, egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(line)
}

/// A network in the list: signal bars, its name and tags, and its
/// security, band and signal on the right.
fn network_row(ui: &mut egui::Ui, n: &WifiNet, open: bool) -> egui::Response {
    let id = ui.id().with(("net", &n.ssid));
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let rect = egui::Frame::new()
        .fill(if open || hovered { pal().bg } else { Color32::TRANSPARENT })
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(8, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                bars(ui, n.signal);
                let name = egui::RichText::new(&n.ssid);
                ui.label(if n.in_use { name.strong() } else if n.signal < 25 { name.color(pal().weak) } else { name });
                if n.in_use {
                    tag(ui, "connected", good());
                } else if n.saved.is_some() {
                    tag(ui, "saved", pal().weak);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(format!("{} · {} · {} %", security_word(n), n.band(), n.signal)).small().color(pal().weak));
                });
            });
        })
        .response
        .rect;
    ui.interact(rect, id, egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn security_word(n: &WifiNet) -> String {
    if n.open() {
        "Open".into()
    } else if n.enterprise() {
        "Enterprise".into()
    } else {
        // "WPA1 WPA2" is just "WPA2" to anyone choosing.
        n.security.split_whitespace().last().unwrap_or("").to_string()
    }
}

/// Four bars, filled for the signal.
fn bars(ui: &mut egui::Ui, signal: u8) {
    let (rect, _) = ui.allocate_exact_size(vec2(18.0, 14.0), egui::Sense::hover());
    let filled = (signal as usize + 24) / 25;
    for i in 0..4 {
        let h = 4.0 + 3.3 * i as f32;
        let x = rect.left() + 1.0 + i as f32 * 4.3;
        let bar = egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - h), egui::pos2(x + 3.0, rect.bottom()));
        ui.painter().rect_filled(bar, 1.0, if i < filled { pal().text } else { pal().key_hover });
    }
}

/// Ethernet as a socket, Wi-Fi as waves, nothing as a crossed circle.
pub fn icon(ui: &mut egui::Ui, kind: Option<Kind>, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
    let p = ui.painter();
    let s = size / 16.0;
    let c = rect.center();
    let stroke = Stroke::new(1.6 * s, color);
    match kind {
        Some(Kind::Wifi) => {
            let base = c + vec2(0.0, 5.0 * s);
            p.circle_filled(base, 1.6 * s, color);
            for r in [4.5, 8.0, 11.5] {
                let pts: Vec<egui::Pos2> = (0..=12)
                    .map(|i| {
                        let ang = std::f32::consts::PI * (1.25 + 0.5 * i as f32 / 12.0);
                        base + vec2(ang.cos(), ang.sin()) * r * s
                    })
                    .collect();
                p.add(egui::Shape::line(pts, stroke));
            }
        }
        Some(Kind::Ethernet) => {
            let body = egui::Rect::from_center_size(c + vec2(0.0, 1.0 * s), vec2(13.0 * s, 10.0 * s));
            p.rect_stroke(body, 2.0 * s, stroke, egui::StrokeKind::Middle);
            let tab = egui::Rect::from_center_size(c + vec2(0.0, -5.0 * s), vec2(6.0 * s, 3.0 * s));
            p.rect_filled(tab, 1.0 * s, color);
            for i in 0..3 {
                let x = c.x + (i as f32 - 1.0) * 3.0 * s;
                p.line_segment([egui::pos2(x, c.y + 0.5 * s), egui::pos2(x, c.y + 3.5 * s)], stroke);
            }
        }
        None => {
            p.circle_stroke(c, 6.5 * s, stroke);
            p.line_segment([c + vec2(-4.5, 4.5) * s, c + vec2(4.5, -4.5) * s], stroke);
        }
    }
}

/// "1 Gbps" for 1000, "2.5 Gbps", "100 Mbps".
fn speed_word(mbps: u32) -> String {
    match mbps {
        m if m >= 1000 && m % 1000 == 0 => format!("{} Gbps", m / 1000),
        m if m >= 1000 => format!("{:.1} Gbps", m as f64 / 1000.0),
        m => format!("{m} Mbps"),
    }
}

/// The adapter's name and what's useful after it: link speed, or the
/// Wi-Fi network and signal.
fn detail(a: &Adapter) -> String {
    format!("{} · {}", a.dev, detail_tail(a)).trim_end_matches(" · ").to_string()
}

fn detail_tail(a: &Adapter) -> String {
    match (a.kind, &a.wifi, a.speed) {
        (Kind::Wifi, Some(w), _) => format!("{} · {} %", w.ssid, w.signal),
        (Kind::Ethernet, _, Some(s)) => speed_word(s),
        _ => String::new(),
    }
}

/// Why nothing's connected, in a few words.
fn why_none(list: &[Adapter]) -> String {
    let cable = list.iter().any(|a| a.kind == Kind::Ethernet);
    let unplugged = list.iter().filter(|a| a.kind == Kind::Ethernet).all(Adapter::unavailable);
    let wifi = list.iter().find(|a| a.kind == Kind::Wifi);
    let mut parts = Vec::new();
    if cable {
        parts.push(if unplugged { "cable unplugged" } else { "cable not connected" });
    }
    if let Some(w) = wifi {
        parts.push(if w.unavailable() { "Wi-Fi off" } else { "Wi-Fi not connected" });
    }
    parts.join(", ")
}

/// Home's line for the adapter in use: its icon, kind, name and detail,
/// and a gear for this screen. Whether the gear was clicked.
pub fn chip(ui: &mut egui::Ui, st: &Shared) -> bool {
    let Some(Ok(list)) = &st.net.adapters else { return false };
    if list.is_empty() {
        return false;
    }
    let primary = list.iter().find(|a| a.primary);
    let standby = list.iter().find(|a| !a.primary && a.connected());
    let trying = st.net.trial.as_ref().filter(|t| t.running());
    let resp = egui::Frame::new()
        .fill(pal().body)
        .stroke(Stroke::new(1.0, pal().edge))
        .corner_radius(14)
        .inner_margin(egui::Margin { left: 8, right: 12, top: 3, bottom: 3 })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.horizontal(|ui| {
                let weak = |t: String| egui::RichText::new(t).size(12.5).color(pal().weak);
                match primary {
                    Some(a) => {
                        icon(ui, Some(a.kind), good(), 15.0);
                        ui.label(egui::RichText::new(a.kind.word()).size(12.5).strong());
                        ui.label(weak(detail(a)));
                    }
                    None => {
                        icon(ui, None, bad(), 15.0);
                        ui.label(egui::RichText::new("No network").size(12.5).strong().color(bad()));
                        ui.label(weak(why_none(list)));
                    }
                }
                match (trying, standby) {
                    (Some(t), _) => {
                        let left = t.secs as i64 - (chrono::Local::now() - t.since).num_seconds();
                        let text = if matches!(t.stage, Stage::Applying) { "· applying…".to_string() } else { format!("· trying new settings, {}s", left.max(0)) };
                        ui.label(egui::RichText::new(text).size(12.5).color(warn()));
                    }
                    (None, Some(s)) if primary.is_some() => {
                        ui.label(egui::RichText::new(format!("+ {} standby", s.kind.word())).size(12.0).color(pal().weak.gamma_multiply(0.8)));
                    }
                    _ => {}
                }
            });
        })
        .response;
    if let Some(a) = primary {
        resp.on_hover_text(tooltip(a));
    }
    look::button(ui, "⚙", false, true).on_hover_text("Connection settings").clicked()
}

fn tooltip(a: &Adapter) -> String {
    let mut lines = vec![
        format!("{} ({})", a.kind.word(), if a.kind == Kind::Wifi { "wireless" } else { "cable" }),
        format!("Adapter: {}{}", a.dev, if a.model.is_empty() { String::new() } else { format!(", {}", a.model) }),
    ];
    if let Some(w) = &a.wifi {
        lines.push(format!("Network: {} · {} · signal {} %", w.ssid, w.band(), w.signal));
    }
    if let Some(s) = a.speed {
        lines.push(format!("Link speed: {}", speed_word(s)));
    }
    if let Some(addr) = &a.address {
        lines.push(format!("Address: {}", addr.split('/').next().unwrap_or(addr)));
    }
    lines.join("\n")
}

/// While a change is tried: a countdown with Keep and Undo now; after,
/// how it went.
fn trial_banner(ui: &mut egui::Ui, st: &Shared, asks: &mut Vec<Ask>) {
    let Some(t) = &st.net.trial else {
        // Seen without one, so the next one slides in.
        look::changed_at(ui.ctx(), egui::Id::new("trial banner"), u64::MAX);
        return;
    };
    let (color, title, text) = match &t.stage {
        Stage::Applying => (warn(), format!("Applying {}…", t.what), String::new()),
        Stage::Waiting => {
            let left = (t.secs as i64 - (chrono::Local::now() - t.since).num_seconds()).max(0);
            (warn(), format!("Trying {}", t.what), format!("Checking the internet… the old settings come back in {left} s unless you keep these."))
        }
        Stage::Kept(m) => (good(), "New settings kept".to_string(), m.clone()),
        Stage::Undone(m) => (bad(), "Put the old settings back".to_string(), m.clone()),
        Stage::Failed(m) => (bad(), "That didn't work".to_string(), m.clone()),
    };
    // Slides down into view, and again for how it went.
    let stage = match t.stage {
        Stage::Applying => 0,
        Stage::Waiting => 1,
        Stage::Kept(_) => 2,
        Stage::Undone(_) => 3,
        Stage::Failed(_) => 4,
    };
    let came = look::changed_at(ui.ctx(), egui::Id::new("trial banner"), stage);
    let k = look::progress(ui.ctx(), came, 0.35);
    let drop = egui::emath::TSTransform::from_translation(vec2(0.0, -10.0 * (1.0 - k)));
    ui.scope(|ui| {
        ui.multiply_opacity(k);
        ui.with_visual_transform(drop, |ui| egui::Frame::new()
            .fill(color.gamma_multiply(if pal().light { 0.10 } else { 0.16 }))
            .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.horizontal(|ui| {
                            if t.running() {
                                ui.spinner();
                            }
                            ui.label(egui::RichText::new(&title).strong().color(color));
                        });
                        if !text.is_empty() {
                            ui.add(egui::Label::new(egui::RichText::new(&text).small()).wrap());
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match t.stage {
                        Stage::Waiting => {
                            if look::button(ui, "Undo now", false, t.decision.is_none()).clicked() {
                                asks.push(Ask::Net(Do::Undo));
                            }
                            if look::button(ui, "Keep", true, t.decision.is_none()).clicked() {
                                asks.push(Ask::Net(Do::Keep));
                            }
                        }
                        Stage::Applying => {}
                        _ => {
                            if look::button(ui, "OK", false, true).clicked() {
                                asks.push(Ask::Net(Do::Dismiss));
                            }
                        }
                    });
                });
                if matches!(t.stage, Stage::Waiting) {
                    let k = 1.0 - ((chrono::Local::now() - t.since).num_milliseconds() as f32 / (t.secs as f32 * 1000.0)).clamp(0.0, 1.0);
                    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 5.0), egui::Sense::hover());
                    ui.painter().rect_filled(bar, 3.0, pal().key_hover);
                    ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, vec2(bar.width() * k, bar.height())), 3.0, color);
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                }
            }));
    });
}
