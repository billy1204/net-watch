//! The window's tools. Home shows a card for each; opening one shows its
//! screen. A new tool is one more file here and one more line in `all()`.
mod connection;
mod devices;
mod diagnose;
mod dnscheck;
mod firewall;
mod lag;
pub mod network;
mod settings;
mod speed;

use crate::monitor::Shared;
use crate::store::Prefs;
use crate::look::pal;
use eframe::egui::{self, Color32, vec2};
use std::path::PathBuf;

/// Something a tool wants done once the shared state is unlocked: the core
/// takes the lock itself, so asking it from inside would deadlock.
pub enum Ask {
    CheckFirewall,
    /// Run the "What's wrong?" check.
    Diagnose,
    /// Trace the route to "Cloudflare", "Google" or a website someone typed.
    Trace(String),
    /// Run the DNS check.
    CheckDns,
    /// Open a web browser at a page a sign-in page will take over.
    SignIn,
    ScanDevices,
    TestSpeed,
    CheckNow,
    /// Open the tool with this name.
    Open(&'static str),
    /// A note under the status row: text, whether it's a problem, and a
    /// file it's about.
    Note(String, bool, Option<PathBuf>),
    SetPrefs(Prefs),
    /// Something for the network adapters.
    Net(crate::netif::Do),
    Quit,
}

/// What a tool gets to draw its screen with.
pub struct View<'a> {
    pub st: &'a Shared,
    /// The chosen period, an index into `RANGES`; the tools share it, and
    /// the report covers it.
    pub range: &'a mut usize,
    pub asks: &'a mut Vec<Ask>,
}

pub trait Tool {
    fn name(&self) -> &'static str;
    /// A symbol beside its name on its card.
    fn icon(&self) -> &'static str;
    /// The symbol's color: the accent, unless there's news.
    fn icon_color(&self, _st: &Shared) -> Color32 {
        pal().accent
    }
    /// What it's for, in a line.
    fn about(&self) -> &'static str;
    /// A small label beside its name on its card, like "1 new".
    fn badge(&self, _st: &Shared) -> Option<(String, Color32)> {
        None
    }
    /// A button on its card that acts without opening the tool, like "Run
    /// test"; no `Ask` while it can't.
    fn action(&self, _st: &Shared) -> Option<(String, Option<Ask>)> {
        None
    }
    /// How things stand, on the left of its card.
    fn card(&self, ui: &mut egui::Ui, st: &Shared);
    /// How wide `glance` is; 0 for none.
    fn glance_width(&self) -> f32 {
        0.0
    }
    /// A small picture on the right of its card.
    fn glance(&self, _ui: &mut egui::Ui, _st: &Shared) {}
    /// The line at the foot of its card.
    fn footer(&self, _st: &Shared) -> String {
        self.about().into()
    }
    fn show(&mut self, ui: &mut egui::Ui, view: &mut View);
    /// How wide its screen gets.
    fn max_width(&self) -> f32 {
        f32::INFINITY
    }
}

pub const SETTINGS: &str = settings::NAME;
pub const SPEED: &str = speed::NAME;
pub const NETWORK: &str = network::NAME;
pub const DIAGNOSE: &str = diagnose::NAME;

/// Every tool, in Home's order.
pub fn all(st: &Shared) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(connection::Connection),
        Box::new(diagnose::Diagnose::new()),
        Box::new(lag::Lag::new()),
        Box::new(dnscheck::DnsCheck::new()),
        Box::new(speed::Speed),
        Box::new(firewall::Firewall),
        Box::new(devices::Devices),
        Box::new(network::Network::new()),
        Box::new(settings::Settings::new(st.prefs.clone())),
    ]
}

/// The 24 h / 7 days / 30 days choice.
fn range_tabs(ui: &mut egui::Ui, range: &mut usize) {
    let labels: Vec<&str> = crate::RANGES.iter().map(|r| r.0).collect();
    crate::look::tabs(ui, "range", range, &labels);
}

/// A line of text with a dot of color before it, for a card.
fn dot_line(ui: &mut egui::Ui, color: Color32, text: impl Into<egui::WidgetText>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 3.5, color);
        ui.add(egui::Label::new(text).truncate());
    });
}

/// A rounded label in a tint of its color, like "7 outages".
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(12.5), color);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(18.0, 6.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, rect.height() / 2.0, color.gamma_multiply(if pal().light { 0.12 } else { 0.18 }));
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
}

/// Whether the pointer is over `rect`, for a card's tooltip.
fn pointing_at(ui: &egui::Ui, rect: egui::Rect) -> bool {
    ui.ctx().pointer_hover_pos().is_some_and(|p| rect.contains(p))
}

/// A box in the background color for a card's picture, with a caption
/// under it.
fn glance_box(ui: &mut egui::Ui, size: egui::Vec2, caption: &str) -> egui::Rect {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter().rect_filled(rect, 8.0, pal().bg);
        ui.painter().rect_stroke(rect, 8.0, egui::Stroke::new(1.0, pal().edge), egui::StrokeKind::Inside);
        if !caption.is_empty() {
            ui.label(egui::RichText::new(caption).size(10.5).color(pal().weak));
        }
        rect
    })
    .inner
}

/// `when` in the middle of a sentence: "last test today 2:14 pm".
fn when_mid(t: crate::store::Time) -> String {
    let w = crate::when(t);
    for day in ["Today", "Yesterday", "Tomorrow"] {
        if let Some(rest) = w.strip_prefix(day) {
            return format!("{}{rest}", day.to_lowercase());
        }
    }
    w
}
