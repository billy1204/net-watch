//! What's on the home network, when asked or every so often.
use super::{Ask, Tool, View};
use crate::look::{self, good, warn, pal};
use crate::monitor::Shared;
use crate::{MEDIUM, tag, when};
use eframe::egui::{self, Color32, vec2};

pub struct Devices;

impl Tool for Devices {
    fn name(&self) -> &'static str {
        "Devices"
    }

    fn icon(&self) -> &'static str {
        "📱"
    }

    fn about(&self) -> &'static str {
        "What's connected to your home network, and a pop-up when something new joins"
    }

    fn badge(&self, st: &Shared) -> Option<(String, Color32)> {
        let Some(Ok(s)) = &st.devices else { return None };
        let new = s.devices.iter().filter(|d| d.new).count();
        (new > 0).then(|| (format!("{new} new"), pal().accent))
    }

    fn card(&self, ui: &mut egui::Ui, st: &Shared) {
        match &st.devices {
            _ if st.scanning => super::dot_line(ui, pal().accent, "Scanning…"),
            Some(Ok(s)) => {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    ui.label(egui::RichText::new(s.devices.len().to_string()).strong());
                    ui.label("on your network");
                });
            }
            Some(Err(_)) => super::dot_line(ui, warn(), "The last scan didn't work"),
            None => {
                ui.label(egui::RichText::new("Not scanned yet").color(pal().weak));
            }
        }
        if st.prefs.watch_devices && st.away_from_home {
            super::pill(ui, "Watching paused · not your home network", warn());
        }
    }

    fn glance_width(&self) -> f32 {
        104.0
    }

    /// Overlapping circles for the kinds of device found, then how many more.
    fn glance(&self, ui: &mut egui::Ui, st: &Shared) {
        let Some(Ok(s)) = &st.devices else { return };
        let mut kinds: Vec<&str> = Vec::new();
        for d in &s.devices {
            let k = kind(d);
            if !kinds.contains(&k) {
                kinds.push(k);
            }
        }
        let shown = kinds.len().min(3);
        let more = s.devices.len().saturating_sub(shown);
        let (rect, _) = ui.allocate_exact_size(vec2(104.0, 32.0), egui::Sense::hover());
        let p = ui.painter();
        // Each slides in from the left as the card comes in.
        let grow = look::appear(ui.ctx());
        let center = |i: usize| egui::pos2(rect.left() + 16.0 + i as f32 * 22.0 * grow, rect.center().y);
        let circle = |i: usize, fill: Color32, text: &str, color: Color32, size: f32| {
            let c = center(i);
            p.circle_filled(c, 16.0, pal().body);
            p.circle_filled(c, 14.0, fill);
            p.text(c, egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(size), color);
        };
        for (i, k) in kinds.iter().take(3).enumerate() {
            circle(i, pal().bg, k, pal().text, 14.0);
        }
        if more > 0 {
            circle(shown, pal().accent, &format!("+{more}"), Color32::WHITE, 11.5);
        }
        // Which devices each circle stands for.
        for (i, k) in kinds.iter().take(3).enumerate() {
            if super::pointing_at(ui, egui::Rect::from_center_size(center(i), vec2(22.0, 30.0))) {
                let names: Vec<String> = s.devices.iter().filter(|d| kind(d) == *k).map(|d| d.name.clone().unwrap_or_else(|| d.ip.to_string())).collect();
                look::set_tip(ui.ctx(), names.join(", "));
            }
        }
        if more > 0 && super::pointing_at(ui, egui::Rect::from_center_size(center(shown), vec2(30.0, 30.0))) {
            look::set_tip(ui.ctx(), format!("{} devices in all; open Devices for the list", s.devices.len()));
        }
    }

    fn footer(&self, st: &Shared) -> String {
        let scanned = match &st.devices {
            Some(Ok(s)) => Some(format!("scanned {}", super::when_mid(s.at))),
            _ => None,
        };
        match (st.prefs.watch_devices, scanned) {
            (true, Some(s)) => format!("Watching every {} · {s}", every(st.prefs.device_mins)),
            (true, None) => format!("Watching every {}", every(st.prefs.device_mins)),
            (false, Some(s)) => format!("S{}", &s[1..]),
            (false, None) => self.about().into(),
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, v: &mut View) {
        let st = v.st;
        let narrow = ui.available_width() < MEDIUM;
        let weak = |t: &str| egui::RichText::new(t).color(pal().weak);
        look::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(look::caption("Devices on your network"));
                if st.prefs.watch_devices {
                    if st.away_from_home {
                        tag(ui, "Watching paused · not your home network", warn());
                    } else {
                        tag(ui, &format!("Watching · every {}", every(st.prefs.device_mins)), good());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if st.devices.is_some() { "Scan again" } else { "Scan network" };
                    if look::button(ui, label, false, !st.scanning).clicked() {
                        v.asks.push(Ask::ScanDevices);
                    }
                    if st.scanning {
                        ui.spinner();
                    } else if let Some(Ok(s)) = &st.devices {
                        let n = if s.devices.len() == 1 { "1 device".into() } else { format!("{} devices", s.devices.len()) };
                        ui.label(egui::RichText::new(format!("{n} · {} · scanned {}", s.range, when(s.at))).small().color(pal().weak));
                    }
                });
            });
            let scan = match &st.devices {
                Some(Ok(s)) => s,
                Some(Err(e)) if !st.scanning => {
                    ui.label(egui::RichText::new(format!("Couldn't scan: {e}")).color(warn()));
                    return;
                }
                _ => {
                    let text = if st.scanning {
                        "Scanning… about 5 seconds."
                    } else {
                        "Finds what's connected to your home network (phones, TVs, speakers, printers and so on) \
                         with names, makers and a guess at what each one is. Stays on your network; takes about 5 seconds."
                    };
                    ui.add(egui::Label::new(weak(text)).wrap());
                    return;
                }
            };
            let new = scan.devices.iter().filter(|d| d.new).count();
            let note = match (scan.first, new) {
                (true, _) => "First scan: next time, anything that wasn't here is marked New.".to_string(),
                (false, 0) => "Nothing new since the last scan.".to_string(),
                (false, 1) => "1 device wasn't here before.".to_string(),
                (false, n) => format!("{n} devices weren't here before."),
            };
            ui.add(egui::Label::new(weak(&format!(
                "{note} Devices that are asleep or hide may not show. \"Private address\": phones and laptops make one up for privacy, so the maker isn't known."
            ))).wrap());
            egui::Grid::new("devices").striped(true).num_columns(if narrow { 2 } else { 4 }).spacing(vec2(18.0, 8.0)).show(ui, |ui| {
                let head = |ui: &mut egui::Ui, t: &str| ui.label(egui::RichText::new(t).small().color(pal().weak));
                head(ui, "Device");
                head(ui, "Address");
                if !narrow {
                    head(ui, "Maker");
                    head(ui, "Might be");
                }
                ui.end_row();
                for d in &scan.devices {
                    ui.horizontal(|ui| {
                        // A picture only for what it surely is; an empty
                        // space otherwise, so the names line up.
                        match sure(d) {
                            Some((sort, why)) => {
                                let color = if sort == Sort::Router { good() } else { pal().accent };
                                device_icon(ui, sort, color, 18.0).on_hover_text(why);
                            }
                            None => {
                                ui.add_space(18.0);
                            }
                        }
                        let name = d.name.clone().unwrap_or_else(|| if narrow { d.maker_text() } else { "Unnamed".into() });
                        let r = ui.label(if d.name.is_some() { egui::RichText::new(name) } else { weak(&name) });
                        let ports: Vec<String> = d.ports.iter().map(u16::to_string).collect();
                        r.on_hover_text(format!(
                            "Hardware address: {}\nAnswers on ports: {}",
                            d.mac.as_deref().unwrap_or("unknown"),
                            if ports.is_empty() { "none of those tried".into() } else { ports.join(", ") }
                        ));
                        if d.me {
                            tag(ui, "This computer", pal().accent);
                        }
                        if d.router {
                            tag(ui, "Router", good());
                        }
                        if d.new {
                            tag(ui, "New", warn());
                        }
                    });
                    ui.label(weak(&d.ip.to_string()));
                    if !narrow {
                        ui.label(weak(&d.maker_text()));
                        ui.label(weak(&d.guess()));
                    }
                    ui.end_row();
                }
            });
        });
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Sort {
    Router,
    Computer,
    Tv,
    Phone,
    Printer,
    Camera,
    Speaker,
}

/// What a device surely is, and how we know: from the router's own table,
/// a port only that kind of device answers on, or the name it gives itself.
/// None when it's only a guess (AirPlay, say, is TVs, Macs and speakers).
fn sure(d: &crate::devices::Device) -> Option<(Sort, &'static str)> {
    let name = d.name.as_deref().unwrap_or("").to_lowercase();
    let on = |port: u16| d.ports.contains(&port);
    let named = |words: &[&str]| words.iter().any(|w| name.contains(w));
    Some(if d.router {
        (Sort::Router, "Your router")
    } else if d.me {
        (Sort::Computer, "This computer")
    } else if on(8060) {
        (Sort::Tv, "A Roku: it answers on Roku's own port")
    } else if on(1400) {
        (Sort::Speaker, "A Sonos speaker: it answers on Sonos's own port")
    } else if on(62078) {
        (Sort::Phone, "An iPhone or iPad: it answers on Apple's phone-sync port")
    } else if named(&["iphone", "ipad"]) {
        (Sort::Phone, "An iPhone or iPad, by its name")
    } else if named(&["pixel-", "galaxy", "z-flip", "z-fold", "android"]) {
        (Sort::Phone, "A phone, by its name")
    } else if on(9100) || on(631) {
        (Sort::Printer, "A printer: it answers on a printing port")
    } else if on(554) {
        (Sort::Camera, "A camera: it answers on the camera-video port")
    } else if name.starts_with("desktop-") || name.starts_with("laptop-") || named(&["macbook", "imac"]) {
        (Sort::Computer, "A computer, by its name")
    } else if on(548) {
        (Sort::Computer, "A Mac: it answers on Mac file sharing")
    } else {
        return None;
    })
}

/// A line drawing of a kind of device, `size` points square.
fn device_icon(ui: &mut egui::Ui, sort: Sort, color: Color32, size: f32) -> egui::Response {
    use egui::{Rect, Stroke, StrokeKind};
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
    let p = ui.painter();
    let s = size / 16.0;
    let c = rect.center();
    let at = |x: f32, y: f32| c + vec2(x, y) * s;
    let stroke = Stroke::new(1.5 * s, color);
    let boxed = |x: f32, y: f32, w: f32, h: f32, r: f32| p.rect_stroke(Rect::from_center_size(at(x, y), vec2(w, h) * s), r * s, stroke, StrokeKind::Middle);
    match sort {
        Sort::Router => {
            boxed(0.0, 3.0, 14.0, 5.5, 1.5);
            p.line_segment([at(-4.0, 0.2), at(-5.5, -6.5)], stroke);
            p.line_segment([at(4.0, 0.2), at(5.5, -6.5)], stroke);
            p.circle_filled(at(-3.5, 3.0), 0.9 * s, color);
            p.circle_filled(at(0.0, 3.0), 0.9 * s, color);
        }
        Sort::Computer => {
            boxed(0.0, -1.5, 11.0, 8.0, 1.0);
            p.line_segment([at(-7.5, 4.5), at(7.5, 4.5)], Stroke::new(2.0 * s, color));
        }
        Sort::Tv => {
            boxed(0.0, -1.5, 14.0, 9.5, 1.5);
            p.line_segment([at(0.0, 3.3), at(0.0, 6.0)], stroke);
            p.line_segment([at(-3.5, 6.0), at(3.5, 6.0)], stroke);
        }
        Sort::Phone => {
            boxed(0.0, 0.0, 8.5, 14.0, 2.0);
            p.circle_filled(at(0.0, 4.8), 0.9 * s, color);
        }
        Sort::Printer => {
            boxed(0.0, -5.0, 7.0, 3.5, 0.5);
            boxed(0.0, 0.5, 14.0, 6.5, 1.5);
            boxed(0.0, 5.0, 7.0, 3.0, 0.5);
        }
        Sort::Camera => {
            boxed(-2.0, 0.0, 10.0, 8.0, 1.5);
            p.add(egui::Shape::convex_polygon(vec![at(3.5, -1.2), at(7.0, -3.5), at(7.0, 3.5), at(3.5, 1.2)], Color32::TRANSPARENT, stroke));
        }
        Sort::Speaker => {
            boxed(0.0, 0.0, 10.0, 14.0, 2.0);
            p.circle_stroke(at(0.0, 2.3), 2.8 * s, stroke);
            p.circle_filled(at(0.0, -4.0), 0.9 * s, color);
        }
    }
    resp
}

/// A symbol for what a device seems to be.
fn kind(d: &crate::devices::Device) -> &'static str {
    let name = d.name.as_deref().unwrap_or("").to_lowercase();
    let guess = d.guess();
    let has = |words: &[&str]| words.iter().any(|w| guess.contains(w) || name.contains(&w.to_lowercase()));
    if d.router {
        "🌐"
    } else if d.me || has(&["Windows", "Mac", "DESKTOP", "laptop"]) {
        "💻"
    } else if has(&["Roku", "Chromecast", "AirPlay", "Plex", "TV"]) {
        "📺"
    } else if has(&["iPhone", "Pixel", "Galaxy", "phone"]) || d.private_mac {
        "📱"
    } else if has(&["Printer"]) {
        "🖨"
    } else if has(&["Camera"]) {
        "📷"
    } else if has(&["Sonos", "speaker"]) {
        "🔊"
    } else {
        "💡"
    }
}

/// "15 min" or "hour".
fn every(mins: u64) -> String {
    if mins >= 60 { "hour".into() } else { format!("{mins} min") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::Device;

    fn device(name: Option<&str>, ports: &[u16], private_mac: bool) -> Device {
        Device {
            ip: "192.168.1.9".parse().unwrap(),
            mac: None,
            name: name.map(str::to_string),
            maker: None,
            private_mac,
            ports: ports.to_vec(),
            me: false,
            router: false,
            first_seen: chrono::Local::now(),
            new: false,
            joined: false,
        }
    }

    #[test]
    fn sure_only_when_known() {
        let sort = |d: Device| sure(&d).map(|s| s.0);
        assert_eq!(sort(device(Some("X00400NAXFG5"), &[8060, 7000], false)), Some(Sort::Tv));
        assert_eq!(sort(device(Some("Delores-s-Z-Flip7"), &[], true)), Some(Sort::Phone));
        assert_eq!(sort(device(None, &[62078], true)), Some(Sort::Phone));
        assert_eq!(sort(device(Some("DESKTOP-K3V08PF"), &[], false)), Some(Sort::Computer));
        assert_eq!(sort(device(None, &[631, 80], false)), Some(Sort::Printer));
        // Guesses: a private address, AirPlay alone, a web page.
        assert_eq!(sort(device(None, &[], true)), None);
        assert_eq!(sort(device(None, &[7000], false)), None);
        assert_eq!(sort(device(Some("Unnamed"), &[80, 443], false)), None);
    }
}
