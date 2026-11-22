//! The window's look, from the icon's palette: deep slate, teal for actions, amber for things to
//! check, rose for failures.

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2};

pub const BG: Color32 = Color32::from_rgb(0x11, 0x14, 0x1a);
pub const PANEL: Color32 = Color32::from_rgb(0x16, 0x1a, 0x21);
pub const CARD: Color32 = Color32::from_rgb(0x1d, 0x22, 0x2b);
pub const CARD_HI: Color32 = Color32::from_rgb(0x26, 0x2b, 0x36);
pub const LINE: Color32 = Color32::from_rgb(0x2e, 0x34, 0x41);
pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe8, 0xec);
pub const MUTED: Color32 = Color32::from_rgb(0x8a, 0x93, 0xa5);
pub const FAINT: Color32 = Color32::from_rgb(0x5b, 0x64, 0x75);
pub const ACCENT: Color32 = Color32::from_rgb(0x2d, 0xd4, 0xbf);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x14, 0x5e, 0x56);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x06, 0x1f, 0x1c);
pub const WARN: Color32 = Color32::from_rgb(0xf5, 0x9e, 0x0b);
pub const BAD: Color32 = Color32::from_rgb(0xf4, 0x3f, 0x5e);
pub const GOOD: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);

/// Corner radius of cards and buttons.
pub const RADIUS: u8 = 8;

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.window_margin = Margin::same(14);
        for (style, size) in [
            (egui::TextStyle::Heading, 19.0),
            (egui::TextStyle::Body, 14.0),
            (egui::TextStyle::Button, 14.0),
            (egui::TextStyle::Small, 12.0),
            (egui::TextStyle::Monospace, 12.5),
        ] {
            if let Some(f) = s.text_styles.get_mut(&style) {
                f.size = size;
            }
        }
        let v = &mut s.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(TEXT);
        v.panel_fill = PANEL;
        v.window_fill = CARD;
        v.extreme_bg_color = BG;
        v.faint_bg_color = CARD;
        v.code_bg_color = BG;
        v.window_stroke = Stroke::new(1.0, LINE);
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(RADIUS);
        v.selection.bg_fill = ACCENT_DIM;
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        v.warn_fg_color = WARN;
        v.error_fg_color = BAD;
        v.slider_trailing_fill = true;
        let r = CornerRadius::same(RADIUS);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CARD;
        w.noninteractive.weak_bg_fill = CARD;
        w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = r;
        w.inactive.bg_fill = CARD_HI;
        w.inactive.weak_bg_fill = CARD_HI;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = r;
        w.hovered.bg_fill = Color32::from_rgb(0x30, 0x37, 0x44);
        w.hovered.weak_bg_fill = Color32::from_rgb(0x30, 0x37, 0x44);
        w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x44, 0x4c, 0x5c));
        w.hovered.fg_stroke = Stroke::new(1.5, TEXT);
        w.hovered.corner_radius = r;
        w.active.bg_fill = ACCENT_DIM;
        w.active.weak_bg_fill = ACCENT_DIM;
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.fg_stroke = Stroke::new(1.5, TEXT);
        w.active.corner_radius = r;
        w.open.corner_radius = r;
    });
}

/// A raised block grouping one task.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
}

/// The main action of a screen.
pub fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).strong().color(ON_ACCENT))
        .fill(ACCENT)
        .min_size(Vec2::new(0.0, 34.0))
}

/// A quieter action.
pub fn secondary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(text.into()).min_size(Vec2::new(0.0, 30.0))
}

/// Small grey heading over a card: "PHOTOS", "REFERENCE LOOK".
pub fn eyebrow(ui: &mut egui::Ui, step: &str, title: &str) {
    ui.horizontal(|ui| {
        if !step.is_empty() {
            ui.label(RichText::new(step).small().strong().color(ACCENT));
        }
        ui.label(
            RichText::new(title.to_uppercase())
                .small()
                .strong()
                .color(MUTED),
        );
    });
}

pub fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(MUTED));
}

/// A rounded tag: `● LightCraft`.
pub fn chip(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().color(color));
        });
}

/// A coloured message strip.
pub fn banner(ui: &mut egui::Ui, color: Color32, text: &str) -> bool {
    let mut closed = false;
    egui::Frame::new()
        .fill(color.gamma_multiply(0.14))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(text).color(TEXT)).wrap());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("✕").clicked() {
                        closed = true;
                    }
                });
            });
        });
    closed
}

/// "about 4 min", "about 40 s".
pub fn duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    match s {
        0..=59 => format!("{s} s"),
        60..=5399 => {
            let m = (secs / 60.0).round() as u64;
            format!("{m} min")
        }
        _ => format!("{} h {:02} min", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(12.4), "12 s");
        assert_eq!(duration(95.0), "2 min");
        assert_eq!(duration(7380.0), "2 h 03 min");
    }
}
