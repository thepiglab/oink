//! Colours, type and widget styling for the light and dark themes.

use eframe::egui::{
    self, Color32, CornerRadius, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle,
    Theme, Visuals,
};

pub struct Palette {
    pub dark: bool,
    /// Trace area.
    pub bg: Color32,
    /// Toolbar, sidebar and navigator.
    pub chrome: Color32,
    /// Popups.
    pub raised: Color32,
    pub border: Color32,
    pub hover: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub trace: Color32,
    pub trace_other: Color32,
    pub left: Color32,
    pub right: Color32,
    pub grid: Color32,
    pub grid_strong: Color32,
    pub event: Color32,
    pub event_soft: Color32,
    pub trigger: Color32,
    pub head: Color32,
    pub warn: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const fn rgba(hex: u32, a: u8) -> Color32 {
    // Color32 is premultiplied.
    let f = a as u32;
    Color32::from_rgba_premultiplied(
        (((hex >> 16) & 0xFF) * f / 255) as u8,
        (((hex >> 8) & 0xFF) * f / 255) as u8,
        ((hex & 0xFF) * f / 255) as u8,
        a,
    )
}

pub const LIGHT: Palette = Palette {
    dark: false,
    bg: rgb(0xFFFFFF),
    chrome: rgb(0xF6F6F7),
    raised: rgb(0xFFFFFF),
    border: rgb(0xE2E2E6),
    hover: rgb(0xEAEAED),
    text: rgb(0x1B1B1F),
    muted: rgb(0x6A6A73),
    faint: rgb(0xA3A3AB),
    accent: rgb(0x5552D4),
    accent_soft: rgba(0x5552D4, 0x26),
    trace: rgb(0x1E1E22),
    trace_other: rgb(0x55555D),
    left: rgb(0x2B62C6),
    right: rgb(0xC43D2C),
    grid: rgb(0xF0F0F3),
    grid_strong: rgb(0xE0E0E6),
    event: rgb(0xB4730F),
    event_soft: rgba(0xD69A2D, 0x24),
    trigger: rgb(0x0E8A7A),
    head: rgb(0xC5C5CD),
    warn: rgb(0xB45309),
};

pub const DARK: Palette = Palette {
    dark: true,
    bg: rgb(0x111214),
    chrome: rgb(0x18191C),
    raised: rgb(0x202125),
    border: rgb(0x2B2C31),
    hover: rgb(0x27282D),
    text: rgb(0xECECEF),
    muted: rgb(0x9E9EA7),
    faint: rgb(0x606069),
    accent: rgb(0x8F8CF8),
    accent_soft: rgba(0x8F8CF8, 0x2E),
    trace: rgb(0xD9DADF),
    trace_other: rgb(0x8E8F97),
    left: rgb(0x7AA5FF),
    right: rgb(0xFF8577),
    grid: rgb(0x1A1B1E),
    grid_strong: rgb(0x25262B),
    event: rgb(0xE2A84F),
    event_soft: rgba(0xE2A84F, 0x22),
    trigger: rgb(0x3CC2AE),
    head: rgb(0x4B4C54),
    warn: rgb(0xF59E0B),
};

pub fn palette(ctx: &egui::Context) -> &'static Palette {
    if ctx.style().visuals.dark_mode { &DARK } else { &LIGHT }
}

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Installs fonts and the style for both themes.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (name, data, monospace) in crate::platform::system_fonts() {
        fonts.font_data.insert(name.to_string(), std::sync::Arc::new(data));
        let family = if monospace { FontFamily::Monospace } else { FontFamily::Proportional };
        fonts.families.entry(family).or_default().insert(0, name.to_string());
    }
    ctx.set_fonts(fonts);

    for (theme, pal) in [(Theme::Light, &LIGHT), (Theme::Dark, &DARK)] {
        ctx.style_mut_of(theme, |style| {
            style.visuals = visuals(pal);
            style.text_styles = [
                (TextStyle::Small, font(11.0)),
                (TextStyle::Body, font(13.0)),
                (TextStyle::Button, font(13.0)),
                (TextStyle::Heading, font(15.0)),
                (TextStyle::Monospace, mono(12.0)),
            ]
            .into();
            let s = &mut style.spacing;
            s.item_spacing = egui::vec2(8.0, 6.0);
            s.button_padding = egui::vec2(8.0, 4.0);
            s.interact_size = egui::vec2(24.0, 22.0);
            s.menu_margin = Margin::same(6);
            s.window_margin = Margin::same(10);
            s.icon_width = 14.0;
            s.icon_width_inner = 8.0;
            s.scroll.floating = true;
            s.scroll.bar_width = 6.0;
            style.interaction.selectable_labels = false;
            style.animation_time = 0.12;
        });
    }
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.muted);
    v.panel_fill = p.chrome;
    v.window_fill = p.raised;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.hover;
    v.code_bg_color = p.hover;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    let shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(if p.dark { 110 } else { 34 }) };
    v.popup_shadow = shadow;
    v.window_shadow = shadow;
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.text_cursor.stroke = Stroke::new(1.5, p.accent);

    let radius = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.chrome;
    w.noninteractive.weak_bg_fill = p.chrome;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;
    for (state, fill) in [(&mut w.inactive, p.hover), (&mut w.hovered, p.hover), (&mut w.active, p.border), (&mut w.open, p.hover)] {
        state.bg_fill = fill;
        state.weak_bg_fill = Color32::TRANSPARENT;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.0, p.text);
        state.corner_radius = radius;
        state.expansion = 0.0;
    }
    w.hovered.weak_bg_fill = p.hover;
    w.active.weak_bg_fill = p.border;
    w.open.weak_bg_fill = p.hover;
    w.inactive.bg_fill = p.hover;
    v
}
