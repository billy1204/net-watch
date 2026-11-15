//! Speed tests: the one running now, the last result, the graph and every
//! test in the period.
use super::{Ask, Tool, View};
use crate::look::{self, bad, pal};
use crate::monitor::{Shared, Status};
use crate::{MEDIUM, RANGES, big, charts, data, ookla, probe, speed_error, speed_value, tag, test_left, when};
use chrono::{Duration as Span, Local};
use eframe::egui::{self, Color32, vec2};

pub const NAME: &str = "Speed test";

pub struct Speed;

impl Tool for Speed {
    fn name(&self) -> &'static str {
        NAME
    }

    fn icon(&self) -> &'static str {
        "⚡"
    }

    fn about(&self) -> &'static str {
        "Download and upload speed, tested every few hours by Ookla's Speedtest"
    }

    fn action(&self, st: &Shared) -> Option<(String, Option<Ask>)> {
        if st.testing {
            return Some((format!("Testing… {}", test_left(st)), None));
        }
        if !ookla::ready(&st.prefs) {
            return Some(("Set up".into(), Some(Ask::Open(super::SETTINGS))));
        }
        let can = !matches!(st.status(), Status::Paused | Status::Down);
        Some(("Run test".into(), can.then_some(Ask::TestSpeed)))
    }

    fn card(&self, ui: &mut egui::Ui, st: &Shared) {
        let live = st.live.as_ref().filter(|_| st.testing);
        let last = st.speeds.iter().rev().find(|s| s.error.is_none());
        let (down, up) = match (live, last) {
            (Some(l), _) => (l.down_mbps.unwrap_or(0.0), l.up_mbps.unwrap_or(0.0)),
            (None, Some(s)) => (s.down, s.up),
            (None, None) => {
                let text = if ookla::ready(&st.prefs) { "No tests yet" } else { "Not set up yet" };
                ui.label(egui::RichText::new(text).color(pal().weak));
                return;
            }
        };
        speed_value(ui, down, up);
        // Each way against the plan, or against the faster way without one.
        let p = &st.prefs;
        let (of_down, of_up) = if p.plan_down > 0.0 && p.plan_up > 0.0 { (p.plan_down, p.plan_up) } else { (down.max(up), down.max(up)) };
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(vec2(width, 6.0), egui::Sense::hover());
        let half = (rect.width() - 10.0) / 2.0;
        let grow = look::appear(ui.ctx());
        let planned = p.plan_down > 0.0 && p.plan_up > 0.0;
        for (i, (v, of, color, way)) in [(down, of_down, pal().accent, "Download"), (up, of_up, charts::upload(), "Upload")].into_iter().enumerate() {
            let left = rect.left() + i as f32 * (half + 10.0);
            let track = egui::Rect::from_min_size(egui::pos2(left, rect.top()), vec2(half, 6.0));
            ui.painter().rect_filled(track, 3.0, pal().key);
            let k = (v / of.max(1.0)).clamp(0.03, 1.0) as f32 * grow;
            ui.painter().rect_filled(egui::Rect::from_min_size(track.min, vec2(half * k, 6.0)), 3.0, color);
            if super::pointing_at(ui, track.expand(6.0)) {
                let tip = if planned { format!("{way} {v:.0} of your plan's {of:.0} Mbps") } else { format!("{way} {v:.0} Mbps") };
                look::set_tip(ui.ctx(), tip);
            }
        }
        if planned && live.is_none() {
            let pct = |v: f64, of: f64| (100.0 * v / of).round();
            let text = format!("{:.0} % of plan down · {:.0} % up", pct(down, p.plan_down), pct(up, p.plan_up));
            ui.label(egui::RichText::new(text).small().color(pal().weak));
        }
        if let Some(f) = st.speeds.last().filter(|s| s.error.is_some() && !st.testing) {
            ui.label(egui::RichText::new(format!("Last try failed: {}", speed_error(f.error.as_deref().unwrap_or("")))).small().color(bad()));
        }
    }

    fn footer(&self, st: &Shared) -> String {
        let last = st.speeds.iter().rev().find(|s| s.error.is_none()).map(|s| format!("Last test {}", super::when_mid(s.t)));
        let next = st.next_speed.filter(|_| ookla::ready(&st.prefs)).map(|t| format!("next {}", super::when_mid(t)));
        match (last, next) {
            (Some(l), Some(n)) => format!("{l} · {n}"),
            (Some(l), None) => l,
            (None, Some(n)) => format!("Next test {}", &n[5..]),
            (None, None) => self.about().into(),
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, v: &mut View) {
        let st = v.st;
        let medium = ui.available_width() >= MEDIUM;
        let weak = |t: &str| egui::RichText::new(t).color(pal().weak);
        if !ookla::ready(&st.prefs) {
            look::card(ui, |ui| {
                ui.label(look::caption("Speed tests aren't set up"));
                ui.add(egui::Label::new(weak("They're run by Ookla's free Speedtest program. Read and accept its license in Settings, and Net Watch downloads it.")).wrap());
                if look::button(ui, "Set up in Settings", true, true).clicked() {
                    v.asks.push(Ask::Open(super::SETTINGS));
                }
            });
        }
        if let Some(live) = st.live.as_ref().filter(|_| st.testing) {
            live_card(ui, st, live, medium);
        } else if let Some(s) = st.speeds.iter().rev().find(|s| s.error.is_none()) {
            look::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(look::caption("Last speed test · Mbps"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can = ookla::ready(&st.prefs) && !matches!(st.status(), Status::Paused | Status::Down);
                        if look::button(ui, "⚡ Test again", false, can).clicked() {
                            v.asks.push(Ask::TestSpeed);
                        }
                    });
                });
                speed_value(ui, s.down, s.up);
                let mut under = format!("{} · ping {:.0} ms · {} used", when(s.t), s.ping, data(s.mb));
                if let Some(server) = &s.server {
                    under += &format!(" · {server}");
                }
                ui.add(egui::Label::new(egui::RichText::new(under).small().color(pal().weak)).wrap());
                if let Some(link) = &s.link {
                    ui.hyperlink_to(egui::RichText::new("This result on speedtest.net").small().underline().color(pal().accent), link);
                }
            });
        }
        let (name, days) = RANGES[*v.range];
        let from = Local::now() - Span::days(days);
        look::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(look::caption("Speed (Mbps) ·"));
                charts::legend(ui, pal().accent, "download");
                charts::legend(ui, charts::upload(), "upload");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| super::range_tabs(ui, v.range));
            });
            charts::speeds(ui, st, from);
        });
        history(ui, st, name, from, !medium);
    }
}

/// Every test in the period, newest first.
fn history(ui: &mut egui::Ui, st: &Shared, name: &str, from: crate::store::Time, narrow: bool) {
    let tests: Vec<_> = st.speeds.iter().rev().filter(|s| s.t >= from).collect();
    look::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(look::caption("Tests"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let n = if tests.len() == 1 { "1 test".into() } else { format!("{} tests", tests.len()) };
                ui.label(egui::RichText::new(format!("{n} in the last {name} · newest first")).small().color(pal().weak));
            });
        });
        if tests.is_empty() {
            ui.label(egui::RichText::new("No tests in this period.").color(pal().weak));
            return;
        }
        egui::Grid::new("tests").striped(true).num_columns(if narrow { 3 } else { 5 }).spacing(vec2(18.0, 8.0)).show(ui, |ui| {
            let head = |ui: &mut egui::Ui, t: &str| ui.label(egui::RichText::new(t).small().color(pal().weak));
            head(ui, "When");
            head(ui, "Down");
            head(ui, "Up");
            if !narrow {
                head(ui, "Ping");
                head(ui, "Server");
            }
            ui.end_row();
            for s in tests.iter().take(300) {
                ui.label(when(s.t));
                match &s.error {
                    Some(e) => {
                        tag(ui, "Failed", bad());
                        ui.label("");
                        if !narrow {
                            ui.label("");
                            ui.label(egui::RichText::new(speed_error(e)).color(pal().weak));
                        }
                    }
                    None => {
                        ui.label(format!("{:.0}", s.down));
                        ui.label(format!("{:.0}", s.up));
                        if !narrow {
                            ui.label(egui::RichText::new(format!("{:.0} ms", s.ping)).color(pal().weak));
                            let server = s.server.as_deref().unwrap_or("");
                            match &s.link {
                                Some(link) => ui.hyperlink_to(egui::RichText::new(server).color(pal().weak), link),
                                None => ui.label(egui::RichText::new(server).color(pal().weak)),
                            };
                        }
                    }
                }
                ui.end_row();
            }
        });
    });
}

/// A speed test as it runs: the speed now both ways, ping, how far along
/// it is, and a live graph.
fn live_card(ui: &mut egui::Ui, st: &Shared, live: &probe::Live, wide: bool) {
    let elapsed = st.test_ends.map_or(0.0, |end| live.secs - (end - Local::now()).num_milliseconds().max(0) as f32 / 1000.0);
    look::card(ui, |ui| {
        ui.horizontal(|ui| {
            let doing = match live.step {
                "download" => "downloading",
                "upload" => "uploading",
                "latency" => "measuring latency",
                _ => "starting",
            };
            ui.label(look::caption(&format!("Speed test · {doing}")));
            ui.spinner();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(format!("{} left", test_left(st))).strong());
            });
        });
        // Progress through the test, the two halves in their colors.
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(bar, 3.0, pal().key_hover);
        let k = (elapsed / live.secs.max(1.0)).clamp(0.0, 1.0);
        let split = live.upload_at.unwrap_or(live.secs / 2.0) / live.secs.max(1.0);
        let mid = bar.left() + bar.width() * split.clamp(0.0, 1.0);
        let x = bar.left() + bar.width() * k;
        p.rect_filled(egui::Rect::from_min_max(bar.min, egui::pos2(x.min(mid), bar.max.y)), 3.0, pal().accent);
        if x > mid {
            p.rect_filled(egui::Rect::from_min_max(egui::pos2(mid, bar.min.y), egui::pos2(x, bar.max.y)), 3.0, charts::upload());
        }

        // The speed now while that half runs, the result once it's done.
        let block = |ui: &mut egui::Ui, name: &str, samples: &[(f32, f32)], result: Option<f64>, running: bool, color: Color32| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(look::caption(name).color(color));
                // The speed now glides between readings, like a needle.
                let now = |now: f32| look::glide(ui.ctx(), egui::Id::new("live speed").with(name), now, 0.5);
                let (value, under) = match (running, samples.last(), result) {
                    (true, Some(&(_, n)), Some(r)) => (format!("{:.0}", now(n)), format!("now · result so far {r:.0}")),
                    (true, Some(&(_, n)), None) => (format!("{:.0}", now(n)), "now · warming up".into()),
                    (true, None, _) => ("…".into(), "connecting".into()),
                    (false, _, Some(r)) => (format!("{r:.0}"), "result".into()),
                    (false, _, None) => ("—".into(), if name == "Upload" { "next".into() } else { "waiting".into() }),
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(big(value, Some(if running || result.is_some() { pal().text } else { pal().weak })));
                    ui.label(egui::RichText::new("Mbps").small().color(pal().weak));
                });
                ui.label(egui::RichText::new(under).small().color(pal().weak));
            });
        };
        let ping = |ui: &mut egui::Ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(look::caption("Ping"));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(big(live.ping_ms.map_or("…".into(), |p| format!("{p:.0}")), None));
                    ui.label(egui::RichText::new("ms").small().color(pal().weak));
                });
                ui.label(egui::RichText::new(live.jitter_ms.map_or("measuring".into(), |j| format!("jitter {j:.1} ms"))).small().color(pal().weak));
            });
        };
        let cols = if wide { 3 } else { 2 };
        ui.columns(cols, |c| {
            block(&mut c[0], "Download", &live.down, live.down_mbps, live.step == "download", pal().accent);
            block(&mut c[1], "Upload", &live.up, live.up_mbps, live.step == "upload", charts::upload());
            if wide {
                ping(&mut c[2]);
            }
        });
        if !wide {
            ping(ui);
        }
        charts::live(ui, live, elapsed);
        let server = live.server.clone().unwrap_or_else(|| "finding a server…".into());
        let foot = format!("{} used · {server}", data(live.bytes as f64 / 1e6));
        ui.label(egui::RichText::new(foot).small().color(pal().weak));
    });
}
