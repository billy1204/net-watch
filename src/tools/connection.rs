//! Is the internet up: uptime, the hour-by-hour grid, latency and the
//! outage log.
use super::{Tool, View};
use crate::look::{self, bad, good, warn, pal};
use crate::monitor::{Shared, Status};
use crate::store::{self, Cause, Time};
use crate::{MEDIUM, RANGES, WIDE, big, charts, span, tag, uptime, watched_off, when};
use chrono::{Duration as Span, Local};
use eframe::egui::{self, Color32, vec2};

pub struct Connection;

impl Tool for Connection {
    fn name(&self) -> &'static str {
        "Connection"
    }

    fn about(&self) -> &'static str {
        "Uptime, the hour grid, latency and the outage log"
    }

    fn icon(&self) -> &'static str {
        "●"
    }

    fn icon_color(&self, st: &Shared) -> Color32 {
        match st.status() {
            Status::Up => good(),
            Status::Shaky => warn(),
            Status::Down => bad(),
            Status::Starting | Status::Paused => pal().weak,
        }
    }

    fn card(&self, ui: &mut egui::Ui, st: &Shared) {
        let from = Local::now() - Span::days(7);
        let (watched, off) = watched_off(st, from);
        let n = st.outages_since(from).count();
        ui.horizontal(|ui| {
            if watched == 0 {
                ui.label(egui::RichText::new("No checks yet this week").color(pal().weak));
                return;
            }
            ui.label(egui::RichText::new(format!("{:.1} % up this week", uptime(watched, off))).strong());
            match n {
                0 => super::pill(ui, "no outages", good()),
                1 => super::pill(ui, "1 outage", warn()),
                n => super::pill(ui, &format!("{n} outages"), warn()),
            }
        });
        let last = st.open.as_ref().or(st.outages.last());
        let text = match last {
            Some(o) if o.end.is_none() => format!("Down since {}", super::when_mid(o.start)),
            Some(o) => format!("Last outage {}, {}", super::when_mid(o.start), span(o.secs(Local::now()))),
            None => "No outages recorded".into(),
        };
        ui.label(egui::RichText::new(text).small().color(pal().weak));
    }

    fn glance_width(&self) -> f32 {
        112.0
    }

    /// A bar for each of the last 24 hours, shorter when there was trouble.
    fn glance(&self, ui: &mut egui::Ui, st: &Shared) {
        let rect = super::glance_box(ui, vec2(112.0, 30.0), "last 24 hours");
        let inner = rect.shrink2(vec2(7.0, 5.0));
        let w = inner.width() / 24.0;
        let grow = look::appear(ui.ctx());
        for (h, (from, color)) in charts::last_hours(st).into_iter().enumerate() {
            let (color, k, what) = match color {
                None => (pal().key_hover, 0.3, "not watching"),
                Some(c) if c == bad() => (c, 0.45, "offline 5 minutes or more"),
                Some(c) if c == warn() => (c, 0.65, "a short drop or some failed checks"),
                Some(c) => (c, 1.0, "all fine"),
            };
            let x = inner.left() + w * (h as f32 + 0.5);
            let top = inner.bottom() - inner.height() * k * grow;
            ui.painter().line_segment([egui::pos2(x, top), egui::pos2(x, inner.bottom())], egui::Stroke::new((w - 1.5).max(1.5), color));
            let column = egui::Rect::from_x_y_ranges(x - w / 2.0..=x + w / 2.0, inner.y_range());
            if super::pointing_at(ui, column) {
                let to = from + Span::hours(1);
                look::set_tip(ui.ctx(), format!("{}–{}: {what}", from.format("%-I %P"), to.format("%-I %P")));
            }
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, v: &mut View) {
        let st = v.st;
        let w = ui.available_width();
        let (wide, medium) = (w >= WIDE, w >= MEDIUM);
        tiles(ui, st, *v.range, if wide { 4 } else { 2 });
        let (name, days) = RANGES[*v.range];
        look::card(ui, |ui| {
            ui.horizontal(|ui| {
                let title = match (days, medium) {
                    (_, false) => "Connection",
                    (1, _) => "Connection · a bar per hour",
                    _ => "Connection · a row per day, a bar per hour",
                };
                ui.label(look::caption(title));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| super::range_tabs(ui, v.range));
            });
            charts::hour_grid(ui, st, days as usize);
        });
        let from = Local::now() - Span::days(days);
        look::card(ui, |ui| {
            ui.label(look::caption(&format!("Latency (ms) · last {name} · red = offline")));
            charts::latency(ui, st, from);
        });
        outage_log(ui, st, *v.range, !medium);
    }
}

fn tiles(ui: &mut egui::Ui, st: &Shared, range: usize, cols: usize) {
    let now = Local::now();
    let (range_name, days) = RANGES[range];
    let from = now - Span::days(days);
    let checks = charts::checks_in(&st.checks, from, now + Span::seconds(1));
    let (watched, off) = watched_off(st, from);
    let outages: Vec<_> = st.outages_since(from).collect();
    let longest = outages.iter().max_by_key(|o| o.secs(now));
    let uptime = uptime(watched, off);
    let sent: u32 = checks.iter().map(|c| c.total as u32).sum();
    let lost: u32 = checks.iter().map(|c| (c.total - c.ok) as u32).sum();
    let recent: Vec<f32> = checks.iter().rev().take(10).filter_map(|c| c.latency_ms).collect();
    let jitter = if recent.len() > 1 { recent.windows(2).map(|w| (w[0] - w[1]).abs()).sum::<f32>() / (recent.len() - 1) as f32 } else { 0.0 };
    let hour = charts::checks_in(&st.checks, now - Span::hours(1), now + Span::seconds(1));
    let failed: u32 = hour.iter().map(|c| (c.total - c.ok) as u32).sum();
    let total = hour.iter().map(|c| c.total as u32).sum::<u32>().max(1);

    // Title, the big value, the line under it.
    let tiles: [(String, egui::RichText, String); 4] = [
        (
            format!("Uptime · last {range_name}"),
            big(if watched == 0 { "—".into() } else { format!("{uptime:.1} %") }, None),
            if watched == 0 {
                "No checks yet".into()
            } else if off == 0 {
                format!("no outages in {} watched", span(watched))
            } else {
                format!("{} offline in total", span(off))
            },
        ),
        (
            format!("Outages · last {range_name}"),
            big(outages.len().to_string(), (!outages.is_empty()).then_some(bad())),
            longest.map_or("None so far".into(), |o| format!("longest {} · {}", span(o.secs(now)), when(o.start))),
        ),
        (
            format!("Checks · last {range_name}"),
            big(sent.to_string(), None),
            format!("every {} · {} failed", span(st.prefs.check_secs as i64), lost),
        ),
        (
            "Latency now".into(),
            match st.last.as_ref().and_then(|(_, c)| c.latency) {
                Some(l) => big(format!("{:.0} ms", l.as_secs_f64() * 1000.0), None),
                None => big("—".into(), None),
            },
            format!("jitter {jitter:.0} ms · {:.0} % of checks failed last hour", 100.0 * failed as f64 / total as f64),
        ),
    ];

    for row in tiles.chunks(cols) {
        ui.columns(cols, |columns| {
            for ((title, value, sub), ui) in row.iter().zip(columns) {
                look::card(ui, |ui| {
                    ui.set_min_height(76.0);
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(look::caption(title));
                    ui.label(value.clone());
                    ui.add(egui::Label::new(egui::RichText::new(sub).small().color(pal().weak)).truncate());
                });
            }
        });
    }
}

fn outage_log(ui: &mut egui::Ui, st: &Shared, range: usize, narrow: bool) {
    let now = Local::now();
    let (range_name, days) = RANGES[range];
    let from = now - Span::days(days);
    // Outages, and speed tests far below the plan, newest first.
    enum Row<'a> {
        Out(&'a store::Outage),
        Slow(&'a store::SpeedRow),
    }
    let mut rows: Vec<(Time, Row)> = st.outages_since(from).map(|o| (o.start, Row::Out(o))).collect();
    let outages = rows.len();
    if st.prefs.plan_down > 0.0 {
        rows.extend(
            st.speeds
                .iter()
                .filter(|s| s.t >= from && s.error.is_none() && s.down < st.prefs.plan_down * 0.25)
                .map(|s| (s.t, Row::Slow(s))),
        );
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    look::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(look::caption("Outage log"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let n = if outages == 1 { "1 outage".into() } else { format!("{outages} outages") };
                ui.label(egui::RichText::new(format!("{n} in the last {range_name} · newest first")).small().color(pal().weak));
            });
        });
        if rows.is_empty() {
            ui.label(egui::RichText::new("No outages. If the connection drops, it shows up here with when and for how long.").color(pal().weak));
            return;
        }
        egui::Grid::new("log").striped(true).num_columns(if narrow { 3 } else { 5 }).spacing(vec2(18.0, 8.0)).show(ui, |ui| {
            let head = |ui: &mut egui::Ui, t: &str| ui.label(egui::RichText::new(t).small().color(pal().weak));
            head(ui, "Started");
            if !narrow {
                head(ui, "Back at");
            }
            head(ui, "Lasted");
            head(ui, "What happened");
            if !narrow {
                head(ui, "Details");
            }
            ui.end_row();
            for (_, row) in rows.iter().take(300) {
                match row {
                    Row::Out(o) => {
                        ui.label(when(o.start));
                        if !narrow {
                            ui.label(o.end.map_or("still down".into(), |e| {
                                if e.date_naive() == o.start.date_naive() { e.format("%-I:%M %P").to_string() } else { when(e) }
                            }));
                        }
                        ui.label(span(o.secs(now)));
                        let (word, short, color) = match o.cause {
                            Cause::Provider => ("Provider down", "Provider", bad()),
                            Cause::Local => ("Wi-Fi / router", "Wi-Fi", warn()),
                            Cause::Unknown => ("No internet", "Down", bad()),
                        };
                        tag(ui, if narrow { short } else { word }, color);
                        if !narrow {
                            ui.label(egui::RichText::new(&o.detail).color(pal().weak));
                        }
                    }
                    Row::Slow(s) => {
                        ui.label(when(s.t));
                        if !narrow {
                            ui.label("");
                        }
                        ui.label("—");
                        tag(ui, if narrow { "Slow" } else { "Very slow" }, charts::upload());
                        if !narrow {
                            let text = format!("Up, but download {:.0} Mbps (plan {:.0})", s.down, st.prefs.plan_down);
                            ui.label(egui::RichText::new(text).color(pal().weak));
                        }
                    }
                }
                ui.end_row();
            }
        });
    });
}
