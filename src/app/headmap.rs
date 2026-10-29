//! Head diagram: electrodes in their 10-10 positions with the montage's derivations drawn
//! between them. Used at full size in the sidebar and as thumbnails in the montage picker.

use std::collections::BTreeSet;

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};
use oink_eeg::leads::{ElectrodeId, LeadSet, Side, Site, TEN_TWENTY, electrode};
use oink_eeg::montage::Montage;

use super::theme::{self, Palette};

pub struct HeadMap<'a> {
    pub montage: &'a Montage,
    pub leads: &'a LeadSet,
    pub visible: Option<&'a [bool]>,
    /// Channels to emphasise (hovered or selected in the trace view).
    pub highlight: &'a [usize],
    pub highlight_electrode: Option<ElectrodeId>,
    pub hemisphere_colors: bool,
    /// Full size: electrode names and the empty 10-20 sites.
    pub detailed: bool,
}

/// Room for the ears, nose and eye electrodes outside the head circle (and, at full size,
/// for their names).
const MARGIN: f32 = 1.2;
const MARGIN_DETAILED: f32 = 1.34;

impl HeadMap<'_> {
    /// Draws into `rect`. Returns the electrode under `pointer`, if any.
    pub fn draw(&self, p: &Painter, rect: Rect, pal: &Palette, pointer: Option<Pos2>) -> Option<ElectrodeId> {
        let center = rect.center();
        let r = rect.width().min(rect.height()) * 0.5 / if self.detailed { MARGIN_DETAILED } else { MARGIN };
        let to = |x: f32, y: f32| pos2(center.x + x * r, center.y - y * r);
        let at = |e: ElectrodeId| {
            let info = e.info();
            to(info.x, info.y)
        };
        let line_w = if self.detailed { 1.3 } else { 1.0 };

        // Head outline, nose and ears.
        let outline = Stroke::new(line_w, pal.head);
        p.circle_stroke(center, r, outline);
        p.add(Shape::line(vec![to(-0.11, 0.994), to(0.0, 1.1), to(0.11, 0.994)], outline));
        for side in [-1.0f32, 1.0] {
            let ear: Vec<Pos2> = (0..=12)
                .map(|k| {
                    let a = std::f32::consts::PI * (k as f32 / 12.0 - 0.5);
                    to(side * (0.995 + 0.07 * a.cos()), 0.14 * a.sin() - 0.01)
                })
                .collect();
            p.add(Shape::line(ear, outline));
        }

        let color_of = |side: Side| match (self.hemisphere_colors, side) {
            (true, Side::Left) => pal.left,
            (true, Side::Right) => pal.right,
            _ => pal.trace,
        };

        // Electrodes in the file, and those used by the visible channels.
        let recorded: BTreeSet<ElectrodeId> = self.leads.leads.iter().filter_map(|l| l.electrode).collect();
        let mut used: BTreeSet<ElectrodeId> = BTreeSet::new();
        let mut emphasised: BTreeSet<ElectrodeId> = BTreeSet::new();
        for (i, c) in self.montage.channels.iter().enumerate() {
            if self.visible.is_some_and(|v| !v[i]) {
                continue;
            }
            used.extend(c.a);
            used.extend(c.b);
            if self.highlight.contains(&i) {
                emphasised.extend(c.a);
                emphasised.extend(c.b);
            }
        }
        if let Some(e) = self.highlight_electrode {
            emphasised.insert(e);
        }

        // Empty 10-20 sites, so missing electrodes are visible.
        if self.detailed {
            for name in TEN_TWENTY {
                let (id, _) = electrode(name).expect("10-20 site");
                if !recorded.contains(&id) {
                    p.circle_stroke(at(id), 2.6, Stroke::new(1.0, pal.head));
                }
            }
        }

        // Derivations between two electrodes: the montage's chains.
        let mut lines: Vec<(Pos2, Pos2, Color32, f32)> = Vec::new();
        for (i, c) in self.montage.channels.iter().enumerate() {
            if self.visible.is_some_and(|v| !v[i]) {
                continue;
            }
            let (Some(a), Some(b)) = (c.a, c.b) else { continue };
            let hot = self.highlight.contains(&i) || self.highlight_electrode.is_some_and(|e| e == a || e == b);
            let dim = !emphasised.is_empty() && !hot;
            let color = if hot { pal.accent } else { color_of(c.side) };
            let color = if dim { color.gamma_multiply(0.35) } else { color.gamma_multiply(0.85) };
            let off_scalp = a.info().site != Site::Scalp || b.info().site != Site::Scalp;
            let width = if hot { line_w * 2.2 } else { line_w * 1.4 };
            if off_scalp {
                p.extend(Shape::dashed_line(&[at(a), at(b)], Stroke::new(width * 0.8, color), 3.0, 2.5));
            } else {
                lines.push((at(a), at(b), color, width));
            }
        }
        for (a, b, color, width) in &lines {
            p.line_segment([*a, *b], Stroke::new(*width, *color));
        }

        // Electrode dots, labels, and the hovered electrode.
        let dot = if self.detailed { 3.6 } else { 1.9 };
        let mut hovered = None;
        let mut best = 9.0f32;
        for &e in recorded.iter() {
            let pos = at(e);
            if let Some(ptr) = pointer {
                let d = ptr.distance(pos);
                if d < best {
                    best = d;
                    hovered = Some(e);
                }
            }
        }
        let average = self.montage.mean_count > 0;
        for &e in recorded.iter() {
            let pos = at(e);
            let in_use = used.contains(&e);
            let hot = emphasised.contains(&e) || hovered == Some(e);
            let base = color_of(e.side());
            let fill = if hot {
                pal.accent
            } else if in_use {
                base
            } else {
                pal.faint
            };
            if average && in_use && self.detailed {
                // Each of these electrodes contributes to the average reference.
                p.circle_stroke(pos, dot + 2.5, Stroke::new(1.0, base.gamma_multiply(0.35)));
            }
            p.circle_filled(pos, if hot { dot + 1.2 } else { dot }, fill);
            if self.detailed && (recorded.len() <= 48 || hot) {
                let name = self
                    .leads
                    .leads
                    .iter()
                    .find(|l| l.electrode == Some(e))
                    .and_then(|l| l.electrode_name.clone())
                    .unwrap_or_else(|| e.info().name.clone());
                let color = if hot { pal.accent } else if in_use { pal.muted } else { pal.faint };
                // Outwards from the centre, clear of the chains that run front to back.
                let dir = pos - center;
                let dir = if dir.length() < r * 0.15 { vec2(1.0, 0.0) } else { dir.normalized() };
                let galley = p.layout_no_wrap(name, theme::font(9.5), color);
                let half = galley.size() / 2.0;
                let at = pos + dir * (dot + 3.0 + half.x.max(half.y) * 0.9);
                let rect = egui::Rect::from_center_size(at, galley.size());
                p.rect_filled(rect.expand(1.0), egui::CornerRadius::same(3), pal.chrome.gamma_multiply(0.8));
                p.galley(rect.min, galley, color);
            }
        }
        hovered
    }
}

/// One line describing what the head map shows for this montage.
pub fn caption(montage: &Montage) -> String {
    let n = montage.channels.iter().filter(|c| c.a.is_some()).count();
    if montage.mean_count > 0 {
        return format!("Each electrode against the mean of {} scalp electrodes", montage.mean_count);
    }
    let bipolar = montage.channels.iter().filter(|c| c.a.is_some() && c.b.is_some()).count();
    if bipolar * 2 > n && n > 0 {
        return format!("{bipolar} bipolar derivations; lines join each pair");
    }
    match &montage.reference {
        Some(r) => format!("{n} leads referred to {r}"),
        None if n > 0 => format!("{n} leads against the recording reference"),
        None => "No electrode positions in this file".to_string(),
    }
}

/// Square rect of side `size` at the top of `ui`'s remaining space.
pub fn allocate(ui: &mut egui::Ui, size: f32) -> (Rect, egui::Response) {
    ui.allocate_exact_size(vec2(size, size), egui::Sense::hover())
}
