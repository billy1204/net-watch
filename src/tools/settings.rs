//! Settings: how often to check and test, the plan, pop-ups, the look and
//! where the data is.
use super::{Ask, Tool, View};
use crate::look::{self, warn, pal};
use crate::monitor::Shared;
use crate::store::{self, Prefs};
use crate::{ookla, open_path, show_path, theme_index};
use eframe::egui::{self, Color32, vec2};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const NAME: &str = "Settings";

pub struct Settings {
    /// Settings as being edited; saved when they change.
    prefs: Prefs,
    /// Downloading Ookla's Speedtest: `None` before, then working or done.
    install: Arc<Mutex<Option<Install>>>,
}

#[derive(Clone)]
enum Install {
    Working,
    Done(Result<PathBuf, String>),
}

impl Settings {
    pub fn new(prefs: Prefs) -> Self {
        Self { prefs, install: Default::default() }
    }

    /// Under "Speed tests": what using Ookla means, accepting its license
    /// and getting the program; then that it's ready.
    fn ookla_setup(&self, ui: &mut egui::Ui, p: &mut Prefs) {
        let weak = |t: &str| egui::RichText::new(t).small().color(pal().weak);
        let program = ookla::program();
        let job = self.install.lock().unwrap().clone();
        ui.indent("ookla", |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            if p.ookla_accepted
                && let Some(path) = &program
            {
                ui.label(weak("Ready. Each test takes about 30–40 seconds, picks a server near you, and gets a page on speedtest.net."));
                ui.label(weak(&format!("Program: {}", path.display())));
                return;
            }
            ui.label(weak(
                "Ookla's free Speedtest program runs the test, against a server near you (often your provider's own). \
                 Its license allows personal, non-commercial use. Each result is sent to Ookla with your IP address \
                 and gets a page on speedtest.net you can share.",
            ));
            ui.horizontal(|ui| {
                ui.hyperlink_to(egui::RichText::new("License").small().underline().color(pal().accent), ookla::LICENSE_URL);
                ui.hyperlink_to(egui::RichText::new("Terms").small().underline().color(pal().accent), ookla::TERMS_URL);
                ui.hyperlink_to(egui::RichText::new("Privacy").small().underline().color(pal().accent), ookla::PRIVACY_URL);
            });
            let working = matches!(job, Some(Install::Working));
            ui.horizontal(|ui| {
                let label = match (p.ookla_accepted, &program) {
                    (false, None) => "I accept · download Speedtest",
                    (false, Some(_)) => "I accept",
                    (true, _) => "Download Speedtest",
                };
                if look::button(ui, label, true, !working).clicked() {
                    p.ookla_accepted = true;
                    if program.is_none() {
                        let (job, ctx) = (self.install.clone(), ui.ctx().clone());
                        *job.lock().unwrap() = Some(Install::Working);
                        std::thread::spawn(move || {
                            let done = ookla::install();
                            *job.lock().unwrap() = Some(Install::Done(done));
                            ctx.request_repaint();
                        });
                    }
                }
                match &job {
                    Some(Install::Working) => {
                        ui.spinner();
                        ui.label(weak("Downloading from install.speedtest.net…"));
                    }
                    Some(Install::Done(Err(e))) => {
                        ui.label(egui::RichText::new(format!("Couldn't get Speedtest: {e}")).small().color(warn()));
                    }
                    _ => {}
                }
            });
            ui.label(weak("Until then, there are no speed tests; connection checks carry on."));
        });
        ui.add_space(4.0);
    }
}

impl Tool for Settings {
    fn name(&self) -> &'static str {
        NAME
    }

    fn icon(&self) -> &'static str {
        "⚙"
    }

    fn about(&self) -> &'static str {
        "How often to check and test, your plan, pop-ups, the theme and your data"
    }

    fn icon_color(&self, _st: &Shared) -> Color32 {
        pal().weak
    }

    fn card(&self, ui: &mut egui::Ui, _st: &Shared) {
        ui.add(egui::Label::new(egui::RichText::new(self.about()).color(pal().weak)).wrap());
    }

    fn glance_width(&self) -> f32 {
        36.0
    }

    /// Two small switches: pop-ups and watching for devices, as they're set.
    fn glance(&self, ui: &mut egui::Ui, st: &Shared) {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            for (on, what) in [(st.prefs.popups, "Pop-ups"), (st.prefs.watch_devices, "Watching for new devices")] {
                let (rect, _) = ui.allocate_exact_size(vec2(32.0, 16.0), egui::Sense::hover());
                if super::pointing_at(ui, rect.expand(3.0)) {
                    look::set_tip(ui.ctx(), format!("{what}: {}", if on { "on" } else { "off" }));
                }
                ui.painter().rect_filled(rect, 8.0, if on { pal().accent } else { pal().key_hover });
                let x = if on { rect.right() - 8.0 } else { rect.left() + 8.0 };
                ui.painter().circle_filled(egui::pos2(x, rect.center().y), 6.0, Color32::WHITE);
            }
        });
    }

    fn footer(&self, st: &Shared) -> String {
        let p = &st.prefs;
        let speed = match p.speed_hours {
            0 => "no scheduled speed tests".into(),
            24 => "a speed test every day".into(),
            h => format!("a speed test every {h} h"),
        };
        format!("Checks every {} · {speed}", crate::span(p.check_secs as i64))
    }

    fn show(&mut self, ui: &mut egui::Ui, v: &mut View) {
        let weak = |t: &str| egui::RichText::new(t).small().color(pal().weak);
        let mut p = self.prefs.clone();
        let choice = |ui: &mut egui::Ui, id: &str, value: &mut u64, options: &[(u64, &str)]| {
            let mut i = options.iter().position(|o| o.0 == *value).unwrap_or(0);
            let labels: Vec<&str> = options.iter().map(|o| o.1).collect();
            if look::tabs(ui, id, &mut i, &labels) {
                *value = options[i].0;
            }
        };
        // A setting: its name and a line on what it does on the left, the
        // control on the right (or below, when narrow).
        let row = |ui: &mut egui::Ui, title: &str, sub: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
            let narrow = ui.available_width() < 560.0;
            let text = |ui: &mut egui::Ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(title);
                    if !sub.is_empty() {
                        ui.label(weak(sub));
                    }
                });
            };
            if narrow {
                text(ui);
                ui.horizontal_wrapped(|ui| add(ui));
            } else {
                ui.horizontal(|ui| {
                    text(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| add(ui));
                });
            }
            ui.add_space(4.0);
        };
        look::card(ui, |ui| {
            ui.label(egui::RichText::new("Checking").strong());
            row(ui, "Check the connection every", "Each check is tiny, well under 1 KB", &mut |ui| {
                choice(ui, "check", &mut p.check_secs, &[(15, "15 s"), (30, "30 s"), (60, "1 min"), (120, "2 min")]);
            });
            let mut hours = p.speed_hours as u64;
            row(ui, "Test speed every", "At a slightly random minute, only while online", &mut |ui| {
                choice(ui, "speed", &mut hours, &[(0, "Off"), (1, "1 h"), (2, "2 h"), (4, "4 h"), (6, "6 h"), (12, "12 h"), (24, "1 day")]);
            });
            p.speed_hours = hours as u32;
            row(ui, "Speed tests", "By Ookla's Speedtest (speedtest.net)", &mut |_| {});
            self.ookla_setup(ui, &mut p);
            row(ui, "Your plan's speed (Mbps)", "From your bill, for the graph's dashed lines; 0 if unsure", &mut |ui| {
                ui.add(egui::DragValue::new(&mut p.plan_up).range(0.0..=10_000.0).speed(1.0).suffix(" up"));
                ui.add(egui::DragValue::new(&mut p.plan_down).range(0.0..=10_000.0).speed(5.0).suffix(" down"));
            });
            row(ui, "Watch for new devices", "A pop-up when something new joins. Only on the network you're on when you turn this on", &mut |ui| {
                look::toggle(ui, &mut p.watch_devices);
            });
            if p.watch_devices {
                row(ui, "Scan for devices every", "Each scan takes about 4 seconds and stays on your network", &mut |ui| {
                    choice(ui, "device", &mut p.device_mins, &[(5, "5 min"), (15, "15 min"), (30, "30 min"), (60, "1 h")]);
                });
            }
        });
        look::card(ui, |ui| {
            ui.label(egui::RichText::new("Desktop").strong());
            row(ui, "Pop-ups", "When the internet drops and when it's back", &mut |ui| {
                look::toggle(ui, &mut p.popups);
            });
            let mut on = crate::tray::autostart();
            let mut failed = None;
            row(ui, "Start when I log in", "Starts hidden in the tray", &mut |ui| {
                if look::toggle(ui, &mut on).changed()
                    && let Err(e) = crate::tray::set_autostart(on)
                {
                    failed = Some(e);
                }
            });
            if let Some(e) = failed {
                v.asks.push(Ask::Note(format!("Couldn't change starting at login: {e}"), true, None));
            }
            row(ui, "Animations", "Buttons ripple, highlights slide, the status dot breathes", &mut |ui| {
                look::toggle(ui, &mut p.animate);
            });
            let mut theme = theme_index(&p.theme);
            row(ui, "Theme", look::THEMES[theme].0, &mut |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (i, (name, palette)) in look::THEMES.iter().enumerate().rev() {
                    if look::swatch(ui, palette, i == theme).on_hover_text(*name).clicked() {
                        theme = i;
                    }
                }
            });
            p.theme = look::THEMES[theme].0.into();
        });
        look::card(ui, |ui| {
            ui.label(egui::RichText::new("Your data").strong());
            let dir = store::data_dir();
            row(ui, "Saved on this computer only", &dir.as_deref().map(show_path).unwrap_or_default(), &mut |ui| {
                if let Some(dir) = &dir
                    && look::button(ui, "Open folder", false, true).clicked()
                {
                    let _ = std::fs::create_dir_all(dir);
                    open_path(dir);
                }
            });
            ui.add(
                egui::Label::new(weak(
                    "Checks go to Cloudflare (1.1.1.1), Google (8.8.8.8) and Quad9 (9.9.9.9). Speed tests are run by Ookla's \
                     Speedtest. Net Watch isn't made by or affiliated with any of them.",
                ))
                .wrap(),
            );
        });
        ui.horizontal(|ui| {
            ui.label(weak(&format!("Net Watch {}", env!("CARGO_PKG_VERSION"))));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if look::button(ui, "Quit Net Watch", false, true).on_hover_text("Stop watching the connection").clicked() {
                    v.asks.push(Ask::Quit);
                }
            });
        });

        if p != self.prefs {
            if p.theme != self.prefs.theme {
                look::set_theme(ui.ctx(), theme_index(&p.theme));
            }
            look::set_animated(p.animate);
            self.prefs = p.clone();
            v.asks.push(Ask::SetPrefs(p));
        }
    }
}
