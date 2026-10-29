//! The strip along the bottom: the whole recording as a spectral overview, with events and
//! the current page. Click or drag to move through the recording.

use eframe::egui::{self, Align2, Color32, ColorImage, CornerRadius, Rect, Sense, Stroke, StrokeKind, TextureOptions, pos2, vec2};
use oink_eeg::edf::AnnotationSource;
use oink_eeg::overview::Overview;

use super::doc::Doc;
use super::theme::{self, Palette};
use super::traces::{chip, fmt_clock};

pub struct Navigator {
    texture: Option<egui::TextureHandle>,
    version: u64,
    dark: bool,
}

impl Navigator {
    pub fn new() -> Self {
        Navigator { texture: None, version: u64::MAX, dark: false }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, doc: &mut Doc, page: f64) {
        let pal = theme::palette(ui.ctx());
        let full = ui.max_rect();
        let strip = Rect::from_min_max(pos2(full.left() + 12.0, full.top() + 8.0), pos2(full.right() - 12.0, full.bottom() - 20.0));
        let (lo, hi) = doc.range;
        let span = (hi - lo).max(1e-9);
        let x_of = |t: f64| strip.left() + ((t - lo) / span) as f32 * strip.width();
        let t_of = |x: f32| lo + ((x - strip.left()) / strip.width()) as f64 * span;
        let p = ui.painter();

        p.rect_filled(strip, CornerRadius::same(5), pal.bg);
        if let Some(ov) = &doc.overview {
            if self.version != doc.overview_version || self.dark != pal.dark {
                let image = render(ov, pal);
                match &mut self.texture {
                    Some(t) => t.set(image, TextureOptions::LINEAR),
                    None => self.texture = Some(ui.ctx().load_texture("overview", image, TextureOptions::LINEAR)),
                }
                self.version = doc.overview_version;
                self.dark = pal.dark;
            }
            if let Some(t) = &self.texture {
                let r = Rect::from_min_max(pos2(x_of(ov.t0), strip.top() + 1.0), pos2(x_of(ov.t1), strip.bottom() - 1.0));
                p.with_clip_rect(strip.shrink(1.0)).image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
        }
        for &(a, b) in &doc.gaps {
            p.rect_filled(Rect::from_min_max(pos2(x_of(a), strip.top()), pos2(x_of(b), strip.bottom())), 0.0, pal.grid_strong);
        }
        // Events: a tick along the top edge.
        let mut last_x = f32::NEG_INFINITY;
        for a in &doc.annotations {
            let x = x_of(a.onset).round() + 0.5;
            if x - last_x < 1.0 {
                continue;
            }
            last_x = x;
            let color = if a.source == AnnotationSource::Trigger { pal.trigger } else { pal.event };
            p.vline(x, strip.top()..=strip.top() + 7.0, Stroke::new(1.0f32, color));
        }
        p.rect_stroke(strip, CornerRadius::same(5), Stroke::new(1.0f32, pal.border), StrokeKind::Inside);

        // The current page.
        let view = Rect::from_min_max(pos2(x_of(doc.t0), strip.top()), pos2(x_of(doc.t0 + page).max(x_of(doc.t0) + 3.0), strip.bottom()));
        p.rect_filled(view, CornerRadius::same(3), pal.accent_soft);
        p.rect_stroke(view, CornerRadius::same(3), Stroke::new(1.5f32, pal.accent), StrokeKind::Inside);

        let resp = ui.interact(strip, ui.id().with("navigator"), Sense::click_and_drag());
        if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked() || resp.drag_started()) {
            doc.t0 = t_of(pos.x) - page / 2.0;
            doc.clamp_view(page);
        }
        if let Some(pos) = resp.hover_pos() {
            p.vline(pos.x, strip.y_range(), Stroke::new(1.0f32, pal.text.gamma_multiply(0.5)));
            let t = t_of(pos.x);
            chip(p, pos2(pos.x, strip.top() - 2.0), Align2::CENTER_BOTTOM, &doc.file.clock(t).time_string(false), pal.text, pal.raised, pal.border);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        // Start and end times, and where the page is.
        let y = full.bottom() - 10.0;
        let small = theme::font(11.0);
        p.text(pos2(strip.left(), y), Align2::LEFT_CENTER, doc.file.clock(lo).time_string(false), small.clone(), pal.muted);
        p.text(pos2(strip.right(), y), Align2::RIGHT_CENTER, doc.file.clock(hi).time_string(false), small.clone(), pal.muted);
        let where_ = format!(
            "{} \u{2013} {}   \u{b7}   +{} of {}",
            doc.file.clock(doc.t0).time_string(false),
            doc.file.clock(doc.t0 + page).time_string(false),
            fmt_clock(doc.t0 - lo),
            fmt_clock(span)
        );
        p.text(pos2(strip.center().x, y), Align2::CENTER_CENTER, where_, small, pal.muted);
        if doc.overview.as_ref().is_some_and(|o| !o.done) {
            p.text(pos2(strip.right() - 6.0, strip.bottom() - 4.0), Align2::RIGHT_BOTTOM, "building overview\u{2026}", theme::font(10.0), pal.faint);
        }
    }
}

/// Spectral overview as an image: each frequency row shown relative to its own median, so
/// changes over time (sleep stages, seizures, artefacts) stand out instead of the 1/f slope.
/// Columns not computed yet borrow their nearest computed neighbour.
fn render(ov: &Overview, pal: &Palette) -> ColorImage {
    let (rows, cols) = (ov.rows, ov.cols);
    let mut norm = vec![f32::NAN; rows * cols];
    let mut spread = Vec::new();
    for r in 0..rows {
        let row = &ov.db[r * cols..(r + 1) * cols];
        let mut finite: Vec<f32> = row.iter().copied().filter(|v| v.is_finite()).collect();
        if finite.is_empty() {
            continue;
        }
        finite.sort_by(f32::total_cmp);
        let median = finite[finite.len() / 2];
        for c in 0..cols {
            let v = row[c] - median;
            norm[r * cols + c] = v;
            if v.is_finite() {
                spread.push(v.abs());
            }
        }
    }
    spread.sort_by(f32::total_cmp);
    let scale = spread.get(spread.len() * 97 / 100).copied().unwrap_or(1.0).max(1e-3);

    // Nearest computed column for every column.
    let computed: Vec<usize> = (0..cols).filter(|&c| (0..rows).any(|r| norm[r * cols + c].is_finite())).collect();
    let mut source = vec![usize::MAX; cols];
    if !computed.is_empty() {
        let mut j = 0;
        for (c, s) in source.iter_mut().enumerate() {
            while j + 1 < computed.len() && computed[j + 1].abs_diff(c) <= computed[j].abs_diff(c) {
                j += 1;
            }
            *s = computed[j];
        }
    }

    let low = pal.bg;
    let high = lerp(pal.bg, pal.accent, if pal.dark { 0.85 } else { 0.72 });
    let mut pixels = vec![Color32::TRANSPARENT; rows * cols];
    for r in 0..rows {
        let y = rows - 1 - r; // low frequencies at the bottom
        for c in 0..cols {
            let src = source[c];
            if src == usize::MAX {
                continue;
            }
            let v = norm[r * cols + src];
            if !v.is_finite() {
                continue;
            }
            let t = (0.5 + 0.5 * v / scale).clamp(0.0, 1.0);
            pixels[y * cols + c] = lerp(low, high, t.powf(1.6));
        }
    }
    ColorImage { size: [cols, rows], pixels, source_size: vec2(cols as f32, rows as f32) }
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}
