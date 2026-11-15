//! "What's wrong?": the connection checked step by step, from this computer
//! out to websites, with where it breaks and what to try.
use super::{Ask, Tool, View};
use crate::diag::{Help, Mark, STEPS};
use crate::look::{self, bad, good, warn, pal};
use crate::monitor::Shared;
use crate::{MEDIUM, when};
use eframe::egui::{self, Color32, vec2};

pub const NAME: &str = "What's wrong?";

pub struct Diagnose {
    details: bool,
}

impl Diagnose {
    pub fn new() -> Self {
        // Dev aid: NET_WATCH_DETAILS=1 starts with the details showing.
        Diagnose { details: std::env::var_os("NET_WATCH_DETAILS").is_some() }
    }
}

fn color(m: Mark) -> Color32 {
    match m {
        Mark::Pass => good(),
        Mark::Warn => warn(),
        Mark::Fail => bad(),
        Mark::Skipped => pal().weak,
    }
}

fn symbol(m: Mark) -> &'static str {
    match m {
        Mark::Pass => "✔",
        Mark::Warn => "!",
        Mark::Fail => "✖",
        Mark::Skipped => "–",
    }
}

/// The button for a fix's helper.
fn help_button(ui: &mut egui::Ui, help: Help, asks: &mut Vec<Ask>) {
    let (label, ask) = match help {
        Help::Network => ("Open Network", Ask::Open(super::NETWORK)),
        Help::SignIn => ("Open the sign-in page", Ask::SignIn),
    };
    if look::button(ui, label, true, true).clicked() {
        asks.push(ask);
    }
}

impl Tool for Diagnose {
    fn name(&self) -> &'static str {
        NAME
    }

    fn icon(&self) -> &'static str {
        "🔍"
    }

    fn about(&self) -> &'static str {
        "Checks the connection step by step, finds where it breaks and says what to try"
    }

    fn icon_color(&self, st: &Shared) -> Color32 {
        match &st.diag {
            Some(r) if r.done() => r.problem().map_or(good(), |s| color(s.mark)),
            _ => pal().accent,
        }
    }

    fn action(&self, st: &Shared) -> Option<(String, Option<Ask>)> {
        Some(if st.diagnosing { ("Checking…".into(), None) } else { ("Check".into(), Some(Ask::Diagnose)) })
    }

    fn card(&self, ui: &mut egui::Ui, st: &Shared) {
        let Some(r) = &st.diag else {
            ui.label(egui::RichText::new("Not checked yet").color(pal().weak));
            return;
        };
        if st.diagnosing {
            super::dot_line(ui, pal().accent, format!("Checking… step {} of {}", (r.steps.len() + 1).min(STEPS.len()), STEPS.len()));
            return;
        }
        match r.problem() {
            Some(s) => super::dot_line(ui, color(s.mark), &s.says),
            None => super::dot_line(ui, good(), "Everything checks out"),
        }
    }

    fn glance_width(&self) -> f32 {
        76.0
    }

    /// A dot for each step, in its result's color, joined by a line.
    fn glance(&self, ui: &mut egui::Ui, st: &Shared) {
        let rect = super::glance_box(ui, vec2(76.0, 34.0), &format!("{} steps", STEPS.len()));
        let p = ui.painter();
        let y = rect.center().y;
        let x = |i: usize| rect.left() + 10.0 + i as f32 * 9.33;
        p.line_segment([egui::pos2(x(0), y), egui::pos2(x(STEPS.len() - 1), y)], egui::Stroke::new(1.0, pal().edge));
        for (i, name) in STEPS.iter().enumerate() {
            let step = st.diag.as_ref().and_then(|r| r.steps.get(i));
            let c = step.map_or(pal().key_hover, |s| color(s.mark));
            let at = egui::pos2(x(i), y);
            p.circle_filled(at, 3.2 * look::appear(ui.ctx()).max(0.3), c);
            if super::pointing_at(ui, egui::Rect::from_center_size(at, vec2(9.0, 16.0))) {
                look::set_tip(ui.ctx(), step.map_or_else(|| name.to_string(), |s| format!("{name}: {}", s.says)));
            }
        }
    }

    fn footer(&self, st: &Shared) -> String {
        match &st.diag {
            Some(r) if r.done() => format!("Checked {} · about 5 seconds", super::when_mid(r.at)),
            _ => self.about().into(),
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, v: &mut View) {
        let st = v.st;
        let narrow = ui.available_width() < MEDIUM;
        let weak = |t: &str| egui::RichText::new(t).color(pal().weak);
        look::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(look::caption("What's wrong?"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if st.diag.is_some() { "Check again" } else { "Check my connection" };
                    if look::button(ui, label, true, !st.diagnosing).clicked() {
                        v.asks.push(Ask::Diagnose);
                    }
                    if st.diagnosing {
                        ui.spinner();
                    } else if let Some(r) = &st.diag {
                        if look::chip(ui, "Copy report", true, true).on_hover_text("Copy every step and its numbers, to send to your provider").clicked() {
                            ui.ctx().copy_text(r.text());
                            v.asks.push(Ask::Note("Copied the report. Paste it into an email or chat.".into(), false, None));
                        }
                        if !narrow {
                            ui.label(egui::RichText::new(format!("Checked {}", when(r.at))).small().color(pal().weak));
                        }
                    }
                });
            });
            let Some(r) = &st.diag else {
                ui.add(
                    egui::Label::new(weak(
                        "Checks the connection one step at a time, from this computer out to websites: the cable or Wi-Fi, \
                         the router, your provider, names (DNS) and websites. It stops where things break and says what to try. \
                         Takes about 5 seconds.",
                    ))
                    .wrap(),
                );
                return;
            };

            // The answer first: what's wrong and what to try.
            ui.add_space(4.0);
            match (st.diagnosing, r.problem()) {
                (true, _) => {
                    ui.label(egui::RichText::new("Checking…").size(17.0));
                }
                (false, None) => {
                    ui.label(egui::RichText::new("Everything checks out.").size(17.0).color(good()));
                    ui.add(egui::Label::new(weak("If something still feels slow, a speed test shows how fast it really is.")).wrap());
                    if look::button(ui, "Open Speed test", false, true).clicked() {
                        v.asks.push(Ask::Open(super::SPEED));
                    }
                }
                (false, Some(s)) => {
                    let head = if s.mark == Mark::Fail { format!("Problem: {}", s.name.to_lowercase()) } else { format!("Could be better: {}", s.name.to_lowercase()) };
                    ui.label(egui::RichText::new(head).size(17.0).color(color(s.mark)));
                    ui.add(egui::Label::new(&s.says).wrap());
                    if let Some(fix) = &s.fix {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new("Try:").strong());
                            ui.add(egui::Label::new(fix.as_str()).wrap());
                        });
                    }
                    if let Some(h) = s.help {
                        help_button(ui, h, v.asks);
                    }
                }
            }
            ui.add_space(6.0);
            ui.separator();

            // Every step, each dropping in as it's done.
            ui.horizontal(|ui| {
                ui.label(look::caption("Step by step"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    look::toggle(ui, &mut self.details);
                    ui.label(weak("Show details"));
                });
            });
            let came = look::changed_at(ui.ctx(), egui::Id::new("diag steps"), r.steps.len() as u64 | (r.at.timestamp_millis() as u64) << 8);
            for (i, name) in STEPS.iter().enumerate() {
                let step = r.steps.get(i);
                let newest = i + 1 == r.steps.len();
                let t = if newest { look::progress(ui.ctx(), came, 0.3) } else { 1.0 };
                let drop = egui::emath::TSTransform::from_translation(vec2(0.0, -6.0 * (1.0 - t)));
                ui.scope(|ui| {
                    ui.multiply_opacity(if step.is_some() { t } else { 0.45 });
                    ui.with_visual_transform(drop, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::hover());
                            let running = step.is_none() && st.diagnosing && i == r.steps.len();
                            match step {
                                Some(s) => {
                                    let c = color(s.mark);
                                    ui.painter().circle_filled(rect.center(), 11.0, c.gamma_multiply(if pal().light { 0.14 } else { 0.22 }));
                                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, symbol(s.mark), egui::FontId::proportional(12.5), c);
                                }
                                None if running => {
                                    egui::Spinner::new().size(16.0).paint_at(ui, rect.shrink(3.0));
                                }
                                None => {
                                    ui.painter().circle_stroke(rect.center(), 10.0, egui::Stroke::new(1.0, pal().edge));
                                }
                            }
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.label(egui::RichText::new(*name).strong());
                                if let Some(s) = step {
                                    ui.add(egui::Label::new(weak(&s.says)).wrap());
                                }
                            });
                        });
                        if let Some(s) = step.filter(|s| self.details && !s.details.is_empty()) {
                            ui.horizontal(|ui| {
                                ui.add_space(32.0);
                                ui.spacing_mut().interact_size.y = 0.0;
                                egui::Grid::new(("diag details", i)).num_columns(2).min_col_width(170.0).spacing(vec2(14.0, 3.0)).show(ui, |ui| {
                                    for (k, val) in &s.details {
                                        ui.label(egui::RichText::new(*k).small().color(pal().weak));
                                        ui.add(egui::Label::new(egui::RichText::new(val).small().monospace()).wrap());
                                        ui.end_row();
                                    }
                                });
                            });
                        }
                    });
                });
                ui.add_space(4.0);
            }
        });
    }

    fn max_width(&self) -> f32 {
        820.0
    }
}
