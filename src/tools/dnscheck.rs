//! The DNS check: whether this computer's name server is quick and honest
//! next to the public ones, and switching to one in a click (tried with
//! Network's undo, so a bad change puts itself back).
use super::{Ask, Tool, View};
use crate::diag::ms;
use crate::dns::PUBLIC;
use crate::look::{self, good, warn, pal};
use crate::monitor::Shared;
use crate::netif::{Adapter, Do};
use crate::{MEDIUM, when};
use eframe::egui::{self, Color32, vec2};

pub const NAME: &str = "DNS check";

pub struct DnsCheck {
    details: bool,
}

impl DnsCheck {
    pub fn new() -> Self {
        DnsCheck { details: std::env::var_os("NET_WATCH_DETAILS").is_some() }
    }
}

/// The adapter the internet goes through, with its saved settings.
fn primary(st: &Shared) -> Option<&Adapter> {
    st.net.adapters.as_ref()?.as_ref().ok()?.iter().find(|a| a.primary && a.profile.is_some())
}

/// Use these DNS servers on the adapter the internet goes through
/// (an empty list: the router's again), then show Network, where the change
/// is tried and can be undone.
fn switch(st: &Shared, dns: Vec<String>, asks: &mut Vec<Ask>) {
    let Some(a) = primary(st) else { return };
    let Some(old) = a.profile.clone() else { return };
    let mut new = old.clone();
    new.ignore_auto_dns = !dns.is_empty();
    new.dns = dns;
    asks.push(Ask::Net(Do::Apply { dev: a.dev.clone(), old, new, active: a.active, primary: a.primary }));
    asks.push(Ask::Open(super::NETWORK));
}

fn public_dns(name: &str) -> Option<Vec<String>> {
    PUBLIC.iter().find(|p| p.0 == name).map(|p| vec![std::net::Ipv4Addr::from(p.1).to_string(), std::net::Ipv4Addr::from(p.2).to_string()])
}

impl Tool for DnsCheck {
    fn name(&self) -> &'static str {
        NAME
    }

    fn icon(&self) -> &'static str {
        "🌐"
    }

    fn about(&self) -> &'static str {
        "How fast and honest your DNS (the internet's phone book) is, next to Cloudflare, Google and Quad9"
    }

    fn icon_color(&self, st: &Shared) -> Color32 {
        match &st.dns {
            Some(r) if r.verdict().2 => warn(),
            Some(_) => good(),
            None => pal().accent,
        }
    }

    fn action(&self, st: &Shared) -> Option<(String, Option<Ask>)> {
        Some(if st.checking_dns { ("Checking…".into(), None) } else { ("Check".into(), Some(Ask::CheckDns)) })
    }

    fn card(&self, ui: &mut egui::Ui, st: &Shared) {
        match &st.dns {
            _ if st.checking_dns => super::dot_line(ui, pal().accent, "Checking…"),
            None => {
                ui.label(egui::RichText::new("Not checked yet").color(pal().weak));
            }
            Some(r) => {
                let (head, _, problem) = r.verdict();
                super::dot_line(ui, if problem { warn() } else { good() }, head);
            }
        }
    }

    fn glance_width(&self) -> f32 {
        76.0
    }

    /// A bar per server, yours first in the accent color.
    fn glance(&self, ui: &mut egui::Ui, st: &Shared) {
        let rect = super::glance_box(ui, vec2(76.0, 34.0), "vs public");
        let Some(r) = &st.dns else {
            for i in 0..4 {
                let x = rect.left() + 12.0 + i as f32 * 15.0;
                ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - 18.0), egui::pos2(x + 8.0, rect.bottom() - 6.0)), 2.0, pal().key_hover);
            }
            return;
        };
        let slowest = r.servers.iter().filter_map(|s| s.ready_ms).fold(1.0f32, f32::max);
        for (i, s) in r.servers.iter().take(4).enumerate() {
            let x = rect.left() + 12.0 + i as f32 * 15.0;
            let h = 22.0 * s.ready_ms.map_or(1.0, |v| v / slowest) * look::appear(ui.ctx());
            let bar = egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - 6.0 - h.max(2.0)), egui::pos2(x + 8.0, rect.bottom() - 6.0));
            ui.painter().rect_filled(bar, 2.0, if s.yours { pal().accent } else { pal().accent.gamma_multiply(0.35) });
            if super::pointing_at(ui, bar.expand(3.0)) {
                look::set_tip(ui.ctx(), format!("{}: {}", s.name, s.ready_ms.map_or("no answer".into(), ms)));
            }
        }
    }

    fn footer(&self, st: &Shared) -> String {
        st.dns.as_ref().map_or(self.about().into(), |r| format!("Checked {} · about a second", super::when_mid(r.at)))
    }

    fn show(&mut self, ui: &mut egui::Ui, v: &mut View) {
        let st = v.st;
        let narrow = ui.available_width() < MEDIUM;
        let weak = |t: &str| egui::RichText::new(t).color(pal().weak);
        let can_switch = primary(st).is_some() && st.net.trial.is_none();
        look::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(look::caption("DNS check"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if look::button(ui, if st.dns.is_some() { "Check again" } else { "Check DNS" }, true, !st.checking_dns).clicked() {
                        v.asks.push(Ask::CheckDns);
                    }
                    if st.checking_dns {
                        ui.spinner();
                    } else if let Some(r) = &st.dns {
                        if look::chip(ui, "Copy report", true, true).clicked() {
                            ui.ctx().copy_text(r.text());
                            v.asks.push(Ask::Note("Copied the DNS check.".into(), false, None));
                        }
                        if !narrow {
                            ui.label(egui::RichText::new(format!("Checked {}", when(r.at))).small().color(pal().weak));
                        }
                    }
                });
            });
            let Some(r) = &st.dns else {
                let text = if st.checking_dns {
                    "Checking…"
                } else {
                    "Before any website opens, this computer asks a DNS server for the site's address, like looking it up in a \
                     phone book. This times yours against Cloudflare, Google and Quad9, and checks it doesn't send mistyped \
                     addresses to an ad page. Takes about a second."
                };
                ui.add(egui::Label::new(weak(text)).wrap());
                return;
            };

            ui.add_space(4.0);
            let (head, detail, problem) = r.verdict();
            ui.add(egui::Label::new(egui::RichText::new(head).size(17.0).color(if problem { warn() } else { good() })).wrap());
            ui.add(egui::Label::new(detail).wrap());
            if let Some(b) = r.better()
                && let Some(dns) = public_dns(&b.name)
            {
                let label = format!("Use {}", b.name);
                if look::button(ui, &label, true, can_switch).on_hover_text("Tried first: if the internet doesn't come back, the old settings return by themselves").clicked() {
                    switch(st, dns, v.asks);
                }
            }
            ui.add_space(6.0);
            ui.separator();

            ui.horizontal(|ui| {
                ui.label(look::caption("Side by side"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    look::toggle(ui, &mut self.details);
                    ui.label(weak("Show details"));
                });
            });
            let slowest = r.servers.iter().flat_map(|s| [s.ready_ms, s.fresh_ms]).flatten().fold(1.0f32, f32::max);
            let came = look::changed_at(ui.ctx(), egui::Id::new("dns bars"), r.at.timestamp_millis() as u64);
            let bar_w = if narrow { 90.0 } else { 220.0 };
            egui::Grid::new("dns servers").num_columns(4).spacing(vec2(14.0, 8.0)).show(ui, |ui| {
                for (i, s) in r.servers.iter().enumerate() {
                    let grow = look::progress(ui.ctx(), came + 0.05 * i as f64, 0.5);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&s.name).strong());
                        if s.yours {
                            super::pill(ui, "Yours", pal().accent);
                        }
                    });
                    if !narrow {
                        ui.label(egui::RichText::new(s.ip.to_string()).monospace().size(12.0).color(pal().weak));
                    }
                    // Popular names on top, made-up (fresh) ones under.
                    let rows = if self.details { 2 } else { 1 };
                    let (rect, _) = ui.allocate_exact_size(vec2(bar_w + 80.0, 12.0 * rows as f32 + 2.0), egui::Sense::hover());
                    for (k, val) in [s.ready_ms, s.fresh_ms].into_iter().take(rows).enumerate() {
                        let bar = egui::Rect::from_min_size(rect.min + vec2(0.0, 2.0 + k as f32 * 12.0), vec2(bar_w, 7.0));
                        ui.painter().rect_filled(bar, 3.5, pal().key_hover);
                        let color = if s.yours { pal().accent } else { pal().accent.gamma_multiply(0.45) };
                        let text = match val {
                            Some(t) => {
                                ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, vec2((bar_w * t / slowest).max(3.0) * grow, 7.0)), 3.5, color);
                                ms(t)
                            }
                            None => "no answer".into(),
                        };
                        let text = if self.details { format!("{text} {}", if k == 0 { "popular" } else { "fresh" }) } else { text };
                        ui.painter().text(egui::pos2(bar.right() + 8.0, bar.center().y), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(12.0), pal().text);
                    }
                    ui.horizontal(|ui| {
                        if s.failed > 0 {
                            ui.label(egui::RichText::new(format!("{} of {} unanswered", s.failed, s.asked)).small().color(warn()));
                        }
                        if !s.yours
                            && let Some(dns) = public_dns(&s.name)
                            && look::chip(ui, "Use", can_switch, true).on_hover_text(format!("Switch this connection to {} ({})", s.name, dns.join(", "))).clicked()
                        {
                            switch(st, dns, v.asks);
                        }
                    });
                    ui.end_row();
                }
            });
            if let Some(p) = primary(st).and_then(|a| a.profile.as_ref()).filter(|p| p.ignore_auto_dns) {
                ui.horizontal_wrapped(|ui| {
                    ui.label(weak(&format!("This connection is set to use {} instead of the router's.", p.dns.join(", "))));
                    if look::chip(ui, "Use the router's again", can_switch, true).clicked() {
                        switch(st, Vec::new(), v.asks);
                    }
                });
            }
            ui.add_space(6.0);
            ui.separator();
            let yn = |v: Option<bool>| v.map(|b| if b { good() } else { warn() }).unwrap_or(pal().weak);
            egui::Grid::new("dns facts").num_columns(2).spacing(vec2(18.0, 6.0)).show(ui, |ui| {
                ui.label("Mistyped addresses");
                let honest = r.redirects.map(|b| !b);
                ui.label(egui::RichText::new(match honest {
                    Some(true) => "Answered honestly: \"no such site\"",
                    Some(false) => "Sent to an ad page by your DNS server",
                    None => "Couldn't tell",
                })
                .color(yn(honest)));
                ui.end_row();
                ui.label("Encrypted lookups");
                ui.label(egui::RichText::new(match r.encrypted {
                    Some(true) => "On: others on the network can't see which sites you look up",
                    Some(false) => "Off: like most home setups, lookups travel unencrypted",
                    None => "Couldn't tell",
                })
                .color(if r.encrypted == Some(true) { good() } else { pal().weak }));
                ui.end_row();
            });
            if self.details {
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(weak(
                        "Popular: the middle time for 8 well-known sites, which servers usually have ready. Fresh: made-up names, \
                         which make the server go and ask, like a site nobody near you has opened lately.",
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
