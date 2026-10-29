//! Small custom widgets: toolbar pills, segmented controls and drawn icons.

use eframe::egui::{self, Align2, Color32, CornerRadius, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};

use super::theme::{self, Palette};

/// A toolbar control: a muted caption, a value and a chevron. Highlighted while `open`.
pub fn pill(ui: &mut Ui, pal: &Palette, caption: &str, value: &str, open: bool) -> Response {
    let cap = ui.painter().layout_no_wrap(caption.to_string(), theme::font(11.5), pal.muted);
    let val = ui.painter().layout_no_wrap(value.to_string(), theme::font(13.0), pal.text);
    let gap = if caption.is_empty() { 0.0 } else { 6.0 };
    let width = 10.0 + cap.size().x + gap + val.size().x + 18.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 26.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if open || resp.hovered() {
            p.rect_filled(rect, CornerRadius::same(6), pal.hover);
        }
        let mut x = rect.left() + 10.0;
        let cy = rect.center().y;
        if !caption.is_empty() {
            p.galley(pos2(x, cy - cap.size().y / 2.0 + 0.5), cap.clone(), pal.muted);
            x += cap.size().x + gap;
        }
        p.galley(pos2(x, cy - val.size().y / 2.0), val.clone(), pal.text);
        chevron(p, pos2(rect.right() - 10.0, cy + 0.5), 3.2, pal.muted);
    }
    let label = format!("{caption} {value}");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small downward chevron centred at `c`.
pub fn chevron(p: &egui::Painter, c: Pos2, size: f32, color: Color32) {
    p.add(Shape::line(
        vec![pos2(c.x - size, c.y - size * 0.5), pos2(c.x, c.y + size * 0.5), pos2(c.x + size, c.y - size * 0.5)],
        Stroke::new(1.3f32, color),
    ));
}

/// A row of mutually exclusive options. Returns true if the value changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, pal: &Palette, value: &mut T, options: &[(T, &str)]) -> bool {
    let font = theme::font(12.0);
    let widths: Vec<f32> =
        options.iter().map(|(_, l)| ui.painter().layout_no_wrap(l.to_string(), font.clone(), pal.text).size().x + 16.0).collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, whole) = ui.allocate_exact_size(vec2(total, 24.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(6), pal.hover);
    let mut x = rect.left() + 2.0;
    let mut changed = false;
    for (k, ((v, label), w)) in options.iter().zip(widths).enumerate() {
        let r = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(w, rect.height() - 4.0));
        let resp = ui.interact(r, whole.id.with(k), Sense::click());
        let selected = *value == *v;
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, *label));
        if selected {
            ui.painter().rect_filled(r, CornerRadius::same(5), pal.raised);
            ui.painter().rect_stroke(r, CornerRadius::same(5), Stroke::new(1.0f32, pal.border), StrokeKind::Inside);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(5), pal.border.gamma_multiply(0.5));
        }
        let color = if selected { pal.text } else { pal.muted };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *label, font.clone(), color);
        if resp.clicked() && !selected {
            *value = *v;
            changed = true;
        }
        x += w;
    }
    changed
}

#[derive(Clone, Copy)]
pub enum Icon {
    Sidebar,
    More,
    Open,
}

/// A square icon button.
pub fn icon_button(ui: &mut Ui, pal: &Palette, icon: Icon, active: bool, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 26.0), Sense::click());
    let p = ui.painter();
    if active || resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(6), pal.hover);
    }
    let color = if active { pal.text } else { pal.muted };
    let stroke = Stroke::new(1.3f32, color);
    let c = rect.center();
    match icon {
        Icon::Sidebar => {
            let r = Rect::from_center_size(c, vec2(15.0, 12.0));
            p.rect_stroke(r, CornerRadius::same(2), stroke, StrokeKind::Middle);
            p.line_segment([pos2(r.right() - 5.0, r.top()), pos2(r.right() - 5.0, r.bottom())], stroke);
            if active {
                p.rect_filled(Rect::from_min_max(pos2(r.right() - 5.0, r.top()), r.max), CornerRadius::same(1), color.gamma_multiply(0.35));
            }
        }
        Icon::More => {
            for dx in [-5.0, 0.0, 5.0] {
                p.circle_filled(pos2(c.x + dx, c.y), 1.4, color);
            }
        }
        Icon::Open => {
            // A folder.
            let r = Rect::from_center_size(pos2(c.x, c.y + 0.5), vec2(15.0, 11.0));
            p.add(Shape::line(
                vec![
                    pos2(r.left(), r.bottom()),
                    pos2(r.left(), r.top()),
                    pos2(r.left() + 5.0, r.top()),
                    pos2(r.left() + 6.5, r.top() + 2.0),
                    pos2(r.right(), r.top() + 2.0),
                    pos2(r.right(), r.bottom()),
                    pos2(r.left(), r.bottom()),
                ],
                stroke,
            ));
        }
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tooltip));
    resp.on_hover_text(tooltip)
}

/// A muted, small-caps style section heading.
pub fn heading(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(text.to_uppercase()).font(theme::font(10.5)).color(pal.muted).extra_letter_spacing(0.6));
    ui.add_space(2.0);
}

/// A `key  value` row for information tables.
pub fn field(ui: &mut Ui, pal: &Palette, key: &str, value: &str) {
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(88.0, 18.0), Sense::hover());
        ui.painter().text(rect.left_center(), Align2::LEFT_CENTER, key, theme::font(12.5), pal.muted);
        ui.add(egui::Label::new(value).wrap());
    });
}

/// A small coloured dot.
pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 14.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}
