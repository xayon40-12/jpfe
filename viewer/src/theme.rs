//! A macOS-like look: system colors, light and dark appearances, rounded controls and hairline borders.

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Theme, Visuals};

/// Colors that egui's `Visuals` have no slot for.
#[derive(Clone, Copy)]
pub struct Palette {
    pub sidebar: Color32,
    pub content: Color32,
    pub card: Color32,
    pub separator: Color32,
    pub secondary_text: Color32,
    pub accent: Color32,
    pub orange: Color32,
    pub blue: Color32,
    pub teal: Color32,
    pub purple: Color32,
    pub green: Color32,
    /// Text colors of the dictionary windows, darker (or lighter) than the chart colors so they read well
    /// as text: readings, grammar (parts of speech), and markers (sense numbers, on'yomi)
    pub reading: Color32,
    pub grammar: Color32,
    pub marker: Color32,
}

const LIGHT: Palette = Palette {
    sidebar: Color32::from_rgb(0xF0, 0xF0, 0xF3),
    content: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    separator: Color32::from_rgb(0xDC, 0xDC, 0xE0),
    secondary_text: Color32::from_rgb(0x6E, 0x6E, 0x73),
    accent: Color32::from_rgb(0x00, 0x7A, 0xFF),
    orange: Color32::from_rgb(0xFF, 0x95, 0x00),
    blue: Color32::from_rgb(0x00, 0x7A, 0xFF),
    teal: Color32::from_rgb(0x30, 0xB0, 0xC7),
    purple: Color32::from_rgb(0xAF, 0x52, 0xDE),
    green: Color32::from_rgb(0x34, 0xC7, 0x59),
    reading: Color32::from_rgb(0xB8, 0x4A, 0x00),
    grammar: Color32::from_rgb(0x8E, 0x3F, 0xC4),
    marker: Color32::from_rgb(0x0A, 0x7A, 0x8C),
};

const DARK: Palette = Palette {
    sidebar: Color32::from_rgb(0x26, 0x26, 0x28),
    content: Color32::from_rgb(0x1E, 0x1E, 0x1E),
    card: Color32::from_rgb(0x2A, 0x2A, 0x2C),
    separator: Color32::from_rgb(0x3A, 0x3A, 0x3C),
    secondary_text: Color32::from_rgb(0x98, 0x98, 0x9D),
    accent: Color32::from_rgb(0x0A, 0x84, 0xFF),
    orange: Color32::from_rgb(0xFF, 0x9F, 0x0A),
    blue: Color32::from_rgb(0x0A, 0x84, 0xFF),
    teal: Color32::from_rgb(0x40, 0xC8, 0xE0),
    purple: Color32::from_rgb(0xBF, 0x5A, 0xF2),
    green: Color32::from_rgb(0x30, 0xD1, 0x58),
    reading: Color32::from_rgb(0xFF, 0xB0, 0x5C),
    grammar: Color32::from_rgb(0xCB, 0x9C, 0xF5),
    marker: Color32::from_rgb(0x5C, 0xD0, 0xE3),
};

impl Palette {
    pub fn of(visuals: &Visuals) -> Self {
        if visuals.dark_mode { DARK } else { LIGHT }
    }
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_visuals_of(Theme::Light, visuals(false));
    ctx.set_visuals_of(Theme::Dark, visuals(true));
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
        ]
        .into();
        // Like native apps: text is not selectable, so it does not take clicks meant for what is around it
        style.interaction.selectable_labels = false;
        let spacing = &mut style.spacing;
        spacing.item_spacing = egui::vec2(8.0, 8.0);
        spacing.button_padding = egui::vec2(10.0, 4.0);
        spacing.interact_size.y = 24.0;
        spacing.menu_margin = Margin::same(6);
        spacing.window_margin = Margin::same(12);
        // Narrow enough for phones, where wider tooltips are cut at the screen edge
        spacing.tooltip_width = 300.0;
    });
}

fn visuals(dark: bool) -> Visuals {
    let p = if dark { DARK } else { LIGHT };
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    let text = if dark { Color32::from_rgb(0xF5, 0xF5, 0xF7) } else { Color32::from_rgb(0x1D, 0x1D, 0x1F) };
    let control = if dark { Color32::from_rgb(0x3A, 0x3A, 0x3C) } else { Color32::WHITE };
    let control_hover = if dark { Color32::from_rgb(0x45, 0x45, 0x48) } else { Color32::from_rgb(0xF5, 0xF5, 0xF7) };
    let control_pressed = if dark { Color32::from_rgb(0x52, 0x52, 0x55) } else { Color32::from_rgb(0xE5, 0xE5, 0xEA) };
    let border = if dark { Color32::from_rgb(0x4A, 0x4A, 0x4D) } else { Color32::from_rgb(0xC8, 0xC8, 0xCD) };
    let radius = CornerRadius::same(6);

    v.override_text_color = Some(text);
    v.panel_fill = p.content;
    v.window_fill = p.card;
    v.extreme_bg_color = p.card;
    v.faint_bg_color = p.sidebar;
    v.window_stroke = Stroke::new(1.0, p.separator);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if dark { 90 } else { 40 }) };
    v.popup_shadow = Shadow { offset: [0, 3], blur: 12, spread: 0, color: Color32::from_black_alpha(if dark { 80 } else { 30 }) };
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.linear_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.striped = false;
    // Sliders: blue filled part, like macOS
    v.slider_trailing_fill = true;
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.content;
    w.noninteractive.weak_bg_fill = p.content;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.separator);
    w.noninteractive.fg_stroke = Stroke::new(1.0, text);
    for (state, fill) in [(&mut w.inactive, control), (&mut w.hovered, control_hover), (&mut w.active, control_pressed), (&mut w.open, control_hover)] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0, border);
        state.fg_stroke = Stroke::new(1.0, text);
        state.corner_radius = radius;
        state.expansion = 0.0;
    }
    w.noninteractive.corner_radius = radius;
    v
}

/// A filled accent-colored push button, like a default button on macOS.
pub fn primary_button(text: &str, palette: Palette) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_string()).color(Color32::WHITE))
        .fill(palette.accent)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(6))
}

/// A rounded card with a hairline border.
pub fn card(palette: Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.card)
        .stroke(Stroke::new(1.0, palette.separator))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(12))
}
