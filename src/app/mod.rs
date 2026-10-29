//! The application window.

mod doc;
mod headmap;
mod navigator;
mod renderer;
mod sample;
mod scalp;
mod sidebar;
mod theme;
mod traces;
mod widgets;

use std::path::PathBuf;

use eframe::egui::{
    self, Align, Align2, CornerRadius, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, RichText, Sense, Stroke,
    StrokeKind, ThemePreference, ViewportCommand, pos2, vec2,
};
use oink_eeg::dsp::FilterSettings;
use oink_eeg::maps::Measure;
use oink_eeg::montage::MontageId;
use serde::{Deserialize, Serialize};

use doc::Doc;
use headmap::HeadMap;
use navigator::Navigator;
pub use sample::SAMPLES;
pub use sidebar::Tab;
use theme::Palette;
use traces::{Env, PAGES, SENSITIVITIES, fmt_page};
use widgets::Icon;

pub use renderer::TraceRenderer;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeChoice {
    System,
    Light,
    Dark,
}

/// Preferences remembered between sessions.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// Clinical convention (and the EDF+ spec): negative voltages are drawn upwards.
    pub negative_up: bool,
    pub hemisphere_colors: bool,
    pub montage: MontageId,
    pub filters: FilterSettings,
    /// µV per millimetre.
    pub sensitivity: f32,
    /// Seconds per page.
    pub page: f64,
    pub sidebar: bool,
    pub tab: Tab,
    /// Scalp map drawn on the sidebar's head instead of the montage's lines.
    pub map: Option<Measure>,
    /// Annotations drawn over the traces (bands and labels).
    pub show_annotations: bool,
    pub recent: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeChoice::System,
            negative_up: true,
            hemisphere_colors: true,
            montage: MontageId::AsRecorded,
            filters: FilterSettings::default(),
            sensitivity: 7.0,
            page: 10.0,
            sidebar: true,
            tab: Tab::Montage,
            map: None,
            show_annotations: true,
            recent: Vec::new(),
        }
    }
}

/// Options from the command line.
#[derive(Default)]
pub struct Launch {
    pub file: Option<PathBuf>,
    pub montage: Option<MontageId>,
    pub theme: Option<ThemeChoice>,
    pub at: Option<f64>,
    pub page: Option<f64>,
    pub sidebar: Option<bool>,
    pub tab: Option<Tab>,
    pub map: Option<Measure>,
    /// Open this built-in sample (an index into `SAMPLES`).
    pub sample: Option<usize>,
    /// Render a frame to this PNG and quit.
    pub screenshot: Option<PathBuf>,
    /// Open a menu at start ("montage", "filters", "settings", "map"), for screenshots.
    pub menu: Option<String>,
    /// An EDFbrowser montage file to load and show.
    pub montage_file: Option<PathBuf>,
    /// Start with this time range measured (seconds from the start of the recording).
    pub measure: Option<(f64, f64)>,
    /// Start with the channel with this label selected.
    pub select: Option<String>,
}

struct Shot {
    path: PathBuf,
    frames: u32,
    requested: bool,
}

pub struct App {
    settings: Settings,
    doc: Option<Doc>,
    error: Option<String>,
    navigator: Navigator,
    gpu: bool,
    points_per_mm: Option<f32>,
    help: bool,
    shot: Option<Shot>,
    title: String,
    open_menu: Option<String>,
}

const STORAGE_KEY: &str = "settings";

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, launch: Launch) -> Self {
        theme::install(&cc.egui_ctx);
        crate::platform::set_context(&cc.egui_ctx);
        let mut settings: Settings = if launch.screenshot.is_some() {
            Settings::default()
        } else {
            cc.storage.and_then(|s| eframe::get_value(s, STORAGE_KEY)).unwrap_or_default()
        };
        if let Some(t) = launch.theme {
            settings.theme = t;
        }
        if let Some(m) = launch.montage {
            settings.montage = m;
        }
        if let Some(p) = launch.page {
            settings.page = p;
        }
        if let Some(s) = launch.sidebar {
            settings.sidebar = s;
        }
        if let Some(t) = launch.tab {
            settings.tab = t;
        }
        if launch.map.is_some() {
            settings.map = launch.map;
        }
        let gpu = match &cc.wgpu_render_state {
            Some(rs) => {
                let renderer = TraceRenderer::new(&rs.device, rs.target_format, 1);
                rs.renderer.write().callback_resources.insert(renderer);
                true
            }
            None => false,
        };
        let mut app = App {
            settings,
            doc: None,
            error: None,
            navigator: Navigator::new(),
            gpu,
            points_per_mm: None,
            help: false,
            shot: launch.screenshot.map(|path| Shot { path, frames: 0, requested: false }),
            title: String::new(),
            open_menu: launch.menu,
        };
        if let Some(k) = launch.sample {
            app.open_sample(&cc.egui_ctx, k);
        } else if let Some(path) = launch.file {
            app.open(&cc.egui_ctx, path);
        }
        if let Some(doc) = &mut app.doc {
            // A montage named on the command line wins over a sample's own choice.
            if let Some(m) = launch.montage {
                doc.set_montage(m);
            }
            if let Some(f) = &launch.montage_file {
                let i = doc.load_montage_file(f);
                doc.set_montage_at(i);
            }
            if let Some(at) = launch.at {
                doc.t0 = doc.range.0 + at;
                doc.clamp_view(app.settings.page);
            }
            let start = doc.range.0;
            doc.measure = launch.measure.map(|(a, b)| (start + a, start + b));
            doc.selected_channel = launch.select.and_then(|l| doc.montage.channels.iter().position(|c| c.label.eq_ignore_ascii_case(&l.replace('-', "\u{2013}"))));
        }
        app
    }

    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        match Doc::open(&path, self.settings.montage, ctx) {
            Ok(doc) => {
                let duration = doc.range.1 - doc.range.0;
                if duration > 0.0 && self.settings.page > duration {
                    self.settings.page = PAGES.iter().copied().rev().find(|p| *p <= duration).unwrap_or(duration.max(0.25));
                }
                self.doc = Some(doc);
                self.error = None;
                let path = std::fs::canonicalize(&path).unwrap_or(path);
                self.settings.recent.retain(|p| p != &path);
                self.settings.recent.insert(0, path);
                self.settings.recent.truncate(8);
            }
            Err(e) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.error = Some(format!("Could not open {name}: {e}"));
            }
        }
    }

    fn open_sample(&mut self, ctx: &egui::Context, k: usize) {
        let sample = &SAMPLES[k];
        match Doc::open_sample(sample, sample.montage.unwrap_or(self.settings.montage), ctx) {
            Ok(doc) => {
                if let Some(v) = sample.sensitivity {
                    self.settings.sensitivity = v;
                }
                self.doc = Some(doc);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Could not open the sample: {e}")),
        }
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        let dialog = rfd::FileDialog::new()
            .set_title("Open EEG recording")
            .add_filter("EDF / BDF", &["edf", "bdf", "rec", "EDF", "BDF", "REC"])
            .add_filter("All files", &["*"]);
        if let Some(path) = dialog.pick_file() {
            self.open(ctx, path);
        }
    }

    /// Opens the popup of `resp` if it was asked for on the command line.
    fn maybe_open(&mut self, name: &str, resp: &egui::Response) {
        if self.open_menu.as_deref() == Some(name) {
            self.open_menu = None;
            egui::Popup::open_id(&resp.ctx, egui::Popup::default_response_id(resp));
        }
    }

    fn page(&self) -> f64 {
        self.settings.page
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let cmd = |k| KeyboardShortcut::new(Modifiers::COMMAND, k);
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::O))) {
            self.open_dialog(ctx);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::W))) {
            self.doc = None;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::I))) {
            self.settings.sidebar = !self.settings.sidebar;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::F))) {
            self.settings.sidebar = true;
            self.settings.tab = Tab::Events;
            ctx.memory_mut(|m| m.data.insert_temp(focus_search_id(), true));
        }
        if ctx.wants_keyboard_input() {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Questionmark) || i.consume_key(Modifiers::SHIFT, Key::Slash)) {
            self.help = !self.help;
        }
        let page = self.page();
        let Some(doc) = &mut self.doc else { return };
        let s = &mut self.settings;
        let step = if page >= 5.0 { 1.0 } else { page / 10.0 };
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::SHIFT, Key::ArrowRight) {
                doc.t0 += step;
            }
            if i.consume_key(Modifiers::SHIFT, Key::ArrowLeft) {
                doc.t0 -= step;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowRight) || i.consume_key(Modifiers::NONE, Key::PageDown) {
                doc.t0 += page;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowLeft) || i.consume_key(Modifiers::NONE, Key::PageUp) {
                doc.t0 -= page;
            }
            if i.consume_key(Modifiers::NONE, Key::Home) {
                doc.t0 = doc.range.0;
            }
            if i.consume_key(Modifiers::NONE, Key::End) {
                doc.t0 = doc.range.1;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                s.sensitivity = step_value(&SENSITIVITIES, s.sensitivity, false);
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                s.sensitivity = step_value(&SENSITIVITIES, s.sensitivity, true);
            }
            let zoom_in = i.consume_key(Modifiers::COMMAND, Key::Plus)
                || i.consume_key(Modifiers::COMMAND, Key::Equals)
                || i.consume_key(Modifiers::NONE, Key::Equals)
                || i.consume_key(Modifiers::NONE, Key::Plus);
            let zoom_out = i.consume_key(Modifiers::COMMAND, Key::Minus) || i.consume_key(Modifiers::NONE, Key::Minus);
            if zoom_in || zoom_out {
                let center = doc.t0 + s.page / 2.0;
                let next = step_value(&PAGES.map(|p| p as f32), s.page as f32, zoom_out) as f64;
                s.page = next.min(traces::max_page(doc));
                doc.t0 = center - s.page / 2.0;
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                doc.measure = None;
                doc.selected_channel = None;
                doc.selected_event = None;
            }
            for (k, key) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5].into_iter().enumerate() {
                if i.consume_key(Modifiers::NONE, key) && doc.set_montage(MontageId::ALL[k]) {
                    s.montage = MontageId::ALL[k];
                }
            }
        });
        let (prev, next) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::OpenBracket), i.consume_key(Modifiers::NONE, Key::CloseBracket)));
        if prev || next {
            doc.step_event(next, page);
        }
        doc.clamp_view(self.settings.page);
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let ctx = ui.ctx().clone();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if widgets::icon_button(ui, pal, Icon::Open, false, &format!("Open a file ({CMD}O)")).clicked() {
                self.open_dialog(&ctx);
            }
            ui.add_space(4.0);
            match &self.doc {
                Some(doc) => {
                    let f = &doc.file;
                    ui.label(RichText::new(f.file_name()).font(theme::font(13.5)));
                    if f.in_memory() {
                        ui.label(RichText::new("sample").color(pal.accent).small());
                    }
                    let rates: std::collections::BTreeSet<u64> = f.signals.iter().filter(|s| s.fs > 0.0).map(|s| s.fs.round() as u64).collect();
                    let rate = match (rates.first(), rates.last()) {
                        (Some(a), Some(b)) if a == b => format!("{a} Hz"),
                        (Some(a), Some(b)) => format!("{a}\u{2013}{b} Hz"),
                        _ => String::new(),
                    };
                    let meta = format!(
                        "{} \u{b7} {} signals \u{b7} {} \u{b7} {}",
                        f.format.name(),
                        f.signals.iter().filter(|s| !s.is_annotation).count(),
                        rate,
                        traces::fmt_clock(doc.range.1 - doc.range.0)
                    );
                    ui.add(egui::Label::new(RichText::new(meta).color(pal.muted).small()).truncate());
                }
                None => {
                    ui.label(RichText::new("Oink").color(pal.muted));
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let more = widgets::icon_button(ui, pal, Icon::More, false, "Display settings");
                self.maybe_open("settings", &more);
                egui::Popup::menu(&more).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(250.0).show(|ui| {
                    self.settings_menu(ui, pal);
                });
                if self.doc.is_none() {
                    return;
                }
                if widgets::icon_button(ui, pal, Icon::Sidebar, self.settings.sidebar, &format!("Sidebar ({CMD}I)")).clicked() {
                    self.settings.sidebar = !self.settings.sidebar;
                }
                ui.add_space(6.0);
                self.timebase_pill(ui, pal);
                self.sensitivity_pill(ui, pal);
                self.filters_pill(ui, pal);
                self.montage_pill(ui, pal);
            });
        });
    }

    fn montage_pill(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let Some(doc) = &mut self.doc else { return };
        let resp = widgets::pill(ui, pal, "Montage", &doc.montage_name().to_string(), false);
        if self.open_menu.as_deref() == Some("montage") {
            self.open_menu = None;
            egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&resp));
        }
        let mut chosen = None;
        let mut load = false;
        egui::Popup::menu(&resp).width(360.0).show(|ui| {
            for (k, entry) in doc.montages.iter().enumerate() {
                let (rect, row) = ui.allocate_exact_size(vec2(348.0, 58.0), Sense::click());
                let available = entry.montage.is_ok();
                let active = doc.current == Some(k);
                if active {
                    ui.painter().rect_filled(rect, CornerRadius::same(6), pal.accent_soft);
                } else if row.hovered() && available {
                    ui.painter().rect_filled(rect, CornerRadius::same(6), pal.hover);
                }
                let thumb = egui::Rect::from_min_size(rect.min + vec2(6.0, 4.0), vec2(50.0, 50.0));
                if let Ok(m) = &entry.montage {
                    HeadMap {
                        montage: m,
                        leads: &doc.leads,
                        visible: None,
                        highlight: &[],
                        highlight_electrode: None,
                        hemisphere_colors: self.settings.hemisphere_colors,
                        detailed: false,
                        scalp: None,
                    }
                    .draw(ui.painter(), thumb, pal, None);
                } else {
                    ui.painter().circle_stroke(thumb.center(), 50.0 * 0.5 / 1.2, Stroke::new(1.0f32, pal.head.gamma_multiply(0.5)));
                }
                let x = thumb.right() + 10.0;
                let name_color = if available { pal.text } else { pal.faint };
                let clip = egui::Rect::from_min_max(pos2(x, rect.top()), pos2(rect.right() - 22.0, rect.bottom()));
                let p = ui.painter().with_clip_rect(clip);
                p.text(pos2(x, rect.top() + 12.0), Align2::LEFT_TOP, &entry.name, theme::font(13.0), name_color);
                let detail = match &entry.montage {
                    Ok(m) => {
                        let eeg = m.channels.iter().filter(|c| c.kind == oink_eeg::leads::Kind::Eeg).count();
                        format!("{} \u{b7} {eeg} ch", entry.description)
                    }
                    Err(reason) => reason.clone(),
                };
                p.text(pos2(x, rect.top() + 31.0), Align2::LEFT_TOP, detail, theme::font(11.0), pal.muted);
                if entry.id != MontageId::Custom {
                    ui.painter().text(pos2(rect.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER, (k + 1).to_string(), theme::mono(11.0), pal.faint);
                }
                if row.clicked() && available {
                    chosen = Some(k);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Load EDFbrowser montage\u{2026}").clicked() {
                load = true;
                ui.close();
            }
        });
        if load {
            let pick = rfd::FileDialog::new().set_title("Load an EDFbrowser montage").add_filter("EDFbrowser montage", &["mtg"]).pick_file();
            if let Some(path) = pick {
                chosen = Some(doc.load_montage_file(&path));
            }
        }
        if let Some(k) = chosen {
            if doc.set_montage_at(k) && doc.montages[k].id != MontageId::Custom {
                self.settings.montage = doc.montages[k].id;
            }
        }
    }

    fn filters_pill(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let f = self.settings.filters;
        let hz = |v: Option<f32>| v.map(|v| traces::fmt_hz(v as f64)).unwrap_or_else(|| "off".into());
        let mut value = match (f.highpass, f.lowpass) {
            (None, None) => "Off".to_string(),
            (a, b) => format!("{}\u{2013}{} Hz", hz(a).replace("off", "DC"), hz(b).replace("off", "\u{221e}")),
        };
        if let Some(n) = f.notch {
            value += &format!(" \u{b7} {} Hz notch", traces::fmt_hz(n as f64));
        }
        let resp = widgets::pill(ui, pal, "Filters", &value, false);
        self.maybe_open("filters", &resp);
        egui::Popup::from_toggle_button_response(&resp)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .width(330.0)
            .show(|ui| {
                let f = &mut self.settings.filters;
                ui.label(RichText::new("Zero-phase Butterworth; cutoffs are \u{2212}3 dB points. Applied to EEG, EOG, EMG and ECG.").color(pal.muted).small());
                ui.add_space(6.0);
                ui.label("Low-frequency filter (high-pass)");
                widgets::segmented(ui, pal, &mut f.highpass, &[(None, "Off"), (Some(0.1), "0.1"), (Some(0.3), "0.3"), (Some(0.5), "0.5"), (Some(1.0), "1"), (Some(2.0), "2"), (Some(5.0), "5")]);
                ui.add_space(6.0);
                ui.label("High-frequency filter (low-pass)");
                widgets::segmented(ui, pal, &mut f.lowpass, &[(None, "Off"), (Some(15.0), "15"), (Some(30.0), "30"), (Some(35.0), "35"), (Some(40.0), "40"), (Some(70.0), "70"), (Some(100.0), "100")]);
                ui.add_space(6.0);
                ui.label("Notch");
                widgets::segmented(ui, pal, &mut f.notch, &[(None, "Off"), (Some(50.0), "50 Hz"), (Some(60.0), "60 Hz")]);
                ui.add_space(4.0);
            });
    }

    fn sensitivity_pill(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let value = format!("{} \u{b5}V/mm", traces::fmt_sensitivity(self.settings.sensitivity));
        let resp = widgets::pill(ui, pal, "Sensitivity", &value, false);
        egui::Popup::menu(&resp).width(220.0).show(|ui| {
            ui.label(RichText::new(format!("\u{2191} \u{2193} to step \u{b7} {ALT}scroll for fine control")).color(pal.muted).small());
            ui.add_space(4.0);
            egui::Grid::new("sens").num_columns(3).spacing(vec2(4.0, 4.0)).show(ui, |ui| {
                for (k, v) in SENSITIVITIES.iter().enumerate() {
                    let selected = (self.settings.sensitivity - v).abs() < 1e-3;
                    if ui.add_sized(vec2(64.0, 24.0), egui::Button::selectable(selected, format!("{v}"))).clicked() {
                        self.settings.sensitivity = *v;
                    }
                    if k % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
            if self.doc.as_ref().is_some_and(|d| d.gains.iter().any(|g| (g - 1.0).abs() > 0.01)) {
                ui.separator();
                if ui.button("Reset channel gains").clicked() {
                    if let Some(d) = &mut self.doc {
                        d.gains.iter_mut().for_each(|g| *g = 1.0);
                    }
                }
            }
            let ppm = self.points_per_mm.unwrap_or(96.0 / 25.4);
            let calibrated = crate::platform::points_per_mm(ui.ctx().pixels_per_point()).is_some();
            let note = if calibrated {
                "Millimetres are real millimetres on this display.".to_string()
            } else {
                format!("Display size unknown; assuming {:.0} dpi.", ppm * 25.4)
            };
            ui.label(RichText::new(note).color(pal.faint).small());
        });
    }

    fn timebase_pill(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let resp = widgets::pill(ui, pal, "Page", &fmt_page(self.settings.page), false);
        let max = self.doc.as_ref().map(traces::max_page).unwrap_or(f64::MAX);
        egui::Popup::menu(&resp).width(160.0).show(|ui| {
            ui.label(RichText::new(format!("{CMD}\u{2212} / {CMD}+ or pinch to zoom")).color(pal.muted).small());
            for p in PAGES.iter().copied().filter(|p| *p <= max * 1.0001) {
                let selected = (self.settings.page - p).abs() < 1e-6;
                if ui.add(egui::Button::selectable(selected, fmt_page(p))).clicked() {
                    if let Some(doc) = &mut self.doc {
                        let center = doc.t0 + self.settings.page / 2.0;
                        doc.t0 = center - p / 2.0;
                        doc.clamp_view(p);
                    }
                    self.settings.page = p;
                    ui.close();
                }
            }
        });
    }

    fn settings_menu(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let s = &mut self.settings;
        ui.label("Appearance");
        widgets::segmented(ui, pal, &mut s.theme, &[(ThemeChoice::System, "System"), (ThemeChoice::Light, "Light"), (ThemeChoice::Dark, "Dark")]);
        ui.add_space(6.0);
        ui.label("Polarity");
        widgets::segmented(ui, pal, &mut s.negative_up, &[(true, "Negative up"), (false, "Positive up")]);
        ui.label(RichText::new("Clinical EEG and the EDF+ spec draw negative up.").color(pal.faint).small());
        ui.add_space(6.0);
        ui.checkbox(&mut s.hemisphere_colors, "Colour by hemisphere");
        ui.separator();
        if ui.button("Keyboard shortcuts").clicked() {
            self.help = true;
            ui.close();
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let ctx = ui.ctx().clone();
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, pal.bg);
        let width = 420.0f32.min(full.width() - 40.0);
        let rect = egui::Rect::from_center_size(full.center() - vec2(0.0, 30.0), vec2(width, 360.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Center)));
        let ui = &mut child;
        // A small mark: three traces.
        let (mark, _) = ui.allocate_exact_size(vec2(64.0, 40.0), Sense::hover());
        for (k, color) in [pal.left, pal.trace, pal.right].into_iter().enumerate() {
            let y = mark.top() + 8.0 + k as f32 * 12.0;
            let pts: Vec<egui::Pos2> = (0..=32)
                .map(|i| {
                    let x = i as f32 / 32.0;
                    let v = (x * 19.0 + k as f32).sin() * 2.5 + (x * 41.0 + 2.0 * k as f32).sin() * 1.2;
                    pos2(mark.left() + x * mark.width(), y + v)
                })
                .collect();
            ui.painter().add(egui::Shape::line(pts, Stroke::new(1.4f32, color)));
        }
        ui.add_space(14.0);
        ui.label(RichText::new("Open an EEG recording").font(theme::font(17.0)));
        ui.label(RichText::new("EDF, EDF+, BDF or BDF+ \u{b7} drop a file anywhere").color(pal.muted));
        ui.add_space(14.0);
        let open = ui.add(egui::Button::new(RichText::new("Choose file\u{2026}").color(pal.bg)).fill(pal.accent).corner_radius(7).min_size(vec2(140.0, 30.0)));
        if open.clicked() {
            self.open_dialog(&ctx);
        }
        ui.label(RichText::new(format!("{CMD}O")).color(pal.faint).small());
        ui.add_space(16.0);
        ui.label(RichText::new("TRY A SAMPLE").font(theme::font(10.5)).color(pal.muted).extra_letter_spacing(0.6));
        ui.add_space(2.0);
        for (k, s) in SAMPLES.iter().enumerate() {
            if ui.add(egui::Button::new(RichText::new(s.title).color(pal.accent)).frame(false)).clicked() {
                self.open_sample(&ctx, k);
            }
            ui.label(RichText::new(s.description).color(pal.faint).small());
            ui.add_space(4.0);
        }
        if let Some(err) = &self.error {
            ui.add_space(10.0);
            ui.label(RichText::new(err).color(pal.warn));
        }
        let recent: Vec<PathBuf> = self.settings.recent.iter().filter(|p| p.exists()).cloned().collect();
        if !recent.is_empty() {
            ui.add_space(22.0);
            ui.label(RichText::new("RECENT").font(theme::font(10.5)).color(pal.muted).extra_letter_spacing(0.6));
            ui.add_space(4.0);
            let mut chosen = None;
            for path in recent.iter().take(6) {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let folder = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                let (rect, resp) = ui.allocate_exact_size(vec2(width, 38.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(6), pal.hover);
                }
                ui.painter().text(rect.left_top() + vec2(10.0, 5.0), Align2::LEFT_TOP, &name, theme::font(13.0), pal.text);
                let clip = rect.shrink2(vec2(10.0, 0.0));
                ui.painter().with_clip_rect(clip).text(rect.left_top() + vec2(10.0, 22.0), Align2::LEFT_TOP, folder, theme::font(11.0), pal.faint);
                if resp.clicked() {
                    chosen = Some(path.clone());
                }
            }
            if let Some(p) = chosen {
                self.open(&ctx, p);
            }
        }
    }

    fn help_sheet(&mut self, ctx: &egui::Context, pal: &Palette) {
        let modal = egui::Modal::new(egui::Id::new("help")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("Keyboard and mouse").font(theme::font(15.0)));
            ui.add_space(8.0);
            let rows = [
                ("\u{2190} \u{2192}  PgUp PgDn".to_string(), "Previous / next page"),
                (format!("{SHIFT}\u{2190} \u{2192}"), "Step one second"),
                ("Scroll, swipe".to_string(), "Move through time (or scroll channels)"),
                ("Drag the overview".to_string(), "Jump anywhere in the recording"),
                (format!("{CMD}scroll, pinch, {CMD}\u{2212} {CMD}+"), "Zoom time"),
                ("\u{2191} \u{2193}".to_string(), "Sensitivity"),
                (format!("{ALT}scroll"), "Sensitivity; over a label, that channel only"),
                ("Drag in the traces".to_string(), "Measure time, frequency and peak-to-peak"),
                ("Click a label".to_string(), "Select a channel (shown on the head)"),
                ("[  ]".to_string(), "Previous / next event"),
                ("1 \u{2013} 5".to_string(), "Montage"),
                ("Home  End".to_string(), "Start / end of the recording"),
                (format!("{CMD}O  {CMD}W"), "Open / close a file"),
                (format!("{CMD}I  {CMD}F"), "Sidebar / find an event"),
                ("Esc".to_string(), "Clear measurement and selection"),
            ];
            egui::Grid::new("keys").num_columns(2).spacing(vec2(18.0, 6.0)).show(ui, |ui| {
                for (k, v) in rows {
                    ui.label(RichText::new(k).font(theme::mono(12.0)).color(pal.muted));
                    ui.label(v);
                    ui.end_row();
                }
            });
        });
        if modal.should_close() {
            self.help = false;
        }
    }

    fn handle_shot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        shot.frames += 1;
        ctx.request_repaint();
        if !shot.requested {
            let ready = match &self.doc {
                Some(d) => {
                    d.traces().is_some()
                        && !d.loading(self.settings.page)
                        && d.annotations_ready
                        && d.overview.as_ref().is_none_or(|o| o.done)
                        && !d.map_pending()
                }
                None => true,
            };
            if (ready && shot.frames > 20) || shot.frames > 1200 {
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
                shot.requested = true;
            }
            return;
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            if let Err(e) = save_png(&shot.path, &image) {
                eprintln!("could not write {}: {e}", shot.path.display());
            }
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.set_theme(match self.settings.theme {
            ThemeChoice::System => ThemePreference::System,
            ThemeChoice::Light => ThemePreference::Light,
            ThemeChoice::Dark => ThemePreference::Dark,
        });
        if self.points_per_mm.is_none() {
            self.points_per_mm = Some(crate::platform::points_per_mm(ctx.pixels_per_point()).unwrap_or(96.0 / 25.4));
        }
        for path in crate::platform::take_opened_files() {
            self.open(ctx, path);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if let Some(path) = dropped.into_iter().next() {
            self.open(ctx, path);
        }
        if let Some(doc) = &mut self.doc {
            doc.poll();
            doc.hover_channel = None;
        }
        self.handle_keys(ctx);

        let title = match &self.doc {
            Some(d) => format!("{} \u{2014} Oink", d.file.file_name()),
            None => "Oink".to_string(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        let pal = theme::palette(ctx);
        let bar = Frame::new().fill(pal.chrome).inner_margin(Margin::symmetric(10, 0));
        egui::TopBottomPanel::top("toolbar").exact_height(42.0).frame(bar).show_separator_line(true).show(ctx, |ui| {
            self.toolbar(ui, pal);
        });
        if let Some(doc) = &mut self.doc {
            let page = self.settings.page;
            egui::TopBottomPanel::bottom("navigator")
                .exact_height(66.0)
                .frame(Frame::new().fill(pal.chrome))
                .show(ctx, |ui| self.navigator.ui(ui, doc, page));
            if self.settings.sidebar {
                egui::SidePanel::right("sidebar")
                    .resizable(true)
                    .default_width(300.0)
                    .width_range(250.0..=480.0)
                    .frame(Frame::new().fill(pal.chrome).inner_margin(Margin { left: 14, right: 12, top: 0, bottom: 0 }))
                    .show(ctx, |ui| sidebar::ui(ui, doc, &mut self.settings, &mut self.open_menu));
            }
        }
        egui::CentralPanel::default().frame(Frame::new().fill(pal.bg)).show(ctx, |ui| {
            if let Some(doc) = &mut self.doc {
                let env = Env { points_per_mm: self.points_per_mm.unwrap_or(96.0 / 25.4), gpu: self.gpu };
                traces::ui(ui, doc, &mut self.settings, &env);
            } else {
                self.empty_state(ui, pal);
            }
        });

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let screen = ctx.content_rect();
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
            p.rect_filled(screen, 0.0, pal.bg.gamma_multiply(0.85));
            let r = screen.shrink(24.0);
            p.rect_stroke(r, CornerRadius::same(12), Stroke::new(2.0f32, pal.accent), StrokeKind::Inside);
            p.text(r.center(), Align2::CENTER_CENTER, "Drop to open", theme::font(18.0), pal.accent);
        }
        if self.help {
            self.help_sheet(ctx, pal);
        }
        self.handle_shot(ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.shot.is_none() {
            eframe::set_value(storage, STORAGE_KEY, &self.settings);
        }
    }

    fn persist_egui_memory(&self) -> bool {
        self.shot.is_none()
    }
}

/// Modifier key names as the platform writes them.
pub const CMD: &str = if cfg!(target_os = "macos") { "\u{2318}" } else { "Ctrl+" };
pub const ALT: &str = if cfg!(target_os = "macos") { "\u{2325}" } else { "Alt+" };
pub const SHIFT: &str = if cfg!(target_os = "macos") { "\u{21e7}" } else { "Shift+" };

/// Id of the flag that asks the events tab to focus its search field.
pub fn focus_search_id() -> egui::Id {
    egui::Id::new("focus-event-search")
}

fn step_value(values: &[f32], current: f32, up: bool) -> f32 {
    if up {
        values.iter().copied().find(|v| *v > current * 1.0001).unwrap_or(*values.last().unwrap())
    } else {
        values.iter().rev().copied().find(|v| *v < current * 0.9999).unwrap_or(values[0])
    }
}

fn save_png(path: &std::path::Path, image: &egui::ColorImage) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, image.size[0] as u32, image.size[1] as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    writer.write_image_data(&bytes)?;
    Ok(())
}
