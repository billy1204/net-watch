//! The firewall check: whether this computer's firewall is on and what gets
//! out, when asked.
use super::{Ask, Tool, View};
use crate::look::{self, bad, good, warn, pal};
use crate::monitor::{Shared, Status};
use crate::{MEDIUM, firewall, tag, when};
use eframe::egui::{self, Color32, vec2};

pub struct Firewall;

impl Tool for Firewall {
    fn name(&self) -> &'static str {
        "Firewall check"
    }

    fn icon(&self) -> &'static str {
        "🛡"
    }

    fn about(&self) -> &'static str {
        "Whether this computer's firewall is on, and which kinds of traffic get out"
    }

    fn icon_color(&self, st: &Shared) -> Color32 {
        match &st.firewall {
            Some(r) if r.blocked().count() > 0 => bad(),
            Some(_) => good(),
            None => pal().weak,
        }
    }

    fn card(&self, ui: &mut egui::Ui, st: &Shared) {
        if st.checking_firewall {
            super::dot_line(ui, pal().accent, "Checking…");
            return;
        }
        let Some(r) = &st.firewall else {
            ui.label(egui::RichText::new("Not checked yet").color(pal().weak));
            return;
        };
        match r.blocked().count() {
            0 => super::dot_line(ui, good(), "Everything that should get out does"),
            1 => super::dot_line(ui, bad(), "1 kind of traffic is blocked"),
            n => super::dot_line(ui, bad(), format!("{n} kinds of traffic are blocked")),
        }
        match r.computer_on {
            Some(true) => super::pill(ui, "This computer's firewall is on", good()),
            Some(false) => super::pill(ui, "This computer's firewall is off", warn()),
            None => {}
        }
    }

    fn glance_width(&self) -> f32 {
        76.0
    }

    /// A dot for each kind of traffic tried, in its result's color.
    fn glance(&self, ui: &mut egui::Ui, st: &Shared) {
        let n = st.firewall.as_ref().map_or(12, |r| r.lines.len());
        let rect = super::glance_box(ui, vec2(76.0, 34.0), &format!("{n} kinds tried"));
        for i in 0..n {
            let color = match st.firewall.as_ref().map(|r| &r.lines[i]) {
                None => pal().key_hover,
                Some(l) => match (&l.outcome, l.fine) {
                    (firewall::Outcome::Open(_), _) => good(),
                    (firewall::Outcome::NoName, _) => warn(),
                    (_, Some(_)) => pal().weak,
                    _ => bad(),
                },
            };
            let (row, col) = (i / 6, i % 6);
            let c = egui::pos2(rect.left() + 11.0 + col as f32 * 10.8, rect.top() + 11.0 + row as f32 * 12.0);
            ui.painter().circle_filled(c, 3.5 * look::appear(ui.ctx()).max(0.3), color);
            if let Some(l) = st.firewall.as_ref().map(|r| &r.lines[i])
                && super::pointing_at(ui, egui::Rect::from_center_size(c, vec2(11.0, 12.0)))
            {
                let (word, detail) = firewall::verdict(&l.outcome);
                look::set_tip(ui.ctx(), format!("{} ({}): {word}, {detail}", l.name, l.what));
            }
        }
    }

    fn footer(&self, st: &Shared) -> String {
        st.firewall.as_ref().map_or(self.about().into(), |r| format!("Checked {} · about 5 seconds", super::when_mid(r.at)))
    }

    fn show(&mut self, ui: &mut egui::Ui, v: &mut View) {
        let st = v.st;
        let narrow = ui.available_width() < MEDIUM;
        let weak = |t: &str| egui::RichText::new(t).color(pal().weak);
        look::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(look::caption("Firewall check"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if st.firewall.is_some() { "Check again" } else { "Check firewall" };
                    let can = !st.checking_firewall && st.status() != Status::Down;
                    if look::button(ui, label, false, can).clicked() {
                        v.asks.push(Ask::CheckFirewall);
                    }
                    if st.checking_firewall {
                        ui.spinner();
                    } else if let Some(r) = &st.firewall {
                        ui.label(egui::RichText::new(format!("Checked {}", when(r.at))).small().color(pal().weak));
                    }
                });
            });
            let Some(r) = &st.firewall else {
                let text = if st.checking_firewall {
                    "Checking…"
                } else {
                    "Checks whether this computer's firewall is on, and which kinds of traffic get out to the internet: \
                     websites, email, video calls and so on. Takes a few seconds and almost no data."
                };
                ui.add(egui::Label::new(weak(text)).wrap());
                return;
            };
            ui.horizontal_wrapped(|ui| {
                ui.label("This computer:");
                ui.label(weak(&r.computer));
            });
            let blocked: Vec<&str> = r.blocked().map(|l| l.name).collect();
            let summary = match blocked.len() {
                0 => "Everything that should get out does.".to_string(),
                n => format!(
                    "{n} blocked ({}). Something between this computer and the internet stops {}: this computer's \
                     firewall, the router or your provider.",
                    blocked.join(", ").to_lowercase(),
                    if n == 1 { "it" } else { "them" }
                ),
            };
            ui.add(egui::Label::new(egui::RichText::new(summary).color(if blocked.is_empty() { good() } else { warn() })).wrap());
            egui::Grid::new("firewall").striped(true).num_columns(if narrow { 2 } else { 4 }).spacing(vec2(18.0, 8.0)).show(ui, |ui| {
                for l in &r.lines {
                    let (word, detail) = firewall::verdict(&l.outcome);
                    let open = matches!(l.outcome, firewall::Outcome::Open(_));
                    let color = match (open, l.fine) {
                        (true, _) => good(),
                        (false, Some(_)) => pal().weak,
                        _ if matches!(l.outcome, firewall::Outcome::NoName) => warn(),
                        _ => bad(),
                    };
                    ui.label(l.name).on_hover_text(format!("{} to {}", l.what, l.host));
                    if !narrow {
                        ui.label(weak(l.what));
                    }
                    tag(ui, word, color);
                    if !narrow {
                        let text = match l.fine {
                            Some(why) if !open => format!("{detail}. {why}"),
                            _ => detail,
                        };
                        ui.add(egui::Label::new(weak(&text)).wrap());
                    }
                    ui.end_row();
                }
            });
        });
    }
}
