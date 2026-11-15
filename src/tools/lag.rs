//! "Where's the lag?": every hop between here and a site, and whose part of
//! the way the delay or loss is in.
use super::{Ask, Tool, View};
use crate::diag::ms;
use crate::look::{self, good, warn, pal};
use crate::monitor::Shared;
use crate::route::{Trace, Who};
use crate::{MEDIUM, when};
use eframe::egui::{self, Color32, vec2};

pub const NAME: &str = "Where's the lag?";
const PRESETS: [&str; 2] = ["Cloudflare", "Google"];

pub struct Lag {
    /// 0 and 1 are the presets, 2 a website typed in.
    pick: usize,
    site: String,
    details: bool,
}

impl Lag {
    pub fn new() -> Self {
        Lag { pick: 0, site: String::new(), details: std::env::var_os("NET_WATCH_DETAILS").is_some() }
    }

    fn target(&self) -> String {
        PRESETS.get(self.pick).map_or_else(|| self.site.trim().to_string(), |p| p.to_string())
    }
}

/// The color for a part of the way.
fn tint(who: Who) -> Color32 {
    match who {
        Who::Home => pal().accent,
        Who::Provider => pal().accent.gamma_multiply(0.6),
        Who::Beyond | Who::Destination => pal().accent.gamma_multiply(0.3),
    }
}

/// Home, provider and beyond as one bar, each part as long as its share of
/// the time.
fn split_bar(ui: &mut egui::Ui, rect: egui::Rect, t: &Trace) {
    let p = ui.painter();
    p.rect_filled(rect, rect.height() / 2.0, pal().key_hover);
    let Some((home, provider, beyond)) = t.split() else { return };
    let total = (home + provider + beyond).max(0.001);
    let mut x = rect.left();
    for (i, (who, part)) in [(Who::Home, home), (Who::Provider, provider), (Who::Beyond, beyond)].into_iter().enumerate() {
        let w = look::glide(ui.ctx(), egui::Id::new(("lag split", i, rect.width() as i32)), rect.width() * part / total, 0.5);
        if w < 0.5 {
            continue;
        }
        let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, rect.height()));
        p.rect_filled(seg, rect.height() / 2.0, tint(who));
        x += w;
    }
}

impl Tool for Lag {
    fn name(&self) -> &'static str {
        NAME
    }

    fn icon(&self) -> &'static str {
        "⏱"
    }

    fn about(&self) -> &'static str {
        "Follows the way to a site hop by hop, and shows whether the delay is at home, your provider or beyond"
    }

    fn icon_color(&self, st: &Shared) -> Color32 {
        match &st.route {
            Some(t) if t.done => {
                if t.verdict().2 {
                    warn()
                } else {
                    good()
                }
            }
            _ => pal().accent,
        }
    }

    fn action(&self, st: &Shared) -> Option<(String, Option<Ask>)> {
        let to = self.target();
        Some(if st.tracing { ("Tracing…".into(), None) } else { ("Trace".into(), (!to.is_empty()).then_some(Ask::Trace(to))) })
    }

    fn card(&self, ui: &mut egui::Ui, st: &Shared) {
        match &st.route {
            _ if st.tracing => super::dot_line(ui, pal().accent, "Finding the way…"),
            None => {
                ui.label(egui::RichText::new("Not traced yet").color(pal().weak));
            }
            Some(t) => {
                let (head, _, problem) = t.verdict();
                super::dot_line(ui, if problem { warn() } else { good() }, head);
            }
        }
    }

    fn glance_width(&self) -> f32 {
        76.0
    }

    fn glance(&self, ui: &mut egui::Ui, st: &Shared) {
        let n = st.route.as_ref().map_or(0, |t| t.hops.len());
        let rect = super::glance_box(ui, vec2(76.0, 34.0), &if n > 0 { format!("{n} hops") } else { "hops".into() });
        let bar = egui::Rect::from_center_size(rect.center(), vec2(58.0, 8.0));
        match &st.route {
            Some(t) if t.done => split_bar(ui, bar, t),
            _ => {
                ui.painter().rect_filled(bar, 4.0, pal().key_hover);
            }
        }
        if let Some((h, p, b)) = st.route.as_ref().and_then(Trace::split)
            && super::pointing_at(ui, rect)
        {
            look::set_tip(ui.ctx(), format!("Your home {} · your provider {} · beyond {}", ms(h), ms(p), ms(b)));
        }
    }

    fn footer(&self, st: &Shared) -> String {
        match &st.route {
            Some(t) if t.done => format!("To {} {} · about 5 seconds", t.target, super::when_mid(t.at)),
            _ => self.about().into(),
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, v: &mut View) {
        let st = v.st;
        let narrow = ui.available_width() < MEDIUM;
        let weak = |t: &str| egui::RichText::new(t).color(pal().weak);
        look::card(ui, |ui| {
            let mut go = false;
            ui.horizontal(|ui| {
                ui.label(look::caption("Where's the lag?"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let can = !st.tracing && !self.target().is_empty();
                    go |= look::button(ui, if st.route.is_some() { "Trace again" } else { "Trace" }, true, can).clicked();
                    if st.tracing {
                        ui.spinner();
                    } else if let Some(t) = st.route.as_ref().filter(|t| t.done && t.error.is_none()) {
                        if look::chip(ui, "Copy report", true, true).on_hover_text("Copy every hop and its numbers, to send to your provider").clicked() {
                            ui.ctx().copy_text(t.text());
                            v.asks.push(Ask::Note("Copied the route. Paste it into an email or chat.".into(), false, None));
                        }
                        if !narrow {
                            ui.label(egui::RichText::new(when(t.at)).small().color(pal().weak));
                        }
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("To");
                look::tabs(ui, "lag target", &mut self.pick, &["Cloudflare", "Google", "A website"]);
                if self.pick == 2 {
                    let edit = ui.add(egui::TextEdit::singleline(&mut self.site).hint_text("like youtube.com").desired_width(200.0).char_limit(253));
                    go |= edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !st.tracing;
                }
            });
            if go && !self.target().is_empty() {
                v.asks.push(Ask::Trace(self.target()));
            }
            let Some(t) = &st.route else {
                let text = if st.tracing {
                    "Finding the way…"
                } else {
                    "Follows the way from this computer to a site, one hop (router) at a time, and times each one. \
                     It shows whether a delay or dropouts start in your home network, your provider's network or \
                     beyond it, so you know who can fix it. Takes about 5 seconds."
                };
                ui.add(egui::Label::new(weak(text)).wrap());
                return;
            };

            ui.add_space(4.0);
            if t.done {
                let (head, detail, problem) = t.verdict();
                ui.add(egui::Label::new(egui::RichText::new(head).size(17.0).color(if problem { warn() } else { good() })).wrap());
                ui.add(egui::Label::new(detail).wrap());
                if t.error.is_some() {
                    return;
                }
                ui.add_space(6.0);
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 12.0), egui::Sense::hover());
                split_bar(ui, rect, t);
                if let Some((h, p, b)) = t.split() {
                    ui.horizontal_wrapped(|ui| {
                        for (who, part) in [(Who::Home, h), (Who::Provider, p), (Who::Beyond, b)] {
                            let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().circle_filled(dot.center(), 4.5, tint(who));
                            let name = match (who, &t.provider) {
                                (Who::Provider, Some(p)) => format!("Your provider ({p})"),
                                _ => who.word().to_string(),
                            };
                            ui.label(format!("{name} {}", ms(part)));
                            ui.add_space(10.0);
                        }
                    });
                }
            } else {
                ui.label(egui::RichText::new(format!("Found {} hops; timing each…", t.hops.len())).size(17.0));
            }
            ui.add_space(6.0);
            ui.separator();

            ui.horizontal(|ui| {
                ui.label(look::caption("Hop by hop"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    look::toggle(ui, &mut self.details);
                    ui.label(weak("Show details"));
                });
            });
            let slowest = t.hops.iter().filter_map(|h| h.max_ms.or(h.avg_ms)).fold(1.0f32, f32::max);
            let came = look::changed_at(ui.ctx(), egui::Id::new("lag hops"), t.at.timestamp_millis() as u64 ^ t.done as u64);
            let bar_w = if narrow { 70.0 } else { 140.0 };
            egui::Grid::new("lag hops").num_columns(5).striped(true).spacing(vec2(14.0, 6.0)).show(ui, |ui| {
                for (i, h) in t.hops.iter().enumerate() {
                    let fade = look::progress(ui.ctx(), came + 0.03 * i as f64, 0.3);
                    ui.label(weak(&h.n.to_string()));
                    super::pill(ui, h.who.word(), if h.who == Who::Home { pal().accent } else { pal().weak });
                    let mut label = h.label();
                    if self.details && let (Some(ip), Some(_)) = (h.ip, &h.name) {
                        label = format!("{label}  ({ip})");
                    }
                    let max_w = if narrow { 140.0 } else { 330.0 };
                    ui.allocate_ui_with_layout(vec2(max_w, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_width(max_w);
                        ui.add(egui::Label::new(egui::RichText::new(label).monospace().size(12.0)).truncate());
                    });
                    // How long it takes to answer, as a bar against the slowest.
                    let (rect, _) = ui.allocate_exact_size(vec2(bar_w + 70.0, 14.0), egui::Sense::hover());
                    let bar = egui::Rect::from_min_size(rect.min + vec2(0.0, 3.0), vec2(bar_w, 8.0));
                    ui.painter().rect_filled(bar, 4.0, pal().key_hover);
                    match h.avg_ms {
                        Some(a) => {
                            let w = (bar_w * a / slowest).max(3.0) * fade;
                            ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, vec2(w, 8.0)), 4.0, tint(h.who));
                            let text = if self.details { format!("{} · {}", ms(a), h.max_ms.map_or("-".into(), ms)) } else { ms(a) };
                            ui.painter().text(egui::pos2(bar.right() + 8.0, rect.center().y), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(12.5), pal().text);
                        }
                        None if !t.done => {
                            ui.painter().text(egui::pos2(bar.right() + 8.0, rect.center().y), egui::Align2::LEFT_CENTER, "…", egui::FontId::proportional(12.5), pal().weak);
                        }
                        None => {
                            ui.painter().text(egui::pos2(bar.right() + 8.0, rect.center().y), egui::Align2::LEFT_CENTER, "no answer", egui::FontId::proportional(12.5), pal().weak);
                        }
                    }
                    match h.loss() {
                        Some(0) | None => ui.label(""),
                        Some(l) => ui.label(egui::RichText::new(format!("{l}% lost")).color(warn())),
                    };
                    ui.end_row();
                }
            });
            if self.details {
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(weak(
                        "Times are the average and the worst of 10 pings. Routers on the way often answer pings slowly or skip some \
                         when busy; that only matters when it carries on to the hops after.",
                    ))
                    .wrap(),
                );
            }
        });
    }

    fn max_width(&self) -> f32 {
        900.0
    }
}
