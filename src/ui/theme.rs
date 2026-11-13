//! Spotify-dark color theme.

use egui::Color32;

/// Spotify green accent.
pub const ACCENT: Color32 = Color32::from_rgb(0x1D, 0xB9, 0x54);
/// Warning text (e.g. the playback-blocked banner).
pub const WARNING: Color32 = Color32::from_rgb(0xE9, 0x50, 0x50);
/// Main application background (central panel).
pub const BG: Color32 = Color32::from_rgb(0x12, 0x12, 0x12);
/// Raised container background inside the central content.
pub const PANEL: Color32 = Color32::from_rgb(0x18, 0x18, 0x18);
/// Deepest background: sidebar, bottom bar, scroll bars.
pub const EXTREME: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
/// Primary text.
pub const TEXT: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// Secondary / dimmed text.
pub const TEXT_DIM: Color32 = Color32::from_rgb(0xB3, 0xB3, 0xB3);
/// Subtle separator/frame line.
pub const SEPARATOR: Color32 = Color32::from_rgb(0x28, 0x28, 0x28);
/// Hovered widget background.
pub const HOVER: Color32 = Color32::from_rgb(0x28, 0x28, 0x28);
/// Pressed/active widget background.
pub const ACTIVE: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);

/// Applies the Spotify dark theme to the egui context.
pub fn apply(ctx: &egui::Context) {
    ctx.style_mut(|style| {
        let visuals = &mut style.visuals;
        visuals.dark_mode = true;

        // Layered backgrounds: central content sits on #121212, raised
        // containers on #181818, sidebar / bottom bar / scroll bars pure black.
        visuals.window_fill = BG;
        visuals.panel_fill = EXTREME;
        visuals.faint_bg_color = PANEL;
        visuals.extreme_bg_color = EXTREME;

        // Text.
        visuals.override_text_color = Some(TEXT);
        visuals.hyperlink_color = ACCENT;

        // Accent on selections.
        visuals.selection.bg_fill = ACCENT;
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, TEXT);

        // Rounded corners (~4px) on widgets, menus and windows.
        let radius = egui::CornerRadius::same(4);
        visuals.window_corner_radius = radius;
        visuals.menu_corner_radius = radius;

        // Raised surfaces for non-interactive widgets; the bg_stroke doubles
        // as the separator color, so keep it subtle.
        visuals.widgets.noninteractive.bg_fill = PANEL;
        visuals.widgets.noninteractive.weak_bg_fill = BG;
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, SEPARATOR);
        visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT_DIM);
        visuals.widgets.noninteractive.corner_radius = radius;

        // Buttons and interactives.
        for widgets in [
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widgets.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
            widgets.bg_stroke = egui::Stroke::new(1.0_f32, SEPARATOR);
            widgets.corner_radius = radius;
        }
        visuals.widgets.inactive.weak_bg_fill = PANEL;
        visuals.widgets.hovered.bg_fill = HOVER;
        visuals.widgets.hovered.weak_bg_fill = HOVER;
        visuals.widgets.active.bg_fill = ACTIVE;
        visuals.widgets.active.weak_bg_fill = ACTIVE;

        // Comfortable default spacing.
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
    });
}
