//! What's using the network: which programs are talking to whose servers,
//! which wait for others to connect, and how fast data is moving. Looks
//! again every few seconds while it's open, and not at all otherwise.
use super::{Ask, Tool, View};
use crate::conns::{Proto, Snapshot, port_word};
use crate::look::{self, warn, pal};
use crate::monitor::Shared;
use crate::MEDIUM;
use eframe::egui::{self, vec2};
use std::time::Duration;

pub const NAME: &str = "What's using it?";
const EVERY: Duration = Duration::from_secs(3);

pub struct Apps {
    /// The program whose connections are showing.
    open: Option<String>,
}

impl Apps {
    pub fn new() -> Self {
        Apps { open: std::env::var("NET_WATCH_APP").ok() }
    }
}

/// Bytes a second as people read speeds: "1.2 Mbps", "80 kbps".
fn speed(bytes: f64) -> String {
    let bits = bytes * 8.0;
    match bits {
        b if b >= 1e6 => format!("{:.1} Mbps", b / 1e6),
        b if b >= 1e3 => format!("{:.0} kbps", b / 1e3),
        _ => "idle".into(),
    }
}

fn proto(p: Proto) -> &'static str {
    match p {
        Proto::Tcp => "TCP",
        Proto::Udp => "UDP",
    }
}

fn busiest(s: &Snapshot) -> Option<&crate::conns::App> {
    s.talking().find(|a| !a.other_user)
}

impl Tool for Apps {
    fn name(&self) -> &'static str {
        NAME
    }

    fn icon(&self) -> &'static str {
        "📡"
    }

    fn about(&self) -> &'static str {
        "Which programs on this computer are using the network, and whose servers they talk to"
    }

    fn action(&self, st: &Shared) -> Option<(String, Option<Ask>)> {
        Some(("Look".into(), (!st.looking).then_some(Ask::LookConns)))
    }

    fn card(&self, ui: &mut egui::Ui, st: &Shared) {
        let Some(s) = &st.conns else {
            ui.label(egui::RichText::new("Not looked yet").color(pal().weak));
            return;
        };
        let n = s.talking().count();
        super::dot_line(ui, pal().accent, format!("{n} program{} online", if n == 1 { "" } else { "s" }));
        if let Some(a) = busiest(s) {
            let owners: Vec<String> = a.owners().into_iter().take(2).map(|o| o.0).collect();
            ui.label(egui::RichText::new(format!("Busiest: {} → {}", a.name, owners.join(", "))).small().color(pal().weak));
        }
    }

    fn glance_width(&self) -> f32 {
        76.0
    }

    /// A bar per program, as long as its share of the connections.
    fn glance(&self, ui: &mut egui::Ui, st: &Shared) {
        let rect = super::glance_box(ui, vec2(76.0, 34.0), "connections");
        let counts: Vec<(String, usize)> = st.conns.as_ref().map_or_else(Vec::new, |s| s.talking().take(4).map(|a| (a.name.clone(), a.links.len())).collect());
        let most = counts.iter().map(|c| c.1).max().unwrap_or(1).max(1);
        for i in 0..4 {
            let y = rect.top() + 6.0 + i as f32 * 6.0;
            let full = egui::Rect::from_min_size(egui::pos2(rect.left() + 8.0, y), vec2(60.0, 4.0));
            ui.painter().rect_filled(full, 2.0, pal().key_hover);
            if let Some((name, n)) = counts.get(i) {
                let w = 60.0 * *n as f32 / most as f32 * look::appear(ui.ctx());
                ui.painter().rect_filled(egui::Rect::from_min_size(full.min, vec2(w.max(3.0), 4.0)), 2.0, pal().accent.gamma_multiply(1.0 - i as f32 * 0.2));
                if super::pointing_at(ui, full.expand2(vec2(2.0, 1.0))) {
                    look::set_tip(ui.ctx(), format!("{name}: {n} connection{}", if *n == 1 { "" } else { "s" }));
                }
            }
        }
    }

    fn footer(&self, st: &Shared) -> String {
        st.conns.as_ref().map_or(self.about().into(), |s| format!("Looked {}", super::when_mid(s.at)))
    }

    fn show(&mut self, ui: &mut egui::Ui, v: &mut View) {
        let st = v.st;
        let narrow = ui.available_width() < MEDIUM;
        let weak = |t: &str| egui::RichText::new(t).color(pal().weak);
        // Look again every few seconds while this is on screen.
        let stale = st.conns.as_ref().is_none_or(|s| (chrono::Local::now() - s.at).num_milliseconds() >= EVERY.as_millis() as i64);
        if stale && !st.looking {
            v.asks.push(Ask::LookConns);
        }
        ui.ctx().request_repaint_after(EVERY);

        look::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(look::caption("What's using the network"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new("Updates every 3 seconds").small().color(pal().weak));
                });
            });
            let Some(s) = &st.conns else {
                ui.label(weak("Looking…"));
                return;
            };
            // How fast data is moving right now.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 36.0;
                for (label, value) in [("Coming in", s.rate.map(|r| r.0)), ("Going out", s.rate.map(|r| r.1))] {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(egui::RichText::new(label).small().color(pal().weak));
                        ui.label(egui::RichText::new(value.map_or("…".into(), speed)).size(22.0));
                    });
                }
            });
            ui.add_space(6.0);

            ui.label(look::caption("Talking to the internet"));
            let talking: Vec<_> = s.talking().collect();
            if talking.is_empty() {
                ui.label(weak("No program has a connection open right now."));
            }
            for a in talking {
                let open = self.open.as_deref() == Some(a.name.as_str());
                let row = ui
                    .horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(egui::RichText::new(if open { "⏷" } else { "⏵" }).color(pal().weak));
                        ui.label(egui::RichText::new(&a.name).strong());
                        ui.label(weak(&format!("{} connection{}", a.links.len(), if a.links.len() == 1 { "" } else { "s" })));
                        let owners = a.owners();
                        let shown = if narrow { 2 } else { 4 };
                        for (o, _) in owners.iter().take(shown) {
                            super::pill(ui, o, pal().accent);
                        }
                        if owners.len() > shown {
                            ui.label(weak(&format!("+{} more", owners.len() - shown)));
                        }
                    })
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if row.clicked() {
                    self.open = if open { None } else { Some(a.name.clone()) };
                }
                if open {
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        egui::Grid::new(("links", &a.name)).num_columns(3).striped(true).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                            let mut links = a.links.clone();
                            links.sort_by(|x, y| x.owner.cmp(&y.owner).then(x.remote.port().cmp(&y.remote.port())));
                            for l in &links {
                                ui.label(l.owner.as_deref().unwrap_or("Unknown"));
                                ui.label(egui::RichText::new(l.remote.to_string()).monospace().size(12.0).color(pal().weak));
                                let what = port_word(l.proto, l.remote.port());
                                ui.label(weak(&if what.is_empty() { format!("{} {}", proto(l.proto), l.remote.port()) } else { what.to_string() }));
                                ui.end_row();
                            }
                        });
                    });
                }
            }
            ui.add_space(8.0);
            ui.separator();

            ui.label(look::caption("Waiting for others to connect"));
            let listening: Vec<_> = s.listening().collect();
            if listening.is_empty() {
                ui.label(weak("Nothing on this computer is waiting for connections from the network."));
            } else {
                ui.add(
                    egui::Label::new(weak(
                        "Other devices on your network can reach these if this computer's firewall lets them. That's normal for \
                         printing, finding devices and sharing; if you don't recognise one, close that program.",
                    ))
                    .wrap(),
                );
                egui::Grid::new("listens").num_columns(2).spacing(vec2(18.0, 6.0)).show(ui, |ui| {
                    for a in listening {
                        ui.label(egui::RichText::new(&a.name).strong());
                        ui.horizontal_wrapped(|ui| {
                            for l in &a.listens {
                                let what = port_word(l.proto, l.port);
                                let text = if what.is_empty() { format!("{} {}", proto(l.proto), l.port) } else { format!("{} {} · {what}", proto(l.proto), l.port) };
                                super::pill(ui, &text, if what.is_empty() { warn() } else { pal().weak });
                            }
                        });
                        ui.end_row();
                    }
                });
            }
            if s.apps.iter().any(|a| a.other_user) {
                ui.add_space(6.0);
                ui.add(egui::Label::new(egui::RichText::new(SYSTEM_NOTE).small().color(pal().weak)).wrap());
            }
        });
    }

    fn max_width(&self) -> f32 {
        900.0
    }
}

#[cfg(target_os = "linux")]
const SYSTEM_NOTE: &str = "\"System\" rows are programs run by the system or other users: Linux only shows which program they are to an admin.";
#[cfg(windows)]
const SYSTEM_NOTE: &str = "\"Windows services\" are the many parts of Windows that share one program (svchost) to run in the background.";
