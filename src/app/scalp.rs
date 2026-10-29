//! Scalp maps in the sidebar: band power or a band ratio for each scalp electrode on the
//! average reference, interpolated over the head and drawn in place of the montage's lines.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align2, Color32, ColorImage, Mesh, Rect, RichText, Sense, TextureHandle, TextureOptions, pos2, vec2};
use oink_eeg::edf::EdfFile;
use oink_eeg::leads::ElectrodeId;
use oink_eeg::maps::{self, Interpolator, Measure, Spectra};
use oink_eeg::montage::Montage;

use super::theme::{self, Palette};
use super::widgets;

/// Size of the map texture; the head disc samples it with linear filtering.
const RES: usize = 128;
/// Contour lines, evenly spaced between the lowest and highest electrode values.
const CONTOURS: usize = 5;

/// What a map was asked for: the measure and the span in seconds.
pub type MapKey = (Measure, f64, f64);
/// A finished request: the map, or why there is none.
pub type MapResult = (MapKey, Result<ScalpMap, String>);

/// A computed map, ready to draw.
pub struct ScalpMap {
    pub measure: Measure,
    pub t0: f64,
    pub t1: f64,
    /// Each electrode's value: a percentage of 1-45 Hz power, or a power ratio.
    pub values: Vec<(ElectrodeId, f64)>,
    /// Ends of the colour scale: the lowest and highest electrode values.
    pub lo: f64,
    pub hi: f64,
    /// The interpolated map over [-1, 1] x [-1, 1] in head-map units.
    pub texture: TextureHandle,
    /// Contour lines in head-map units.
    pub contours: Vec<[[f32; 2]; 2]>,
    pub skipped: usize,
    pub sampled: bool,
}

impl ScalpMap {
    pub fn value(&self, e: ElectrodeId) -> Option<f64> {
        self.values.iter().find(|(x, _)| *x == e).map(|(_, v)| *v)
    }

    pub fn format(&self, v: f64) -> String {
        let s = two_digits(v);
        if self.measure.is_ratio() { s } else { s + "%" }
    }

    /// What the colours mean.
    pub fn units(&self) -> String {
        let m = self.measure;
        if m.is_ratio() {
            format!("{} power ratio, log scale", m.detail())
        } else {
            format!("{} {}, % of 1\u{2013}45 Hz power", m.label(), m.detail())
        }
    }
}

/// Two significant digits: 0.042, 0.42, 4.2, 42.
fn two_digits(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return "0".into();
    }
    let decimals = (1 - v.abs().log10().floor() as i32).max(0) as usize;
    format!("{v:.decimals$}")
}

/// Computes maps on a background thread. The spectra of the last span are kept, so switching
/// measures needs no new reading. A running computation finishes before the newest request
/// starts, which keeps the map updating while the view scrolls.
pub struct MapWorker {
    tx: Sender<MapKey>,
    done: Arc<Mutex<Option<MapResult>>>,
    requested: Option<MapKey>,
}

impl MapWorker {
    pub fn new(file: Arc<EdfFile>, avg: Arc<Montage>, ctx: egui::Context, cancel: Arc<AtomicBool>) -> MapWorker {
        let (tx, rx) = mpsc::channel::<MapKey>();
        let done = Arc::new(Mutex::new(None));
        let out = done.clone();
        std::thread::Builder::new()
            .name("scalp maps".into())
            .spawn(move || {
                let points: Vec<(f32, f32)> =
                    maps::electrodes(&avg).map(|c| c.a.expect("electrode").info()).map(|e| (e.x, e.y)).collect();
                let interp = Interpolator::new(&points);
                let cancelled = || cancel.load(Ordering::Relaxed);
                let mut cached: Option<((f64, f64), Spectra)> = None;
                while let Ok(mut key) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        key = newer;
                    }
                    let (measure, t0, t1) = key;
                    if cached.as_ref().is_none_or(|(span, _)| *span != (t0, t1)) {
                        cached = match maps::band_powers(&file, &avg, t0, t1, &cancelled) {
                            Ok(Some(s)) => Some(((t0, t1), s)),
                            Ok(None) => return,
                            Err(e) => {
                                *out.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, Err(format!("Could not read the file: {e}"))));
                                ctx.request_repaint();
                                continue;
                            }
                        };
                    }
                    let Some((_, spectra)) = &cached else { continue };
                    let map = build(measure, t0, t1, spectra, interp.as_ref(), &points, &ctx);
                    *out.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, map));
                    ctx.request_repaint();
                }
            })
            .expect("spawn map worker");
        MapWorker { tx, done, requested: None }
    }

    /// Asks for a map, unless it is the one last asked for.
    pub fn request(&mut self, key: MapKey) {
        if self.requested != Some(key) {
            self.requested = Some(key);
            let _ = self.tx.send(key);
        }
    }

    pub fn requested(&self) -> Option<MapKey> {
        self.requested
    }

    pub fn take(&self) -> Option<MapResult> {
        self.done.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

fn build(
    measure: Measure,
    t0: f64,
    t1: f64,
    s: &Spectra,
    interp: Option<&Interpolator>,
    points: &[(f32, f32)],
    ctx: &egui::Context,
) -> Result<ScalpMap, String> {
    if s.segments == 0 {
        return Err(if t1 - t0 < maps::MIN_SPAN {
            "A map needs at least 1 s: widen the page or the selection."
        } else {
            "No data here: a gap in the recording."
        }
        .into());
    }
    let defined: Vec<(usize, f64)> = s.powers.iter().enumerate().filter_map(|(i, p)| Some((i, measure.value(p)?))).collect();
    if defined.len() < 4 {
        return Err(format!("{} cannot be computed for this recording.", measure.label()));
    }
    // Ratios are mapped on a log scale, so that halving and doubling look alike.
    let ratio = measure.is_ratio();
    let shown: Vec<f64> = defined.iter().map(|&(_, v)| if ratio { v.max(1e-12).ln() } else { v }).collect();
    let subset;
    let interp = match interp {
        Some(it) if defined.len() == s.powers.len() => it,
        _ => {
            subset = Interpolator::new(&defined.iter().map(|&(i, _)| points[i]).collect::<Vec<_>>())
                .ok_or("The electrode positions cannot be mapped.")?;
            &subset
        }
    };
    let (lo, hi) = shown.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
    let range = (hi - lo).max(1e-9);

    // Two texels beyond the head outline, so linear filtering never fades its edge.
    let grid = interp.grid(&shown, RES, 1.0 + 4.0 / RES as f64);
    let pixels = grid
        .iter()
        .map(|&g| if g.is_finite() { viridis(((g as f64 - lo) / range) as f32) } else { Color32::TRANSPARENT })
        .collect();
    let image = ColorImage { size: [RES, RES], pixels, source_size: vec2(RES as f32, RES as f32) };
    let mut contours = Vec::new();
    for k in 1..=CONTOURS {
        maps::isolines(&grid, RES, (lo + range * k as f64 / (CONTOURS + 1) as f64) as f32, &mut contours);
    }
    contours.retain(|s| s.iter().all(|p| p[0].hypot(p[1]) <= 1.0));

    let unscale = |v: f64| if ratio { v.exp() } else { v };
    Ok(ScalpMap {
        measure,
        t0,
        t1,
        values: defined.iter().map(|&(i, v)| (s.powers[i].electrode, v)).collect(),
        lo: unscale(lo),
        hi: unscale(hi),
        texture: ctx.load_texture("scalp map", image, TextureOptions::LINEAR),
        contours,
        skipped: s.skipped,
        sampled: s.sampled,
    })
}

/// Viridis, a perceptually uniform colour map that reads in greyscale and with colour-blindness,
/// sampled at eleven points.
const VIRIDIS: [u32; 11] =
    [0x440154, 0x482475, 0x414487, 0x355F8D, 0x2A788E, 0x21918C, 0x22A884, 0x44BF70, 0x7AD151, 0xBDDF26, 0xFDE725];

pub fn viridis(t: f32) -> Color32 {
    let x = t.clamp(0.0, 1.0) * 10.0;
    let i = (x as usize).min(9);
    let f = x - i as f32;
    let channel = |shift: u32| {
        let (a, b) = (((VIRIDIS[i] >> shift) & 0xFF) as f32, ((VIRIDIS[i + 1] >> shift) & 0xFF) as f32);
        (a + (b - a) * f).round() as u8
    };
    Color32::from_rgb(channel(16), channel(8), channel(0))
}

/// The "Map" control: off, a band's share of power, or a ratio of bands. `nyquist` is the
/// highest frequency the electrodes carry, or why this file cannot be mapped.
pub fn picker(ui: &mut egui::Ui, pal: &Palette, map: &mut Option<Measure>, nyquist: Result<f64, &str>, open_menu: &mut Option<String>) {
    let value = match (*map, &nyquist) {
        (Some(m), Ok(_)) => m.label(),
        _ => "Off",
    };
    let resp = widgets::pill(ui, pal, "Map", value, false);
    if open_menu.as_deref() == Some("map") {
        *open_menu = None;
        egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&resp));
    }
    egui::Popup::menu(&resp).width(270.0).show(|ui| {
        let nyquist = match nyquist {
            Ok(n) => n,
            Err(reason) => {
                ui.label(RichText::new(format!("Scalp maps use the average reference, which this file cannot form. {reason}")).color(pal.muted));
                return;
            }
        };
        if row(ui, pal, "Off", "montage lines", map.is_none(), true) {
            *map = None;
            ui.close();
        }
        for (heading, ratios) in [("Share of 1\u{2013}45 Hz power", false), ("Power ratios", true)] {
            widgets::heading(ui, pal, heading);
            for m in Measure::ALL.into_iter().filter(|m| m.is_ratio() == ratios) {
                if row(ui, pal, m.label(), &m.detail(), *map == Some(m), m.available(nyquist)) {
                    *map = Some(m);
                    ui.close();
                }
            }
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("Each electrode against the average of all, over the page on screen or a measured selection.")
                .color(pal.faint)
                .small(),
        );
    });
}

/// A menu row: a name on the left and a detail on the right. Returns true when clicked.
fn row(ui: &mut egui::Ui, pal: &Palette, label: &str, detail: &str, selected: bool, enabled: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, egui::CornerRadius::same(6), pal.accent_soft);
    } else if enabled && resp.hovered() {
        p.rect_filled(rect, egui::CornerRadius::same(6), pal.hover);
    }
    let (text, muted) = if enabled { (pal.text, pal.muted) } else { (pal.faint, pal.faint) };
    p.text(rect.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, label, theme::font(13.0), text);
    p.text(rect.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, detail, theme::font(11.5), muted);
    let resp = if enabled { resp } else { resp.on_hover_text("Above the Nyquist frequency of this recording") };
    resp.clicked()
}

/// The colour scale under the head: the lowest and highest electrode values either side of the
/// gradient, and what they measure.
pub fn legend(ui: &mut egui::Ui, pal: &Palette, m: &ScalpMap, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
    let p = ui.painter();
    let bar = Rect::from_min_max(pos2(rect.left() + 46.0, rect.top() + 3.0), pos2(rect.right() - 46.0, rect.top() + 11.0));
    let mut mesh = Mesh::default();
    let steps = 40;
    for k in 0..=steps {
        let t = k as f32 / steps as f32;
        let x = bar.left() + t * bar.width();
        mesh.colored_vertex(pos2(x, bar.top()), viridis(t));
        mesh.colored_vertex(pos2(x, bar.bottom()), viridis(t));
        if k > 0 {
            let i = 2 * k as u32;
            mesh.add_triangle(i - 2, i - 1, i);
            mesh.add_triangle(i - 1, i, i + 1);
        }
    }
    p.add(mesh);
    let font = theme::font(11.0);
    p.text(pos2(bar.left() - 6.0, bar.center().y), Align2::RIGHT_CENTER, m.format(m.lo), font.clone(), pal.muted);
    p.text(pos2(bar.right() + 6.0, bar.center().y), Align2::LEFT_CENTER, m.format(m.hi), font.clone(), pal.muted);
    p.text(pos2(bar.center().x, bar.bottom() + 4.0), Align2::CENTER_TOP, m.units(), font, pal.faint);
}
