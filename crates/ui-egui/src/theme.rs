//! Design system: themes, colour tokens, radii, typography.
//!
//! - **Studio** (default): near-black surfaces, rounded cards, Inter + JetBrains Mono, soft violet
//!   accent. Modelled on the look of modern pro editors (such as Photoshop 2025).
//! - **Studio Light**: the same system on light surfaces.
//! - **Classic**: a deliberately Windows-2000-era look (grey bevels, square corners, navy selection)
//!   for people who prefer it.
//!
//! Widgets read [`Tokens::get`] instead of hard-coding colours, so every theme applies everywhere.

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeKind {
    /// Photoshop-style Spectrum dark: flat charcoal panels, tab strips, blue accents (darkest brightness).
    Pro,
    /// Photoshop's default (second) interface brightness: #535353 panels, #282828 canvas (default).
    #[default]
    ProMedium,
    Studio,
    StudioLight,
    Classic,
}

impl ThemeKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Pro => "builtin.pro",
            Self::ProMedium => "builtin.proMedium",
            Self::Studio => "builtin.studio",
            Self::StudioLight => "builtin.studioLight",
            Self::Classic => "builtin.classic",
        }
    }
    pub const ALL: [ThemeKind; 5] = [ThemeKind::Pro, ThemeKind::ProMedium, ThemeKind::Studio, ThemeKind::StudioLight, ThemeKind::Classic];
    pub fn label(self) -> &'static str {
        match self {
            ThemeKind::Pro => "Pro (Dark)",
            ThemeKind::ProMedium => "Pro (Medium Gray)",
            ThemeKind::Studio => "Studio (Dark)",
            ThemeKind::StudioLight => "Studio (Light)",
            ThemeKind::Classic => "Classic",
        }
    }
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace([' ', '_', '-', '(', ')'], "").as_str() {
            "pro" | "prodark" | "photoshop" | "dark" => Some(ThemeKind::Pro),
            "promedium" | "promediumgray" | "medium" | "mediumgray" => Some(ThemeKind::ProMedium),
            "studio" | "studiodark" => Some(ThemeKind::Studio),
            "studiolight" | "light" => Some(ThemeKind::StudioLight),
            "classic" | "win2000" | "retro" => Some(ThemeKind::Classic),
            _ => None,
        }
    }
}

/// Renderer strategy, independent of the inherited palette/layout defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Appearance {
    #[default]
    Modern,
    Windows98,
    Platinum,
    System6,
}

/// Colour and shape tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub appearance: Appearance,
    pub kind: ThemeKind,
    /// Window chrome (title bar, toolbars).
    pub chrome: Color32,
    /// Canvas surround.
    pub canvas: Color32,
    /// Dot colour of the canvas grid pattern.
    pub canvas_dot: Color32,
    /// Dock background (behind cards).
    pub dock: Color32,
    /// Cards / panels.
    pub card: Color32,
    pub card_border: Color32,
    /// Inputs, fields, dropdowns.
    pub field: Color32,
    pub field_border: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub icon: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub accent_border: Color32,
    pub accent_text: Color32,
    pub separator: Color32,
    pub shadow: Color32,
    pub primary_bg: Color32,
    pub primary_text: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub radius_sm: f32,
    pub radius: f32,
    pub radius_lg: f32,
    /// Classic theme draws 3D bevels instead of flat fills.
    pub bevel: bool,
    /// Raised/sunken edge colors; runtime themes can tune all four bevel tones.
    pub bevel_highlight: Color32,
    pub bevel_light: Color32,
    pub bevel_shadow: Color32,
    pub bevel_dark: Color32,
    /// One edge for the existing Classic theme, two nested edges for period controls.
    pub bevel_width: f32,
    /// Pro (Photoshop-grammar) layout: tab strips, flat panels, checkboxes, pill buttons.
    pub pro: bool,
    /// Panel tab-strip background (Pro).
    pub tab_strip: Color32,
    /// Selected list row (layers, history).
    pub row_selected: Color32,
}

impl Tokens {
    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            // Sampled from Photoshop 2026's default brightness (raw display values).
            ThemeKind::ProMedium => Tokens {
                kind,
                chrome: Color32::from_rgb(83, 83, 83),
                dock: Color32::from_rgb(66, 66, 66),
                card: Color32::from_rgb(83, 83, 83),
                card_border: Color32::from_rgb(66, 66, 66),
                field: Color32::from_rgb(69, 69, 69),
                field_border: Color32::from_rgb(104, 104, 104),
                hover: Color32::from_rgb(98, 98, 98),
                pressed: Color32::from_rgb(112, 112, 112),
                text: Color32::from_rgb(238, 238, 238),
                text_dim: Color32::from_rgb(212, 212, 212),
                text_faint: Color32::from_rgb(160, 160, 160),
                icon: Color32::from_rgb(226, 226, 226),
                accent_soft: Color32::from_rgb(110, 110, 110),
                separator: Color32::from_rgb(62, 62, 62),
                tab_strip: Color32::from_rgb(74, 74, 74),
                row_selected: Color32::from_rgb(107, 107, 107),
                ..Tokens::for_kind(ThemeKind::Pro)
            },
            ThemeKind::Pro => Tokens {
                kind,
                chrome: Color32::from_rgb(50, 50, 50),
                canvas: Color32::from_rgb(40, 40, 40),
                canvas_dot: Color32::from_rgb(40, 40, 40),
                dock: Color32::from_rgb(30, 30, 30),
                card: Color32::from_rgb(50, 50, 50),
                card_border: Color32::from_rgb(30, 30, 30),
                field: Color32::from_rgb(36, 36, 36),
                field_border: Color32::from_rgb(74, 74, 74),
                hover: Color32::from_rgb(66, 66, 66),
                pressed: Color32::from_rgb(78, 78, 78),
                text: Color32::from_rgb(222, 222, 222),
                text_dim: Color32::from_rgb(178, 178, 178),
                text_faint: Color32::from_rgb(128, 128, 128),
                icon: Color32::from_rgb(200, 200, 200),
                accent: Color32::from_rgb(55, 142, 240),
                accent_soft: Color32::from_rgb(78, 78, 78),
                accent_border: Color32::from_rgb(55, 142, 240),
                accent_text: Color32::WHITE,
                separator: Color32::from_rgb(30, 30, 30),
                shadow: Color32::from_black_alpha(150),
                primary_bg: Color32::from_rgb(55, 142, 240),
                primary_text: Color32::WHITE,
                danger: Color32::from_rgb(236, 91, 98),
                warning: Color32::from_rgb(232, 176, 70),
                radius_sm: 3.0,
                radius: 4.0,
                radius_lg: 6.0,
                appearance: Appearance::Modern,
                bevel: false,
                bevel_highlight: Color32::WHITE,
                bevel_light: Color32::from_gray(223),
                bevel_shadow: Color32::from_gray(64),
                bevel_dark: Color32::BLACK,
                bevel_width: 1.0,
                pro: true,
                tab_strip: Color32::from_rgb(38, 38, 38),
                row_selected: Color32::from_rgb(82, 82, 82),
            },
            ThemeKind::Studio => Tokens {
                kind,
                chrome: Color32::from_rgb(20, 20, 21),
                canvas: Color32::from_rgb(14, 14, 15),
                canvas_dot: Color32::from_rgb(46, 46, 50),
                dock: Color32::from_rgb(17, 17, 18),
                card: Color32::from_rgb(26, 26, 28),
                card_border: Color32::from_rgb(40, 40, 44),
                field: Color32::from_rgb(35, 35, 38),
                field_border: Color32::from_rgb(52, 52, 57),
                hover: Color32::from_rgb(44, 44, 48),
                pressed: Color32::from_rgb(56, 56, 62),
                text: Color32::from_rgb(236, 236, 240),
                text_dim: Color32::from_rgb(150, 150, 158),
                text_faint: Color32::from_rgb(96, 96, 104),
                icon: Color32::from_rgb(196, 196, 204),
                accent: Color32::from_rgb(139, 124, 246),
                accent_soft: Color32::from_rgba_unmultiplied(139, 124, 246, 46),
                accent_border: Color32::from_rgba_unmultiplied(160, 148, 255, 110),
                accent_text: Color32::from_rgb(214, 208, 255),
                separator: Color32::from_rgb(38, 38, 42),
                shadow: Color32::from_black_alpha(140),
                primary_bg: Color32::from_rgb(246, 246, 248),
                primary_text: Color32::from_rgb(12, 12, 14),
                danger: Color32::from_rgb(240, 96, 96),
                warning: Color32::from_rgb(240, 190, 90),
                radius_sm: 6.0,
                radius: 8.0,
                radius_lg: 12.0,
                appearance: Appearance::Modern,
                bevel: false,
                bevel_highlight: Color32::WHITE,
                bevel_light: Color32::from_gray(223),
                bevel_shadow: Color32::from_gray(64),
                bevel_dark: Color32::BLACK,
                bevel_width: 1.0,
                pro: false,
                tab_strip: Color32::TRANSPARENT,
                row_selected: Color32::TRANSPARENT,
            },
            ThemeKind::StudioLight => Tokens {
                kind,
                chrome: Color32::from_rgb(246, 246, 248),
                canvas: Color32::from_rgb(226, 226, 230),
                canvas_dot: Color32::from_rgb(200, 200, 206),
                dock: Color32::from_rgb(240, 240, 243),
                card: Color32::from_rgb(252, 252, 253),
                card_border: Color32::from_rgb(222, 222, 228),
                field: Color32::from_rgb(242, 242, 245),
                field_border: Color32::from_rgb(214, 214, 220),
                hover: Color32::from_rgb(232, 232, 237),
                pressed: Color32::from_rgb(220, 220, 226),
                text: Color32::from_rgb(24, 24, 28),
                text_dim: Color32::from_rgb(96, 96, 106),
                text_faint: Color32::from_rgb(150, 150, 160),
                icon: Color32::from_rgb(60, 60, 68),
                accent: Color32::from_rgb(108, 92, 231),
                accent_soft: Color32::from_rgba_unmultiplied(108, 92, 231, 36),
                accent_border: Color32::from_rgba_unmultiplied(108, 92, 231, 120),
                accent_text: Color32::from_rgb(80, 64, 200),
                separator: Color32::from_rgb(226, 226, 232),
                shadow: Color32::from_black_alpha(50),
                primary_bg: Color32::from_rgb(20, 20, 24),
                primary_text: Color32::from_rgb(250, 250, 252),
                danger: Color32::from_rgb(210, 60, 60),
                warning: Color32::from_rgb(190, 130, 20),
                radius_sm: 6.0,
                radius: 8.0,
                radius_lg: 12.0,
                appearance: Appearance::Modern,
                bevel: false,
                bevel_highlight: Color32::WHITE,
                bevel_light: Color32::from_gray(223),
                bevel_shadow: Color32::from_gray(64),
                bevel_dark: Color32::BLACK,
                bevel_width: 1.0,
                pro: false,
                tab_strip: Color32::TRANSPARENT,
                row_selected: Color32::TRANSPARENT,
            },
            ThemeKind::Classic => Tokens {
                kind,
                chrome: Color32::from_rgb(212, 208, 200),
                canvas: Color32::from_rgb(128, 128, 128),
                canvas_dot: Color32::from_rgb(128, 128, 128),
                dock: Color32::from_rgb(212, 208, 200),
                card: Color32::from_rgb(212, 208, 200),
                card_border: Color32::from_rgb(128, 128, 128),
                field: Color32::WHITE,
                field_border: Color32::from_rgb(128, 128, 128),
                hover: Color32::from_rgb(226, 222, 214),
                pressed: Color32::from_rgb(190, 186, 178),
                text: Color32::BLACK,
                text_dim: Color32::from_rgb(64, 64, 64),
                text_faint: Color32::from_rgb(128, 128, 128),
                icon: Color32::BLACK,
                accent: Color32::from_rgb(10, 36, 106),
                accent_soft: Color32::from_rgb(10, 36, 106),
                accent_border: Color32::from_rgb(10, 36, 106),
                accent_text: Color32::WHITE,
                separator: Color32::from_rgb(128, 128, 128),
                shadow: Color32::from_black_alpha(0),
                primary_bg: Color32::from_rgb(212, 208, 200),
                primary_text: Color32::BLACK,
                danger: Color32::from_rgb(160, 0, 0),
                warning: Color32::from_rgb(128, 96, 0),
                radius_sm: 0.0,
                radius: 0.0,
                radius_lg: 0.0,
                appearance: Appearance::Modern,
                bevel: true,
                bevel_highlight: Color32::WHITE,
                bevel_light: Color32::from_gray(223),
                bevel_shadow: Color32::from_gray(64),
                bevel_dark: Color32::BLACK,
                bevel_width: 1.0,
                pro: false,
                tab_strip: Color32::from_rgb(212, 208, 200),
                row_selected: Color32::from_rgb(10, 36, 106),
            },
        }
    }

    /// Tokens for the active theme (stored in egui's context data by [`apply`]).
    pub fn get(ctx: &egui::Context) -> Tokens {
        ctx.data(|d| d.get_temp::<Tokens>(egui::Id::new("photocraft-theme"))).unwrap_or_else(|| Tokens::for_kind(ThemeKind::Studio))
    }

    pub fn dark(&self) -> bool {
        matches!(self.kind, ThemeKind::Studio | ThemeKind::Pro | ThemeKind::ProMedium)
    }
}

/// Register Inter (UI) and JetBrains Mono (numbers) plus named weights.
pub fn install_fonts(ctx: &egui::Context) {
    install_fonts_with(ctx, crate::cjk_fonts::Sources::system());
}

/// [`install_fonts`] with the CJK fallback fonts taken from `cjk` (tests swap the sources).
pub fn install_fonts_with(ctx: &egui::Context, cjk: crate::cjk_fonts::Sources) {
    let mut fonts = FontDefinitions::default();
    let add = |fonts: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add(&mut fonts, "Inter", photocraft_text::fonts::INTER_REGULAR);
    add(&mut fonts, "Inter-Medium", photocraft_text::fonts::INTER_MEDIUM);
    add(&mut fonts, "Inter-SemiBold", photocraft_text::fonts::INTER_SEMIBOLD);
    add(&mut fonts, "JetBrainsMono", photocraft_text::fonts::JETBRAINS_MONO_REGULAR);
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "Inter".to_owned());
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "JetBrainsMono".to_owned());
    // Named weights fall back to the default stack for missing glyphs.
    let fallback: Vec<String> = fonts.families[&FontFamily::Proportional].clone();
    for (fam, primary) in [("medium", "Inter-Medium"), ("semibold", "Inter-SemiBold")] {
        let mut stack = vec![primary.to_owned()];
        stack.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(fam.into()), stack);
    }
    crate::theme_fonts::install(ctx, &mut fonts);
    ctx.set_fonts(fonts);
    // Japanese / Chinese / Korean fallback fonts (craft-fonts' Japanese ones if built in, then
    // the system's) are registered on demand (cjk_fonts.rs).
    crate::cjk_fonts::install_with(ctx, cjk);
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Apply a theme to egui's global style and publish its tokens.
pub fn apply(ctx: &egui::Context, kind: ThemeKind) {
    let t = Tokens::for_kind(kind);
    apply_tokens(ctx, t);
}

/// Apply resolved tokens to both shared widgets and stock egui controls in the same frame.
pub(crate) fn apply_tokens(ctx: &egui::Context, t: Tokens) {
    crate::theme_fonts::set(ctx, None);
    LayoutTokens::for_kind(t.kind).apply(ctx);
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("photocraft-theme"), t));
    let mut v = if t.dark() { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = t.chrome;
    v.window_fill = t.card;
    v.window_stroke = Stroke::new(1.0, t.card_border);
    v.extreme_bg_color = t.field;
    v.faint_bg_color = t.card;
    v.code_bg_color = t.field;
    v.override_text_color = Some(t.text);
    v.hyperlink_color = t.accent;
    v.warn_fg_color = t.warning;
    v.error_fg_color = t.danger;
    v.window_corner_radius = CornerRadius::same(t.radius_lg as u8);
    v.menu_corner_radius = CornerRadius::same(t.radius as u8);
    v.window_shadow = egui::Shadow { offset: [0, 10], blur: 32, spread: 0, color: t.shadow };
    v.popup_shadow = egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: t.shadow };
    v.selection.bg_fill = if t.bevel || t.pro || t.appearance != Appearance::Modern { t.accent } else { t.accent_soft };
    v.selection.stroke = Stroke::new(1.0, t.accent_text);
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.striped = false;
    v.disabled_alpha = if t.appearance == Appearance::System6 { 1.0 } else { 0.5 };
    let r = CornerRadius::same(t.radius_sm as u8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.card;
    w.noninteractive.weak_bg_fill = t.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.separator);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.text_dim);
    w.noninteractive.corner_radius = r;
    for (wv, bg, stroke) in [
        (&mut w.inactive, t.field, t.field_border),
        (&mut w.hovered, t.hover, t.field_border),
        (&mut w.active, t.pressed, t.accent_border),
        (&mut w.open, t.hover, t.field_border),
    ] {
        wv.bg_fill = bg;
        wv.weak_bg_fill = bg;
        wv.bg_stroke = if t.bevel { Stroke::new(1.0, t.bevel_shadow) } else { Stroke::new(1.0, stroke) };
        wv.fg_stroke = Stroke::new(1.0, t.text);
        wv.corner_radius = r;
        wv.expansion = 0.0;
    }
    if t.appearance != Appearance::Modern {
        v.window_shadow = egui::Shadow { offset: [1, 1], blur: 0, spread: 0, color: t.shadow };
        v.popup_shadow = egui::Shadow { offset: [1, 1], blur: 0, spread: 0, color: t.shadow };
    }
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(10.5)),
            (TextStyle::Body, FontId::proportional(if t.pro { 12.0 } else { 12.5 })),
            (TextStyle::Button, FontId::proportional(if t.pro { 12.0 } else { 12.5 })),
            (TextStyle::Heading, semibold(15.0)),
            (TextStyle::Monospace, FontId::monospace(12.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.spacing.interact_size = egui::vec2(24.0, 24.0);
        s.spacing.slider_width = 150.0;
        s.spacing.combo_width = 120.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.window_margin = egui::Margin::same(16);
        s.spacing.icon_width = 14.0;
        s.visuals.indent_has_left_vline = false;
        s.interaction.tooltip_delay = TOOLTIP_DELAY;
        // Thin overlay scrollbars that appear on hover (Photoshop/macOS style).
        s.spacing.scroll = if t.bevel || t.appearance != Appearance::Modern { egui::style::ScrollStyle::solid() } else { egui::style::ScrollStyle::thin() };
        s.spacing.tooltip_width = 280.0;
    });
}

/// Shared layout proportions used by app-drawn controls and chrome.
/// Defaults preserve the existing built-in appearances; runtime packs override these independently.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutTokens {
    pub button_height: f32,
    pub button_font_size: f32,
    pub field_font_size: f32,
    pub tab_font_size: f32,
    pub button_padding_x: f32,
    pub field_height: f32,
    pub tab_height: f32,
    pub panel_padding: f32,
    pub panel_gap: f32,
    pub row_gap: f32,
    pub title_bar_height: f32,
    pub options_bar_height: f32,
    pub status_bar_height: f32,
    pub tool_button_size: f32,
    pub toolbar_padding: f32,
    pub rail_button_size: f32,
    pub preference_spacing: [f32; 2],
}

impl LayoutTokens {
    pub fn for_kind(kind: ThemeKind) -> Self {
        let pro = Tokens::for_kind(kind).pro;
        Self {
            button_height: if pro { 28.0 } else { 30.0 },
            button_font_size: 13.0,
            field_font_size: 12.0,
            tab_font_size: 12.5,
            button_padding_x: 14.0,
            field_height: 24.0,
            tab_height: if pro { 26.0 } else { 24.0 },
            panel_padding: 8.0,
            panel_gap: if pro { 2.0 } else { 6.0 },
            row_gap: ROW_GAP,
            title_bar_height: if pro { 32.0 } else { 38.0 },
            options_bar_height: if pro { 36.0 } else { 42.0 },
            status_bar_height: if pro { 24.0 } else { 30.0 },
            tool_button_size: if pro { 30.0 } else { 36.0 },
            toolbar_padding: if pro { 5.0 } else { 7.0 },
            rail_button_size: if pro { 28.0 } else { 32.0 },
            preference_spacing: [14.0, 7.0],
        }
    }

    pub fn get(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp(egui::Id::new("photocraft-layout"))).unwrap_or_else(|| Self::for_kind(Tokens::get(ctx).kind))
    }

    pub(crate) fn apply(self, ctx: &egui::Context) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("photocraft-layout"), self));
    }
}

/// Seconds the pointer rests on a control before its tooltip shows.
pub const TOOLTIP_DELAY: f32 = 0.35;

/// Vertical gap between stacked control rows in panels (Properties fields, the Layers panel's
/// Opacity and Fill rows); docks zero egui's item spacing, so rows add this themselves.
pub const ROW_GAP: f32 = 4.0;

pub fn canvas_bg(t: &Tokens) -> Color32 {
    t.canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_parse() {
        assert_eq!(ThemeKind::from_name("Classic"), Some(ThemeKind::Classic));
        assert_eq!(ThemeKind::from_name("studio (light)"), Some(ThemeKind::StudioLight));
        assert_eq!(ThemeKind::from_name("dark"), Some(ThemeKind::Pro));
        assert_eq!(ThemeKind::from_name("studio"), Some(ThemeKind::Studio));
        assert_eq!(ThemeKind::from_name("Pro (Medium Gray)"), Some(ThemeKind::ProMedium));
        let m = Tokens::for_kind(ThemeKind::ProMedium);
        assert!(m.pro && m.dark() && m.kind == ThemeKind::ProMedium && m.card == Color32::from_rgb(83, 83, 83));
        assert_eq!(ThemeKind::from_name("neon"), None);
    }

    #[test]
    fn studio_text_contrast_is_high() {
        let t = Tokens::for_kind(ThemeKind::Studio);
        let p = Tokens::for_kind(ThemeKind::Pro);
        assert!(p.pro && !t.pro);
        let lum = |c: Color32| 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
        assert!(lum(t.text) - lum(t.card) > 180.0);
        assert!(lum(t.text_dim) - lum(t.card) > 90.0);
    }
}

/// Native release/debug theme-file adapter. Shared parsing also builds for WASM.
/// Set `PHOTOCRAFT_THEME_FILE` to a versioned theme pack or legacy token file.
#[cfg(not(target_arch = "wasm32"))]
pub mod live {
    use std::io::Read;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use super::ThemeKind;
    use crate::theme_pack::{MAX_THEME_BYTES, ThemePack};

    #[derive(Default)]
    pub struct LiveTokens {
        path: Option<PathBuf>,
        stamp: Option<(SystemTime, u64)>,
        last_check: f64,
        kind: Option<ThemeKind>,
        last_valid: Option<String>,
        last_error: Option<String>,
        pending_error: Option<String>,
    }

    impl LiveTokens {
        pub fn from_env() -> Self {
            Self::from_path(std::env::var_os("PHOTOCRAFT_THEME_FILE").map(Into::into))
        }

        pub fn from_path(path: Option<PathBuf>) -> Self {
            Self { path, last_check: f64::NEG_INFINITY, ..Default::default() }
        }

        /// A new diagnostic only; avoid replacing status every frame for an unchanged bad file.
        pub fn take_error(&mut self) -> Option<String> {
            self.pending_error.take()
        }

        /// The shell has rebuilt the base style; reapply the active file next frame,
        /// even when the user selected the same built-in theme again.
        pub fn invalidate(&mut self) {
            self.kind = None;
        }

        pub fn poll(&mut self, ctx: &egui::Context, kind: ThemeKind) -> bool {
            if self.path.is_none() {
                return false;
            }
            let now = ctx.input(|i| i.time);
            let kind_changed = self.kind != Some(kind);
            ctx.request_repaint_after(Duration::from_secs(2));
            if !kind_changed && now - self.last_check < 2.0 {
                return false;
            }
            self.last_check = now;
            self.kind = Some(kind);
            match self.reload(ctx, kind, kind_changed) {
                Ok(changed) => {
                    self.last_error = None;
                    changed
                }
                Err(error) => {
                    if self.last_error.as_ref() != Some(&error) {
                        self.pending_error = Some(error.clone());
                        self.last_error = Some(error);
                    }
                    // A built-in selection must not discard the last valid file style just
                    // because an editor has temporarily saved incomplete JSON.
                    if kind_changed
                        && let Some(text) = &self.last_valid
                        && let Ok(pack) = ThemePack::parse_file(text, kind)
                        && let Ok(resolved) = pack.resolve()
                    {
                        resolved.apply(ctx);
                        return true;
                    }
                    false
                }
            }
        }

        fn reload(&mut self, ctx: &egui::Context, kind: ThemeKind, force: bool) -> Result<bool, String> {
            let Some(path) = &self.path else { return Ok(false) };
            let metadata = std::fs::metadata(path).map_err(|e| format!("theme file: {e}"))?;
            if !metadata.is_file() {
                return Err("theme path must be a regular file".into());
            }
            if metadata.len() > MAX_THEME_BYTES as u64 {
                return Err(format!("theme exceeds {MAX_THEME_BYTES} bytes"));
            }
            let stamp = (metadata.modified().map_err(|e| format!("theme file timestamp: {e}"))?, metadata.len());
            if !force && self.stamp == Some(stamp) {
                return Ok(false);
            }
            let file = std::fs::File::open(path).map_err(|e| format!("theme file: {e}"))?;
            let mut text = String::new();
            file.take((MAX_THEME_BYTES + 1) as u64).read_to_string(&mut text).map_err(|e| format!("theme file: {e}"))?;
            let pack = ThemePack::parse_file(&text, kind)?;
            let resolved = pack.resolve()?;
            resolved.apply(ctx);
            self.stamp = Some(stamp);
            self.last_valid = Some(text);
            Ok(true)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn failed_save_retains_style_and_recovery_updates_all_controls() {
            let path = std::env::temp_dir().join(format!("photocraft-theme-{}-{:?}.json", std::process::id(), std::thread::current().id()));
            let ctx = egui::Context::default();
            let mut live = LiveTokens::from_path(Some(path.clone()));
            std::fs::write(&path, r##"{"card":"#102030"}"##).unwrap();
            assert!(live.reload(&ctx, ThemeKind::Studio, true).unwrap());
            let before = super::super::Tokens::get(&ctx);
            std::fs::write(&path, "{partial").unwrap();
            assert!(live.reload(&ctx, ThemeKind::Studio, true).is_err());
            assert_eq!(super::super::Tokens::get(&ctx), before);
            assert_eq!(ctx.global_style().visuals.window_fill, before.card);
            std::fs::write(&path, r##"{"card":"#334455"}"##).unwrap();
            assert!(live.reload(&ctx, ThemeKind::Studio, true).unwrap());
            assert_eq!(ctx.global_style().visuals.window_fill, egui::Color32::from_rgb(51, 68, 85));
            std::fs::remove_file(path).unwrap();
        }

        #[test]
        fn selecting_the_same_base_reapplies_the_file_and_bad_edits_keep_the_last_valid_style() {
            let path = std::env::temp_dir().join(format!("photocraft-theme-reselect-{}-{:?}.json", std::process::id(), std::thread::current().id()));
            let ctx = egui::Context::default();
            let mut live = LiveTokens::from_path(Some(path.clone()));
            std::fs::write(&path, r##"{"card":"#102030"}"##).unwrap();
            assert!(live.poll(&ctx, ThemeKind::Studio));
            let expected = super::super::Tokens::get(&ctx);
            super::super::apply(&ctx, ThemeKind::Studio);
            live.invalidate();
            assert!(live.poll(&ctx, ThemeKind::Studio));
            assert_eq!(super::super::Tokens::get(&ctx), expected);
            std::fs::write(&path, "{partial").unwrap();
            super::super::apply(&ctx, ThemeKind::Studio);
            live.invalidate();
            assert!(live.poll(&ctx, ThemeKind::Studio));
            assert_eq!(super::super::Tokens::get(&ctx), expected);
            assert!(live.take_error().is_some());
            assert!(live.take_error().is_none());
            std::fs::remove_file(path).unwrap();
        }
    }
}
