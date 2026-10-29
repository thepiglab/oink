//! The trace view: channel labels, time axis, the GPU-drawn traces, and everything drawn over
//! them (grid, annotations, cursor readout, measurement, scale bar).

use eframe::egui::{self, Align2, Color32, CornerRadius, Rangef, Rect, Sense, Shape, Stroke, StrokeKind, Vec2, pos2, vec2};
use eframe::egui_wgpu;
use oink_eeg::dsp::BREAK;
use oink_eeg::edf::AnnotationSource;
use oink_eeg::leads::{Kind, Side};
use oink_eeg::montage::Channel;

use super::Settings;
use super::doc::{Doc, Lanes};
use super::renderer::{ChannelGpu, FLAG_HIDDEN, FLAG_HIGHLIGHT, Globals, TraceCallback, pack};
use super::theme::{self, Palette};

/// Page lengths offered in the toolbar and stepped through with the keyboard (seconds).
pub const PAGES: [f64; 14] = [1.0, 2.0, 3.0, 5.0, 10.0, 15.0, 20.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1200.0, 1800.0];
/// Sensitivities offered in the toolbar (µV/mm).
pub const SENSITIVITIES: [f32; 18] =
    [1.0, 2.0, 3.0, 5.0, 7.0, 10.0, 15.0, 20.0, 30.0, 50.0, 70.0, 100.0, 150.0, 200.0, 300.0, 500.0, 1000.0, 2000.0];
const MIN_PAGE: f64 = 0.25;
const MAX_PAGE: f64 = 3600.0;

pub struct Env {
    /// Logical points per physical millimetre (for µV/mm).
    pub points_per_mm: f32,
    pub gpu: bool,
}

pub fn ui(ui: &mut egui::Ui, doc: &mut Doc, s: &mut Settings, env: &Env) {
    let pal = theme::palette(ui.ctx());
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, pal.bg);
    if doc.montage.channels.is_empty() {
        ui.painter().text(
            full.center(),
            Align2::CENTER_CENTER,
            "This file has no signals to plot.",
            theme::font(13.0),
            pal.muted,
        );
        return;
    }

    let label_font = theme::font(12.0);
    let widest = doc
        .montage
        .channels
        .iter()
        .enumerate()
        .filter(|(i, _)| doc.visible[*i])
        .map(|(_, c)| ui.painter().layout_no_wrap(c.label.clone(), label_font.clone(), pal.text).size().x)
        .fold(0.0f32, f32::max);
    let gutter_w = (widest + 30.0).clamp(64.0, 170.0);
    let axis_h = 24.0;
    let plot = Rect::from_min_max(pos2(full.left() + gutter_w, full.top() + axis_h), full.max);
    let gutter = Rect::from_min_max(pos2(full.left(), plot.top()), pos2(plot.left(), full.bottom()));
    let axis = Rect::from_min_max(pos2(plot.left(), full.top()), pos2(full.right(), plot.top()));
    if plot.width() < 20.0 || plot.height() < 20.0 {
        return;
    }

    let lanes = doc.lanes(plot.height());
    let resp = ui.interact(plot, ui.id().with("plot"), Sense::click_and_drag());
    handle_input(ui, doc, s, plot, gutter, &lanes, &resp);
    doc.v_scroll = doc.v_scroll.clamp(0.0, (lanes.content_h - plot.height()).max(0.0));

    let page = s.page;
    let pts_per_sec = plot.width() as f64 / page;
    let t0 = doc.t0;
    let x_of = |t: f64| plot.left() + ((t - t0) * pts_per_sec) as f32;
    let t_of = |x: f32| t0 + (x - plot.left()) as f64 / pts_per_sec;
    let ppp = ui.ctx().pixels_per_point();
    // The same pixel rounding egui-wgpu applies to the callback's viewport.
    let screen = ui.ctx().viewport_rect();
    let screen_px = [(screen.width() * ppp).round() as u32, (screen.height() * ppp).round() as u32];
    let info = egui::PaintCallbackInfo { viewport: plot, clip_rect: plot, pixels_per_point: ppp, screen_size_px: screen_px };
    let viewport = info.viewport_in_pixels();
    doc.ensure_traces(viewport.width_px as f64 / page, s.filters, page);

    let p = ui.painter_at(plot);
    // Outside the recording, and gaps in a discontinuous one.
    let shade = pal.grid;
    let (lo, hi) = doc.range;
    if x_of(lo) > plot.left() {
        p.rect_filled(Rect::from_min_max(plot.min, pos2(x_of(lo), plot.bottom())), 0.0, shade);
    }
    if x_of(hi) < plot.right() {
        p.rect_filled(Rect::from_min_max(pos2(x_of(hi), plot.top()), plot.max), 0.0, shade);
    }
    for &(a, b) in &doc.gaps {
        if b > doc.t0 && a < doc.t0 + page {
            let r = Rect::from_min_max(pos2(x_of(a), plot.top()), pos2(x_of(b), plot.bottom()));
            p.rect_filled(r, 0.0, shade);
            if r.width() > 40.0 {
                p.text(r.center_top() + vec2(0.0, 30.0), Align2::CENTER_TOP, "gap", theme::font(11.0), pal.faint);
            }
        }
    }
    draw_grid(&p, plot, doc.t0, page, pal);
    if s.show_annotations {
        draw_annotation_bands(&p, plot, doc, &lanes, page, pal);
    }
    if let Some((a, b)) = doc.measure {
        let (a, b) = (a.min(b), a.max(b));
        let r = Rect::from_min_max(pos2(x_of(a), plot.top()), pos2(x_of(b), plot.bottom()));
        p.rect_filled(r, 0.0, pal.accent_soft);
        p.vline(r.left(), plot.y_range(), Stroke::new(1.0f32, pal.accent));
        p.vline(r.right(), plot.y_range(), Stroke::new(1.0f32, pal.accent));
    }

    // The traces.
    let scale = viewport.height_px as f32 / plot.height();
    let highlight = |i: usize| {
        doc.hover_channel == Some(i)
            || doc.selected_channel == Some(i)
            || doc.hover_electrode.is_some_and(|e| doc.montage.channels[i].a == Some(e) || doc.montage.channels[i].b == Some(e))
    };
    let mut chans = vec![ChannelGpu { flags: FLAG_HIDDEN, ..Default::default() }; doc.montage.channels.len()];
    for &(i, yc) in &lanes.rows {
        let c = &doc.montage.channels[i];
        let color = if doc.selected_channel == Some(i) { pal.accent } else { trace_color(c, s, pal) };
        chans[i] = ChannelGpu {
            y0: (yc - doc.v_scroll) * scale,
            gain: gain(c, doc.gains[i], s, env, &lanes) * scale,
            color: pack(color),
            flags: if highlight(i) { FLAG_HIGHLIGHT } else { 0 },
        };
    }
    if let Some(data) = doc.traces().cloned() {
        if env.gpu {
            let globals = Globals {
                size: [viewport.width_px as f32, viewport.height_px as f32],
                x_origin: (doc.t0 - data.anchor) as f32,
                x_scale: (viewport.width_px as f64 / page) as f32,
                half_width: 0.55 * ppp.max(1.0),
                linear: 0,
                _pad: [0.0; 2],
            };
            p.add(egui_wgpu::Callback::new_paint_callback(plot, TraceCallback { data, globals, channels: chans }));
        } else {
            draw_traces_cpu(&p, plot, doc, &data, &chans, scale, page);
        }
    }

    if s.show_annotations {
        annotation_markers(ui, plot, doc, &lanes, page, pal);
    }

    // Cursor and value readout.
    let hover = resp.hover_pos().filter(|_| !resp.dragged());
    // The pointer's time, drawn over the axis once that is drawn.
    let mut time_chip = None;
    if let Some(pos) = hover {
        p.vline(pos.x, plot.y_range(), Stroke::new(1.0f32, pal.faint));
        let t = t_of(pos.x);
        let label = doc.file.clock(t).time_string(true);
        time_chip = Some((pos2(pos.x, axis.center().y), label));
        if let Some(ch) = lane_at(&lanes, doc.v_scroll, plot, pos.y) {
            if let Some(text) = readout(doc, ch, t, 0.75 / pts_per_sec) {
                let c = &doc.montage.channels[ch];
                let text = format!("{}   {text}", c.label);
                let anchor = if pos.x > plot.right() - 220.0 { Align2::RIGHT_BOTTOM } else { Align2::LEFT_BOTTOM };
                let off = if anchor == Align2::RIGHT_BOTTOM { vec2(-12.0, -10.0) } else { vec2(12.0, -10.0) };
                chip(&p, pos + off, anchor, &text, pal.text, pal.raised, pal.border);
            }
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if let Some((a, b)) = doc.measure {
        let (a, b) = (a.min(b), a.max(b));
        if b - a > 1e-6 {
            let mut text = format!("\u{394} {}  \u{b7}  {:.2} Hz", fmt_seconds(b - a), 1.0 / (b - a));
            let target = doc.selected_channel.or_else(|| hover.and_then(|h| lane_at(&lanes, doc.v_scroll, plot, h.y)));
            if let Some(ch) = target.filter(|&c| doc.visible[c]) {
                if let Some((min, max)) = extremes(doc, ch, a, b) {
                    let c = &doc.montage.channels[ch];
                    text += &format!("  \u{b7}  {} {} p-p ({})", fmt_value(max - min), c.unit, c.label);
                }
            }
            let x = x_of((a + b) / 2.0).clamp(plot.left() + 120.0, plot.right() - 120.0);
            chip(&p, pos2(x, plot.top() + 8.0), Align2::CENTER_TOP, &text, pal.bg, pal.accent, pal.accent);
        }
    }

    if lanes.rows.iter().any(|&(i, _)| doc.montage.channels[i].sensitivity) {
        scale_bar(&p, plot, s, env, &lanes, pts_per_sec, pal);
    }
    draw_labels(ui, gutter, plot, doc, s, &lanes, pal, &label_font);
    draw_axis(ui.painter(), axis, full, doc, page, pts_per_sec, pal);
    if let Some((at, label)) = &time_chip {
        chip(ui.painter(), *at, Align2::CENTER_CENTER, label, pal.text, pal.raised, pal.border);
    }

    if let Some(err) = &doc.read_error {
        chip(&p, plot.center_bottom() - vec2(0.0, 12.0), Align2::CENTER_BOTTOM, err, pal.warn, pal.raised, pal.border);
    } else if doc.traces().is_none() {
        p.text(plot.center(), Align2::CENTER_CENTER, "Reading\u{2026}", theme::font(12.0), pal.muted);
    }
}

fn handle_input(ui: &egui::Ui, doc: &mut Doc, s: &mut Settings, plot: Rect, gutter: Rect, lanes: &Lanes, resp: &egui::Response) {
    let page = s.page;
    let pts_per_sec = plot.width() as f64 / page;
    let t_of = |t0: f64, x: f32| t0 + (x - plot.left()) as f64 / pts_per_sec;
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let over = pointer.is_some_and(|p| plot.contains(p) || gutter.contains(p));
    if over && !egui::Popup::is_any_open(ui.ctx()) {
        let (scroll, zoom, mods) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers));
        if zoom != 1.0 {
            // Pinch or cmd/ctrl + wheel: zoom time around the pointer.
            let anchor = pointer.map(|p| t_of(doc.t0, p.x.clamp(plot.left(), plot.right()))).unwrap_or(doc.t0 + page / 2.0);
            let new_page = (page / zoom as f64).clamp(MIN_PAGE, max_page(doc));
            doc.t0 = anchor - (anchor - doc.t0) * new_page / page;
            s.page = new_page;
        } else if mods.alt && scroll.y != 0.0 {
            // Alt + wheel: amplitude (one channel when over its label).
            let factor = (scroll.y * 0.01).exp();
            match pointer.filter(|p| gutter.contains(*p)).and_then(|p| lane_at(lanes, doc.v_scroll, plot, p.y)) {
                Some(ch) => doc.gains[ch] = (doc.gains[ch] * factor).clamp(0.05, 50.0),
                None => s.sensitivity = (s.sensitivity / factor).clamp(0.5, 5000.0),
            }
        } else {
            if scroll.x != 0.0 {
                doc.t0 -= scroll.x as f64 / pts_per_sec;
            }
            if scroll.y != 0.0 {
                if lanes.content_h > plot.height() + 0.5 {
                    doc.v_scroll -= scroll.y;
                } else {
                    doc.t0 -= scroll.y as f64 / pts_per_sec;
                }
            }
        }
        if scroll != Vec2::ZERO {
            ui.ctx().input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
        }
        doc.clamp_view(s.page);
    }

    if resp.drag_started() {
        if let Some(o) = ui.input(|i| i.pointer.press_origin()) {
            let t = t_of(doc.t0, o.x);
            doc.measure = Some((t, t));
        }
    }
    if resp.dragged() {
        if let (Some(p), Some(m)) = (resp.interact_pointer_pos(), doc.measure.as_mut()) {
            m.1 = t_of(doc.t0, p.x.clamp(plot.left(), plot.right()));
        }
    }
    if resp.clicked() {
        doc.measure = None;
    }
}

pub fn max_page(doc: &Doc) -> f64 {
    let duration = doc.range.1 - doc.range.0;
    MAX_PAGE.min(duration.max(MIN_PAGE) * 1.0).max(MIN_PAGE)
}

fn lane_at(lanes: &Lanes, v_scroll: f32, plot: Rect, y: f32) -> Option<usize> {
    let rel = y - plot.top() + v_scroll;
    lanes.rows.iter().find(|(_, c)| (rel - c).abs() <= lanes.lane_h / 2.0).map(|(i, _)| *i)
}

/// Points per display unit, signed for screen coordinates (y grows downwards).
fn gain(c: &Channel, user: f32, s: &Settings, env: &Env, lanes: &Lanes) -> f32 {
    let base = if c.sensitivity { env.points_per_mm / s.sensitivity } else { lanes.lane_h * 0.42 / c.half_range.max(1e-12) };
    // Clinical EEG convention: negative is up. It applies to EEG-like signals only.
    let sign = if c.sensitivity && s.negative_up { 1.0 } else { -1.0 };
    sign * base * user
}

pub fn trace_color(c: &Channel, s: &Settings, pal: &Palette) -> Color32 {
    if !matches!(c.kind, Kind::Eeg | Kind::Eog) || !c.sensitivity {
        return pal.trace_other;
    }
    if !s.hemisphere_colors {
        return pal.trace;
    }
    match c.side {
        Side::Left => pal.left,
        Side::Right => pal.right,
        _ => pal.trace,
    }
}

fn draw_grid(p: &egui::Painter, plot: Rect, t0: f64, page: f64, pal: &Palette) {
    let pts_per_sec = plot.width() as f64 / page;
    let x_of = |t: f64| plot.left() + ((t - t0) * pts_per_sec) as f32;
    let lines = |step: f64, color: Color32| {
        let first = (t0 / step).ceil() as i64;
        let last = ((t0 + page) / step).floor() as i64;
        if last - first > 4000 {
            return;
        }
        for k in first..=last {
            p.vline(x_of(k as f64 * step).round() + 0.5, plot.y_range(), Stroke::new(1.0f32, color));
        }
    };
    if pts_per_sec * 0.2 >= 14.0 {
        lines(0.2, pal.grid);
    }
    if pts_per_sec >= 6.0 {
        lines(1.0, pal.grid_strong);
    } else {
        lines(tick_step(pts_per_sec), pal.grid_strong);
    }
}

fn tick_step(pts_per_sec: f64) -> f64 {
    const STEPS: [f64; 18] =
        [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0, 14400.0];
    STEPS.iter().copied().find(|s| s * pts_per_sec >= 78.0).unwrap_or(14400.0)
}

fn draw_axis(p: &egui::Painter, axis: Rect, full: Rect, doc: &Doc, page: f64, pts_per_sec: f64, pal: &Palette) {
    let p = p.with_clip_rect(axis.union(Rect::from_min_max(full.min, pos2(axis.left(), axis.bottom()))));
    p.hline(full.x_range(), axis.bottom() - 0.5, Stroke::new(1.0f32, pal.border));
    let step = tick_step(pts_per_sec);
    let day = doc.file.start.hour as f64 * 3600.0 + doc.file.start.minute as f64 * 60.0 + doc.file.start.second as f64;
    let first = ((doc.t0 + day) / step).ceil();
    let x_of = |t: f64| axis.left() + ((t - doc.t0) * pts_per_sec) as f32;
    for k in 0..400 {
        let t = (first + k as f64) * step - day;
        if t > doc.t0 + page {
            break;
        }
        let x = x_of(t).round() + 0.5;
        p.vline(x, (axis.bottom() - 5.0)..=axis.bottom(), Stroke::new(1.0f32, pal.faint));
        let text = doc.file.clock(t).time_string(step < 1.0);
        let text = if step < 1.0 { text.trim_end_matches('0').trim_end_matches('.').to_string() } else { text };
        let galley = p.layout_no_wrap(text, theme::font(11.5), pal.muted);
        let half = galley.size().x / 2.0;
        if x - half >= axis.left() + 2.0 && x + half <= axis.right() - 2.0 {
            p.galley(pos2(x - half, axis.center().y - 1.0 - galley.size().y / 2.0), galley, pal.muted);
        }
    }
    // Elapsed time of the view start, in the corner above the labels.
    let elapsed = doc.t0 - doc.range.0;
    p.text(
        pos2(full.left() + 10.0, axis.center().y - 1.0),
        Align2::LEFT_CENTER,
        format!("+{}", fmt_clock(elapsed)),
        theme::mono(11.0),
        pal.faint,
    );
}

/// The colour of annotation `i`: its label's, or the default for where it came from.
pub fn event_color(doc: &Doc, i: usize, pal: &Palette) -> Color32 {
    match doc.label(i).and_then(|l| l.color) {
        Some(c) => pal.labels[c],
        None if doc.annotations[i].source == AnnotationSource::Trigger => pal.trigger,
        None => pal.event,
    }
}

/// Vertical extents of the lanes drawn from file signal `s`.
fn signal_lanes<'a>(doc: &'a Doc, lanes: &'a Lanes, plot: Rect, s: usize) -> impl Iterator<Item = Rangef> + 'a {
    lanes.rows.iter().filter(move |(i, _)| doc.montage.channels[*i].terms.iter().any(|t| t.0 as usize == s)).map(move |&(_, yc)| {
        let y = plot.top() + yc - doc.v_scroll;
        Rangef::new(y - lanes.lane_h / 2.0, y + lanes.lane_h / 2.0)
    })
}

/// Annotations with a duration, tinted in their label's colour: across every lane, or only the
/// lanes of the signal an annotation is about.
fn draw_annotation_bands(p: &egui::Painter, plot: Rect, doc: &Doc, lanes: &Lanes, page: f64, pal: &Palette) {
    let pts_per_sec = plot.width() as f64 / page;
    let x_of = |t: f64| plot.left() + ((t - doc.t0) * pts_per_sec) as f32;
    let t1 = doc.t0 + page;
    for (i, a) in doc.annotations.iter().enumerate() {
        if a.onset > t1 {
            break;
        }
        if a.duration <= 0.0 || a.onset + a.duration < doc.t0 || !doc.event_shown(i) {
            continue;
        }
        let color = event_color(doc, i, pal).gamma_multiply(0.14);
        let x = Rangef::new(x_of(a.onset), x_of(a.onset + a.duration));
        match a.signal {
            Some(s) => {
                for y in signal_lanes(doc, lanes, plot, s) {
                    p.rect_filled(Rect::from_x_y_ranges(x, y), 0.0, color);
                }
            }
            None => {
                p.rect_filled(Rect::from_x_y_ranges(x, plot.y_range()), 0.0, color);
            }
        }
    }
}

/// Event lines and labelled chips along the top of the plot.
fn annotation_markers(ui: &egui::Ui, plot: Rect, doc: &mut Doc, lanes: &Lanes, page: f64, pal: &Palette) {
    let pts_per_sec = plot.width() as f64 / page;
    let x_of = |t: f64| plot.left() + ((t - doc.t0) * pts_per_sec) as f32;
    let p = ui.painter_at(plot);
    let t1 = doc.t0 + page;
    let mut rows: [f32; 3] = [f32::NEG_INFINITY; 3];
    let mut clicked = None;
    let start = doc.annotations.partition_point(|a| a.onset < doc.t0 - 1e-9);
    for (i, a) in doc.annotations.iter().enumerate().skip(start) {
        if a.onset > t1 {
            break;
        }
        if !doc.event_shown(i) {
            continue;
        }
        let color = event_color(doc, i, pal);
        let x = x_of(a.onset).round() + 0.5;
        let selected = doc.selected_event == Some(i);
        let stroke = Stroke::new(if selected { 1.5f32 } else { 1.0 }, if selected { color } else { color.gamma_multiply(0.7) });
        let line = |y: Rangef| Shape::dashed_line(&[pos2(x, y.min), pos2(x, y.max)], stroke, 4.0, 3.0);
        match a.signal {
            Some(s) => signal_lanes(doc, lanes, plot, s).for_each(|y| p.extend(line(y))),
            None => p.extend(line(plot.y_range())),
        }

        let full = doc.event_text(i);
        let mut text: String = full.chars().take(28).collect();
        if full.chars().count() > 28 {
            text.push('\u{2026}');
        }
        let galley = ui.painter().layout_no_wrap(text, theme::font(11.0), color);
        let w = galley.size().x + 10.0;
        let Some(row) = rows.iter().position(|&r| r < x - 2.0) else { continue };
        rows[row] = x + w;
        let rect = Rect::from_min_size(pos2(x, plot.top() + 4.0 + row as f32 * 20.0), vec2(w, 17.0));
        p.rect_filled(rect, CornerRadius::same(4), if selected { color.gamma_multiply(0.22) } else { pal.bg.gamma_multiply(0.92) });
        p.rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0f32, color.gamma_multiply(0.45)), StrokeKind::Inside);
        p.galley(rect.min + vec2(5.0, (rect.height() - galley.size().y) / 2.0), galley, color);
        let resp = ui.interact(rect, ui.id().with(("event", i)), Sense::click());
        if resp.clicked() {
            clicked = Some(i);
        }
        let when = doc.file.clock(a.onset).time_string(true);
        let dur = if a.duration > 0.0 { format!("  \u{b7}  {}", fmt_seconds(a.duration)) } else { String::new() };
        resp.on_hover_text(format!("{full}\n{when}{dur}"));
    }
    if let Some(i) = clicked {
        doc.select_event(i);
    }
}

fn draw_labels(
    ui: &mut egui::Ui,
    gutter: Rect,
    plot: Rect,
    doc: &mut Doc,
    s: &Settings,
    lanes: &Lanes,
    pal: &Palette,
    font: &egui::FontId,
) {
    let p = ui.painter_at(gutter);
    p.vline(gutter.right() - 0.5, gutter.y_range(), Stroke::new(1.0f32, pal.border));
    let small = lanes.lane_h < 15.0;
    let font = if small { theme::font(10.5) } else { font.clone() };
    let mut hovered = None;
    let mut toggle_select = None;
    let mut hide = None;
    let mut reset_gain = None;
    for &(i, yc) in &lanes.rows {
        let y = plot.top() + yc - doc.v_scroll;
        if y < gutter.top() - lanes.lane_h || y > gutter.bottom() + lanes.lane_h {
            continue;
        }
        let lane = Rect::from_center_size(pos2(gutter.center().x, y), vec2(gutter.width(), lanes.lane_h.max(10.0))).intersect(gutter);
        let resp = ui.interact(lane, ui.id().with(("lane", i)), Sense::click());
        let c = &doc.montage.channels[i];
        let selected = doc.selected_channel == Some(i);
        if resp.hovered() || selected {
            p.rect_filled(lane.shrink2(vec2(4.0, 1.0)), CornerRadius::same(4), if selected { pal.accent_soft } else { pal.hover });
        }
        let color = if selected {
            pal.accent
        } else {
            match trace_color(c, s, pal) {
                col if col == pal.trace || col == pal.trace_other => {
                    if c.kind == Kind::Eeg { pal.text } else { pal.muted }
                }
                col => col,
            }
        };
        p.text(pos2(gutter.right() - 10.0, y), Align2::RIGHT_CENTER, &c.label, font.clone(), color);
        let user_gain = doc.gains[i];
        if (user_gain - 1.0).abs() > 0.01 && !small {
            p.text(pos2(gutter.left() + 6.0, y), Align2::LEFT_CENTER, format!("\u{d7}{user_gain:.1}"), theme::font(10.0), pal.faint);
        }
        if resp.hovered() {
            hovered = Some(i);
        }
        if resp.clicked() {
            toggle_select = Some(i);
        }
        let resp = resp.on_hover_ui(|ui| {
            ui.set_max_width(320.0);
            ui.label(egui::RichText::new(&c.label).strong());
            ui.label(egui::RichText::new(&c.detail).color(pal.muted));
            let scaling = if c.sensitivity { "drawn at the global sensitivity".to_string() } else { format!("fitted to its lane (\u{b1}{} {})", fmt_value(c.half_range), c.unit) };
            ui.label(egui::RichText::new(format!("{} \u{b7} {} Hz \u{b7} {} \u{b7} {scaling}", c.kind.name(), fmt_hz(c.fs), c.unit)).color(pal.muted).small());
            ui.label(egui::RichText::new(format!("Click to select \u{b7} {}scroll for this channel's gain", super::ALT)).color(pal.faint).small());
        });
        resp.context_menu(|ui| {
            if ui.button("Hide channel").clicked() {
                hide = Some(i);
                ui.close();
            }
            if (user_gain - 1.0).abs() > 0.01 && ui.button("Reset gain").clicked() {
                reset_gain = Some(i);
                ui.close();
            }
        });
    }
    doc.hover_channel = hovered.or(doc.hover_channel);
    if let Some(i) = toggle_select {
        doc.selected_channel = if doc.selected_channel == Some(i) { None } else { Some(i) };
    }
    if let Some(i) = hide {
        doc.set_visible(i, false);
        if doc.selected_channel == Some(i) {
            doc.selected_channel = None;
        }
    }
    if let Some(i) = reset_gain {
        doc.gains[i] = 1.0;
    }
}

/// An L-shaped calibration mark: µV per vertical bar at the current sensitivity, and seconds
/// per horizontal bar.
fn scale_bar(p: &egui::Painter, plot: Rect, s: &Settings, env: &Env, lanes: &Lanes, pts_per_sec: f64, pal: &Palette) {
    const UV: [f32; 16] = [1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0, 20_000.0];
    const SECS: [f64; 13] = [0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0];
    let pts_per_uv = env.points_per_mm / s.sensitivity;
    let target = (lanes.lane_h * 0.8).clamp(16.0, 56.0);
    let uv = UV.iter().copied().find(|v| v * pts_per_uv >= target * 0.55).unwrap_or(20_000.0);
    let h = uv * pts_per_uv;
    let secs = SECS.iter().copied().find(|v| v * pts_per_sec >= 36.0).unwrap_or(600.0);
    let w = (secs * pts_per_sec) as f32;
    let uv_label = if uv >= 1000.0 { format!("{} mV", uv / 1000.0) } else { format!("{uv} \u{b5}V") };
    let corner = pos2(plot.right() - 18.0, plot.bottom() - 26.0);
    let bg = Rect::from_min_max(pos2(corner.x - w - 60.0, corner.y - h - 8.0), pos2(corner.x + 8.0, corner.y + 19.0));
    p.rect_filled(bg, CornerRadius::same(6), pal.bg.gamma_multiply(0.78));
    let stroke = Stroke::new(1.6f32, pal.text);
    let left = corner.x - w;
    p.line_segment([pos2(left, corner.y - h), pos2(left, corner.y)], stroke);
    p.line_segment([pos2(left, corner.y), pos2(corner.x, corner.y)], stroke);
    p.text(pos2(left - 6.0, corner.y - h / 2.0 - 6.0), Align2::RIGHT_CENTER, uv_label, theme::font(11.0), pal.text);
    let polarity = if s.negative_up { "neg. up" } else { "pos. up" };
    p.text(pos2(left - 6.0, corner.y - h / 2.0 + 7.0), Align2::RIGHT_CENTER, polarity, theme::font(10.0), pal.muted);
    p.text(pos2((left + corner.x) / 2.0, corner.y + 4.0), Align2::CENTER_TOP, fmt_seconds(secs), theme::font(11.0), pal.text);
}

/// Text for the value of channel `ch` at time `t` (± `half` seconds).
fn readout(doc: &Doc, ch: usize, t: f64, half: f64) -> Option<String> {
    let c = &doc.montage.channels[ch];
    let (min, max) = extremes(doc, ch, t - half, t + half)?;
    let offset = if c.ac { 0.0 } else { c.center };
    let (min, max) = (min + offset, max + offset);
    Some(if (max - min).abs() <= 0.05 * max.abs().max(min.abs()).max(1e-6) {
        format!("{} {}", fmt_value((min + max) / 2.0), c.unit)
    } else {
        format!("{} \u{2026} {} {}", fmt_value(min), fmt_value(max), c.unit)
    })
}

fn extremes(doc: &Doc, ch: usize, a: f64, b: f64) -> Option<(f32, f32)> {
    let data = doc.traces()?;
    let pts = data.points_in(ch, a, b);
    let mut it = pts.iter().filter(|p| p.ch != BREAK).map(|p| p.y);
    let first = it.next()?;
    Some(it.fold((first, first), |(lo, hi), v| (lo.min(v), hi.max(v))))
}

fn draw_traces_cpu(p: &egui::Painter, plot: Rect, doc: &Doc, data: &oink_eeg::pipeline::TraceData, chans: &[ChannelGpu], scale: f32, page: f64) {
    let pts_per_sec = plot.width() as f64 / page;
    for (i, range) in data.ranges.iter().enumerate() {
        let Some(c) = chans.get(i) else { continue };
        if c.flags & FLAG_HIDDEN != 0 || range.is_empty() {
            continue;
        }
        let color = Color32::from_rgba_premultiplied(c.color as u8, (c.color >> 8) as u8, (c.color >> 16) as u8, (c.color >> 24) as u8);
        let stroke = Stroke::new(if c.flags & FLAG_HIGHLIGHT != 0 { 1.8f32 } else { 1.0 }, color);
        let mut line = Vec::new();
        for pt in &data.points[range.start as usize..range.end as usize] {
            if pt.ch == BREAK {
                if line.len() > 1 {
                    p.add(Shape::line(std::mem::take(&mut line), stroke));
                }
                line.clear();
                continue;
            }
            let x = plot.left() + ((pt.x as f64 + data.anchor - doc.t0) * pts_per_sec) as f32;
            line.push(pos2(x, plot.top() + (c.y0 + pt.y * c.gain) / scale));
        }
        if line.len() > 1 {
            p.add(Shape::line(line, stroke));
        }
    }
}

/// A small rounded label.
pub fn chip(p: &egui::Painter, pos: egui::Pos2, align: Align2, text: &str, fg: Color32, bg: Color32, border: Color32) {
    let galley = p.layout_no_wrap(text.to_string(), theme::font(11.5), fg);
    let size = galley.size() + vec2(12.0, 6.0);
    let rect = align.anchor_size(pos, size);
    p.rect_filled(rect, CornerRadius::same(5), bg);
    p.rect_stroke(rect, CornerRadius::same(5), Stroke::new(1.0f32, border), StrokeKind::Inside);
    p.galley(rect.min + vec2(6.0, 3.0), galley, fg);
}

/// Sensitivity for the toolbar: "7", "7.5".
pub fn fmt_sensitivity(v: f32) -> String {
    if (v - v.round()).abs() < 0.05 { format!("{}", v.round()) } else { format!("{v:.1}") }
}

pub fn fmt_value(v: f32) -> String {
    let a = v.abs();
    if a >= 100.0 {
        format!("{v:.0}")
    } else if a >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

pub fn fmt_seconds(s: f64) -> String {
    if s < 1.0 {
        format!("{:.0} ms", s * 1000.0)
    } else if s < 60.0 {
        let t = format!("{s:.2}");
        format!("{} s", t.trim_end_matches('0').trim_end_matches('.'))
    } else if s < 3600.0 {
        format!("{:.0} min", s / 60.0)
    } else {
        format!("{:.1} h", s / 3600.0)
    }
}

pub fn fmt_hz(hz: f64) -> String {
    let t = format!("{hz:.3}");
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// `h:mm:ss` or `m:ss`.
pub fn fmt_clock(seconds: f64) -> String {
    let s = seconds.max(0.0).floor() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Page length for the toolbar.
pub fn fmt_page(page: f64) -> String {
    if page < 60.0 {
        let t = format!("{page:.1}");
        format!("{} s", t.trim_end_matches('0').trim_end_matches('.'))
    } else {
        let m = page / 60.0;
        let t = format!("{m:.1}");
        format!("{} min", t.trim_end_matches('0').trim_end_matches('.'))
    }
}
