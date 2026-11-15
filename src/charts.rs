//! The graphs: the hour-by-hour grid, latency over time and the speed tests.
use crate::look::{self, bad, good, warn, pal};
use crate::monitor::Shared;
use crate::store::{CheckRow, Outage, Time};
use chrono::{Duration as Span, Local, TimeZone, Timelike};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, pos2, vec2};

pub fn upload() -> Color32 {
    pal().second
}

/// Seconds of `outages` inside `from..to`.
pub fn offline_secs<'a>(outages: impl Iterator<Item = &'a Outage>, from: Time, to: Time, now: Time) -> i64 {
    outages
        .map(|o| {
            let (s, e) = (o.start.max(from), o.end.unwrap_or(now).min(to));
            (e - s).num_seconds().max(0)
        })
        .sum()
}

/// The checks with times in `from..to`.
pub fn checks_in(checks: &[CheckRow], from: Time, to: Time) -> &[CheckRow] {
    let a = checks.partition_point(|c| c.t < from);
    let b = checks.partition_point(|c| c.t < to);
    &checks[a..b.max(a)]
}

/// Seconds the app was watching in `from..to`: the gaps between checks,
/// skipping long ones (app closed or computer asleep).
pub fn watched_secs(checks: &[CheckRow], max_gap: i64) -> i64 {
    checks.windows(2).map(|w| (w[1].t - w[0].t).num_seconds().clamp(0, max_gap)).sum::<i64>() + if checks.is_empty() { 0 } else { 30 }
}

fn hour_start(t: Time) -> Time {
    let naive = t.date_naive().and_hms_opt(t.hour(), 0, 0).unwrap();
    Local.from_local_datetime(&naive).earliest().unwrap_or(t)
}

/// An hour's color: green all fine, amber a short drop or failed checks,
/// red 5+ minutes offline; `None` when not watching then.
pub fn hour_color(st: &Shared, from: Time, now: Time) -> Option<Color32> {
    let to = (from + Span::hours(1)).min(now);
    let checks = checks_in(&st.checks, from, to);
    let off = offline_secs(st.outages_since(from), from, to, now);
    let failed = checks.iter().filter(|c| c.ok < c.total).count();
    if from > now || (checks.is_empty() && off == 0) {
        None
    } else if off >= 300 {
        Some(bad())
    } else if off > 0 || failed * 10 > checks.len() {
        Some(warn())
    } else {
        Some(good().gamma_multiply(0.85))
    }
}

/// The last 24 hours, newest last: when each started and its color.
pub fn last_hours(st: &Shared) -> Vec<(Time, Option<Color32>)> {
    let now = Local::now();
    let first = hour_start(now) - Span::hours(23);
    (0..24).map(|h| first + Span::hours(h)).map(|t| (t, hour_color(st, t, now))).collect()
}

/// One cell per hour, a row per day (or one row for the last 24 hours).
/// Green: all fine; amber: a short drop or failed checks; red: 5+ minutes
/// offline; dark: not watching then.
pub fn hour_grid(ui: &mut egui::Ui, st: &Shared, days: usize) {
    let now = Local::now();
    let label_w = 46.0;
    let rows = days.max(1);
    let gap = 3.0;
    let width = ui.available_width();
    let cell_w = ((width - label_w) / 24.0).floor();
    let cell_h = if rows > 10 { 7.0 } else { 14.0 };
    let row_h = cell_h + if rows > 10 { 2.0 } else { 4.0 };
    let height = rows as f32 * row_h + 18.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let p = ui.painter_at(rect);
    let font = FontId::proportional(11.0);
    let today = now.date_naive();
    let mut hover: Option<String> = None;
    for r in 0..rows {
        // With one row: the 24 hours up to now; otherwise whole days.
        let (first, label) = if rows == 1 {
            (hour_start(now) - Span::hours(23), String::new())
        } else {
            let day = today - chrono::Days::new((rows - 1 - r) as u64);
            let start = Local.from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap()).earliest().unwrap_or(now);
            let label = if day == today {
                "Today".into()
            } else if rows > 10 {
                if r % 3 == 0 && r + 1 < rows { day.format("%b %-d").to_string() } else { String::new() }
            } else {
                day.format("%a").to_string()
            };
            (start, label)
        };
        let y = rect.top() + r as f32 * row_h;
        if !label.is_empty() {
            p.text(pos2(rect.left(), y + cell_h / 2.0), egui::Align2::LEFT_CENTER, label, font.clone(), pal().weak);
        }
        for h in 0..24 {
            let from = first + Span::hours(h);
            let to = from + Span::hours(1);
            let cell = Rect::from_min_size(pos2(rect.left() + label_w + h as f32 * cell_w, y), vec2(cell_w - gap, cell_h));
            let checks = checks_in(&st.checks, from, to.min(now));
            let off = offline_secs(st.outages_since(from), from, to.min(now), now);
            let failed = checks.iter().filter(|c| c.ok < c.total).count();
            let color = hour_color(st, from, now).unwrap_or(pal().key);
            p.rect_filled(cell, 3.0, color);
            if resp.hover_pos().is_some_and(|pos| cell.expand(gap / 2.0).contains(pos)) {
                p.rect_stroke(cell, 3.0, Stroke::new(1.5, pal().text), egui::StrokeKind::Outside);
                let what = if from > now {
                    "Still to come".to_string()
                } else if checks.is_empty() && off == 0 {
                    "Not watching (app closed or computer asleep)".into()
                } else if off > 0 {
                    format!("Offline {}", crate::span(off))
                } else if failed > 0 {
                    format!("Online · {failed} of {} checks had trouble", checks.len())
                } else {
                    format!("Online · {} checks, all fine", checks.len())
                };
                hover = Some(format!("{} · {}–{}\n{what}", crate::day_word(from), from.format("%-I %P"), to.format("%-I %P")));
            }
        }
    }
    let y = rect.top() + rows as f32 * row_h + 8.0;
    for h in [0, 6, 12, 18] {
        let label = if rows == 1 {
            (hour_start(now) - Span::hours(23) + Span::hours(h)).format("%-I %P").to_string()
        } else {
            ["12 am", "6 am", "12 pm", "6 pm"][h as usize / 6].to_string()
        };
        p.text(pos2(rect.left() + label_w + h as f32 * cell_w, y), egui::Align2::LEFT_CENTER, label, font.clone(), pal().weak);
    }
    if let Some(text) = hover {
        resp.on_hover_text_at_pointer(text);
    }
}

/// A round number at or above `v` for a graph's top (1, 2 or 5 × 10ⁿ).
fn nice_max(v: f64) -> f64 {
    let v = v.max(1.0);
    let mag = 10f64.powf(v.log10().floor());
    [1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0].iter().map(|m| m * mag).find(|n| *n >= v).unwrap_or(10.0 * mag)
}

/// The frame of a time graph: grid lines with labels on the left, times
/// along the bottom. Returns the plotting area and the painter.
fn axes(ui: &mut egui::Ui, height: f32, from: Time, to: Time, max: f64, ticks: &[f64], sense: Sense) -> (Rect, egui::Painter, egui::Response) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);
    let p = ui.painter_at(rect);
    let plot = Rect::from_min_max(pos2(rect.left() + 34.0, rect.top() + 6.0), pos2(rect.right() - 4.0, rect.bottom() - 20.0));
    let font = FontId::proportional(11.0);
    for v in ticks {
        let y = plot.bottom() - (*v / max) as f32 * plot.height();
        p.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, pal().text.gamma_multiply(0.07)));
        p.text(pos2(rect.left(), y), egui::Align2::LEFT_CENTER, format!("{v}"), font.clone(), pal().weak);
    }
    let span = (to - from).num_seconds() as f64;
    let n = 4;
    for i in 0..=n {
        let t = from + Span::seconds((span * i as f64 / n as f64) as i64);
        let label = if i == n && (Local::now() - to).num_seconds() < 60 {
            "now".to_string()
        } else if span <= 3.0 * 3600.0 {
            t.format("%-I:%M %P").to_string()
        } else if span <= 86_400.0 * 1.5 {
            t.format("%-I %P").to_string()
        } else {
            t.format("%b %-d").to_string()
        };
        let x = plot.left() + plot.width() * i as f32 / n as f32;
        let align = if i == n { egui::Align2::RIGHT_CENTER } else if i == 0 { egui::Align2::LEFT_CENTER } else { egui::Align2::CENTER_CENTER };
        p.text(pos2(x, rect.bottom() - 8.0), align, label, font.clone(), pal().weak);
    }
    (plot, p, resp)
}

fn x_of(plot: Rect, from: Time, to: Time, t: Time) -> f32 {
    let f = (t - from).num_seconds() as f32 / (to - from).num_seconds().max(1) as f32;
    plot.left() + f.clamp(0.0, 1.0) * plot.width()
}

/// Latency over time, averaged per pixel column, with the lowest and
/// highest check in each column as a faint band (so a short spike shows at
/// any zoom) and a tick at the top where a check went off the scale;
/// outages are shaded red. The mouse wheel (or a pinch) zooms in
/// around the pointer, dragging moves along, and a double-click shows the
/// whole period again. The scale stays that of the whole period, so the
/// line keeps its height when zoomed.
pub fn latency(ui: &mut egui::Ui, st: &Shared, from: Time) {
    let now = Local::now();
    // The zoomed view as seconds before now, kept per period shown. A view
    // that reaches now follows it; one in the past stays put (`at` is when
    // it was set, so it can be held still as time passes).
    let id = ui.id().with(("latency zoom", (now - from).num_hours()));
    let full = (now - from).num_seconds().max(60) as f64;
    let (mut ago_from, mut ago_to, at) = ui.data(|d| d.get_temp::<(f64, f64, i64)>(id)).unwrap_or((full, 0.0, now.timestamp()));
    if ago_to > 1.0 {
        let passed = (now.timestamp() - at) as f64;
        ago_from = (ago_from + passed).min(full);
        ago_to = (ago_to + passed).min(ago_from - 600.0f64.min(full)).max(0.0);
    }
    let (view_from, view_to) = (now - Span::seconds(ago_from as i64), now - Span::seconds(ago_to as i64));

    let mut all: Vec<f32> = checks_in(&st.checks, from, now + Span::seconds(1)).iter().filter_map(|c| c.latency_ms).collect();
    all.sort_by(f32::total_cmp);
    let checks = checks_in(&st.checks, view_from, view_to + Span::seconds(1));
    let lat: Vec<f32> = checks.iter().filter_map(|c| c.latency_ms).collect();
    let p95 = all.get(all.len() * 95 / 100).copied().unwrap_or(50.0) as f64;
    let max = nice_max((p95 * 1.4).clamp(40.0, 2000.0));
    let ticks = [0.0, max / 2.0, max];
    let (plot, p, resp) = axes(ui, 170.0, view_from, view_to, max, &ticks, Sense::click_and_drag());
    for o in st.outages_since(view_from) {
        let (a, b) = (x_of(plot, view_from, view_to, o.start), x_of(plot, view_from, view_to, o.end.unwrap_or(now)));
        p.rect_filled(Rect::from_x_y_ranges(a..=(b.max(a + 2.0)), plot.y_range()), 0.0, bad().gamma_multiply(0.28));
    }
    // Average per column; keep each column's first and last check time so
    // the line breaks only where checks really stopped (app closed, asleep).
    let cols = (plot.width() / 2.0).max(2.0) as usize;
    let mut sums: Vec<(f32, u32, Option<(Time, Time)>)> = vec![(0.0, 0, None); cols];
    let mut range = vec![(f32::MAX, 0f32); cols];
    for c in checks {
        if let Some(l) = c.latency_ms {
            let i = ((((x_of(plot, view_from, view_to, c.t) - plot.left()) / plot.width()) * (cols - 1) as f32) as usize).min(cols - 1);
            let col = &mut sums[i];
            col.0 += l;
            col.1 += 1;
            col.2 = Some(col.2.map_or((c.t, c.t), |(a, _)| (a, c.t)));
            range[i] = (range[i].0.min(l), range[i].1.max(l));
        }
    }
    let y_of = |v: f64| plot.bottom() - (v.min(max) / max) as f32 * plot.height();
    let col_w = plot.width() / (cols - 1) as f32;
    for (i, (lo, hi)) in range.iter().enumerate() {
        let x = plot.left() + col_w * i as f32;
        let xs = (x - col_w / 2.0)..=(x + col_w / 2.0);
        // Every check shows, so zooming doesn't change what's there; a check
        // off the scale is cut at the top and gets a tick there.
        let (band_lo, band_hi) = (*lo as f64, (*hi as f64).min(max));
        if band_hi > band_lo {
            p.rect_filled(Rect::from_x_y_ranges(xs.clone(), y_of(band_hi)..=y_of(band_lo)), 0.0, pal().accent.gamma_multiply(0.22));
        }
        if *hi as f64 > max {
            p.rect_filled(Rect::from_x_y_ranges(xs, plot.top()..=(plot.top() + 3.0)), 0.0, pal().accent.gamma_multiply(0.7));
        }
    }
    let max_gap = st.prefs.check_secs as i64 * 3 + 30;
    let mut line: Vec<Pos2> = Vec::new();
    let mut last_t: Option<Time> = None;
    let stroke = Stroke::new(1.6, pal().accent);
    for (i, (sum, n, times)) in sums.iter().enumerate() {
        let Some((first, last)) = times else { continue };
        if last_t.is_some_and(|t| (*first - t).num_seconds() > max_gap) && !line.is_empty() {
            draw_line(&p, std::mem::take(&mut line), stroke);
        }
        last_t = Some(*last);
        let v = (sum / *n as f32) as f64;
        line.push(pos2(plot.left() + col_w * i as f32, y_of(v)));
    }
    draw_line(&p, line, stroke);
    if lat.is_empty() {
        p.text(plot.center(), egui::Align2::CENTER_CENTER, "No checks here", FontId::proportional(13.0), pal().weak);
    }
    let zoomed = ago_from < full - 1.0 || ago_to > 1.0;
    let hint = if zoomed { "Double-click to see it all" } else { "Scroll to zoom" };
    if resp.hovered() || zoomed {
        p.text(pos2(plot.right() - 4.0, plot.top() + 2.0), egui::Align2::RIGHT_TOP, hint, FontId::proportional(10.0), pal().weak);
    }

    // Zoom and move. Times are in seconds before now; the view stays inside
    // the period and is at least ten minutes long.
    let span = ago_from - ago_to;
    let secs_per_px = span / plot.width() as f64;
    let mut changed = false;
    if resp.hovered() {
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = (-(scroll as f64) * 0.003).exp() / pinch as f64;
        if (factor - 1.0).abs() > 1e-4
            && let Some(pos) = resp.hover_pos()
        {
            // Keep the time under the pointer where it is.
            let k = ((pos.x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64;
            let at = ago_from - k * span;
            let new_span = (span * factor).clamp(600.0, full);
            ago_from = at + k * new_span;
            ago_to = ago_from - new_span;
            changed = true;
            // The wheel zooms here instead of scrolling the page.
            ui.ctx().input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
    }
    if resp.dragged() {
        let dx = resp.drag_delta().x as f64 * secs_per_px;
        ago_from += dx;
        ago_to += dx;
        changed = true;
    }
    if resp.double_clicked() {
        (ago_from, ago_to) = (full, 0.0);
        changed = true;
    }
    let following = ago_to <= 1.0;
    if !changed && !following {
        ui.data_mut(|d| d.insert_temp(id, (ago_from, ago_to, now.timestamp())));
    }
    if changed {
        let span = (ago_from - ago_to).clamp(600.0f64.min(full), full);
        ago_to = ago_to.clamp(0.0, full - span);
        ago_from = ago_to + span;
        ui.data_mut(|d| d.insert_temp(id, (ago_from, ago_to, now.timestamp())));
        ui.ctx().request_repaint();
    }
    if resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if resp.hovered() && zoomed {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }

    if let Some(pos) = resp.hover_pos()
        && plot.contains(pos)
        && !resp.dragged()
    {
        let k = (pos.x - plot.left()) / plot.width();
        let i = (k * (cols - 1) as f32).round() as i64;
        let t = view_from + Span::seconds((k as f64 * (view_to - view_from).num_seconds() as f64) as i64);
        // The nearest column with checks, a few pixels either way.
        let near = (0..=3i64).flat_map(|d| [i - d, i + d]).filter_map(|j| usize::try_from(j).ok()).find(|&j| sums.get(j).is_some_and(|c| c.1 > 0));
        let text = match near {
            Some(j) if sums[j].1 > 1 => {
                let (s, n, _) = sums[j];
                format!("{} · {:.0} ms average of {n} checks ({:.0}–{:.0})", crate::when(t), s / n as f32, range[j].0, range[j].1)
            }
            Some(j) => format!("{} · {:.0} ms", crate::when(t), sums[j].0),
            None => format!("{} · no answer", crate::when(t)),
        };
        p.line_segment([pos2(pos.x, plot.top()), pos2(pos.x, plot.bottom())], Stroke::new(1.0, pal().weak.gamma_multiply(0.5)));
        resp.on_hover_text_at_pointer(text);
    }
}

/// A line through `points`; a lone point (a short stretch of checks) as a dot.
fn draw_line(p: &egui::Painter, points: Vec<Pos2>, stroke: Stroke) {
    match points.len() {
        0 => {}
        1 => {
            p.circle_filled(points[0], 2.0, stroke.color);
        }
        _ => {
            p.add(Shape::line(points, stroke));
        }
    }
}

/// Each speed test as a dot, download and upload, with the plan's speeds
/// as dashed lines.
pub fn speeds(ui: &mut egui::Ui, st: &Shared, from: Time) {
    let now = Local::now();
    let tests: Vec<_> = st.speeds.iter().filter(|s| s.t >= from && s.error.is_none()).collect();
    let top = tests.iter().map(|s| s.down.max(s.up)).fold(st.prefs.plan_down.max(st.prefs.plan_up), f64::max);
    let max = nice_max(top * 1.15);
    let ticks = [0.0, max / 2.0, max];
    let (plot, p, resp) = axes(ui, 170.0, from, now, max, &ticks, Sense::hover());
    let y_of = |v: f64| plot.bottom() - (v.min(max) / max) as f32 * plot.height();
    for (plan, color) in [(st.prefs.plan_down, pal().accent), (st.prefs.plan_up, upload())] {
        if plan > 0.0 {
            let y = y_of(plan);
            p.extend(Shape::dashed_line(&[pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, color.gamma_multiply(0.5)), 5.0, 4.0));
        }
    }
    if st.prefs.plan_down > 0.0 {
        p.text(pos2(plot.right(), y_of(st.prefs.plan_down) - 7.0), egui::Align2::RIGHT_CENTER, "your plan", FontId::proportional(10.0), pal().weak);
    }
    let mut nearest: Option<(f32, String)> = None;
    for (get, color) in [((|s: &&crate::store::SpeedRow| s.down) as fn(&&crate::store::SpeedRow) -> f64, pal().accent), (|s| s.up, upload())] {
        let pts: Vec<Pos2> = tests.iter().map(|s| pos2(x_of(plot, from, now, s.t), y_of(get(s)))).collect();
        if pts.len() > 1 {
            p.add(Shape::line(pts.clone(), Stroke::new(1.2, color.gamma_multiply(0.55))));
        }
        for pt in &pts {
            p.circle_filled(*pt, 3.0, color);
        }
    }
    // Failed tests: a red mark on the bottom line.
    let failures: Vec<_> = st.speeds.iter().filter(|s| s.t >= from && s.error.is_some()).collect();
    for f in &failures {
        let c = pos2(x_of(plot, from, now, f.t), plot.bottom() - 4.0);
        let d = 3.5;
        for (a, b) in [(vec2(-d, -d), vec2(d, d)), (vec2(-d, d), vec2(d, -d))] {
            p.line_segment([c + a, c + b], Stroke::new(1.8, bad()));
        }
    }
    if let Some(pos) = resp.hover_pos() {
        for f in &failures {
            let d = (x_of(plot, from, now, f.t) - pos.x).abs();
            if d < 12.0 && nearest.as_ref().is_none_or(|(n, _)| d < *n) {
                let why = crate::speed_error(f.error.as_deref().unwrap_or(""));
                nearest = Some((d, format!("{}\nTest failed: {why}", crate::when(f.t))));
            }
        }
        for s in &tests {
            let d = (x_of(plot, from, now, s.t) - pos.x).abs();
            if d < 12.0 && nearest.as_ref().is_none_or(|(n, _)| d < *n) {
                let by = s.server.as_deref().map(|v| format!("\n{v}")).unwrap_or_default();
                nearest = Some((d, format!("{}\n{:.0} down / {:.0} up Mbps · ping {:.0} ms{by}", crate::when(s.t), s.down, s.up, s.ping)));
            }
        }
    }
    if tests.is_empty() {
        let next = if failures.is_empty() { "First test at" } else { "Next try at" };
        let text = match st.next_speed {
            _ if !crate::ookla::ready(&st.prefs) => "Set up speed tests in Settings".into(),
            Some(t) => format!("{next} {}", t.format("%-I:%M %P")),
            None => "Speed tests are off".into(),
        };
        p.text(plot.center(), egui::Align2::CENTER_CENTER, text, FontId::proportional(13.0), pal().weak);
    }
    if let Some((_, text)) = nearest {
        resp.on_hover_text_at_pointer(text);
    }
}

/// A running speed test: its live speed both ways over the test's length,
/// with the halves (download, then upload) marked.
pub fn live(ui: &mut egui::Ui, live: &crate::probe::Live, elapsed: f32) {
    let top = live.down.iter().chain(&live.up).map(|s| s.1 as f64).fold(100.0, f64::max);
    let max = nice_max(top * 1.15);
    let (resp, p) = ui.allocate_painter(vec2(ui.available_width(), 150.0), Sense::hover());
    let plot = Rect::from_min_max(resp.rect.min + vec2(44.0, 6.0), resp.rect.max - vec2(4.0, 18.0));
    let x_of = |t: f32| plot.left() + (t / live.secs.max(1.0)).clamp(0.0, 1.0) * plot.width();
    let y_of = |v: f64| plot.bottom() - (v.min(max) / max) as f32 * plot.height();
    let font = FontId::proportional(10.0);
    for v in [0.0, max / 2.0, max] {
        let y = y_of(v);
        p.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, pal().key_hover));
        p.text(pos2(plot.left() - 6.0, y), egui::Align2::RIGHT_CENTER, format!("{v:.0}"), font.clone(), pal().weak);
    }
    let half = x_of(live.upload_at.unwrap_or(live.secs / 2.0));
    p.extend(Shape::dashed_line(&[pos2(half, plot.top()), pos2(half, plot.bottom())], Stroke::new(1.0, pal().weak.gamma_multiply(0.6)), 4.0, 4.0));
    for (text, x, color) in [("download", plot.left(), pal().accent), ("upload", half, upload())] {
        p.text(pos2(x + 6.0, plot.bottom() + 4.0), egui::Align2::LEFT_TOP, text, font.clone(), color);
    }
    p.text(pos2(plot.right(), plot.bottom() + 4.0), egui::Align2::RIGHT_TOP, "Mbps over the test", font.clone(), pal().weak);
    // Where the test is now.
    if elapsed < live.secs {
        let x = x_of(elapsed);
        p.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(1.0, pal().text.gamma_multiply(0.35)));
    }
    for (samples, color) in [(&live.down, pal().accent), (&live.up, upload())] {
        let pts: Vec<Pos2> = samples.iter().map(|&(t, v)| pos2(x_of(t), y_of(v as f64))).collect();
        draw_line(&p, pts.clone(), Stroke::new(1.6, color));
        if let Some(last) = pts.last() {
            p.circle_filled(*last, 3.5, color);
        }
    }
}

/// A legend dot and label.
pub fn legend(ui: &mut egui::Ui, color: Color32, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
    ui.label(look::caption(text).color(color));
}
