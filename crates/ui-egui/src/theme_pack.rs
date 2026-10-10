//! Versioned runtime theme definitions. Parsing and resolution are platform independent.
//! A definition is fully validated before any egui state changes.

use std::collections::BTreeMap;

use egui::{Color32, FontId, TextStyle};
use serde::{Deserialize, Serialize};

use crate::theme::{ThemeKind, Tokens};

/// Bound allocation and parsing work for files, browser imports and control requests.
pub const MAX_THEME_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemePack {
    pub version: u32,
    /// Stable machine ID; independent of the display name.
    pub id: String,
    pub name: String,
    /// Built-in fallback and defaults. Arbitrary pack inheritance is deliberately unsupported.
    pub base: String,
    #[serde(default)]
    pub appearance: crate::theme::Appearance,
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub geometry: Geometry,
    #[serde(default)]
    pub typography: Typography,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Geometry {
    pub bevel_width: Option<f32>,
    pub radius_small: Option<f32>,
    pub radius: Option<f32>,
    pub radius_large: Option<f32>,
    pub item_spacing: Option<[f32; 2]>,
    pub button_padding: Option<[f32; 2]>,
    pub control_size: Option<[f32; 2]>,
    pub slider_width: Option<f32>,
    pub dropdown_width: Option<f32>,
    pub icon_width: Option<f32>,
    pub button_height: Option<f32>,
    pub field_height: Option<f32>,
    pub scroll_bar_width: Option<f32>,
    pub tab_height: Option<f32>,
    pub panel_padding: Option<f32>,
    pub panel_gap: Option<f32>,
    pub row_gap: Option<f32>,
    pub menu_bar_height: Option<f32>,
    pub document_tab_height: Option<f32>,
    pub layer_row_height: Option<f32>,
    pub layer_thumbnail_size: Option<f32>,
    pub swatch_size: Option<f32>,
    pub color_panel_height: Option<f32>,
    pub title_bar_height: Option<f32>,
    pub options_bar_height: Option<f32>,
    pub status_bar_height: Option<f32>,
    pub tool_button_size: Option<f32>,
    pub toolbar_padding: Option<f32>,
    pub rail_button_size: Option<f32>,
    pub preference_spacing: Option<[f32; 2]>,
    pub menu_margin: Option<f32>,
    pub dialog_margin: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Typography {
    pub family: Option<String>,
    pub small: Option<f32>,
    pub body: Option<f32>,
    pub button: Option<f32>,
    pub heading: Option<f32>,
    pub monospace: Option<f32>,
}

/// Validated, resolved style. Creating it does not alter the current UI.
pub struct ResolvedTheme {
    pub tokens: Tokens,
    geometry: Geometry,
    typography: Typography,
}

impl ThemePack {
    /// Read either a versioned pack or the legacy flat token override format.
    /// Legacy files inherit the current built-in theme, preserving the developer workflow.
    pub fn parse_file(text: &str, fallback: ThemeKind) -> Result<Self, String> {
        if text.len() > MAX_THEME_BYTES {
            return Err(format!("theme exceeds {MAX_THEME_BYTES} bytes"));
        }
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("theme JSON: {e}"))?;
        let object = value.as_object().ok_or("theme must be a JSON object")?;
        if object.contains_key("version") {
            return Self::parse(text);
        }
        let mut pack = Self {
            version: 1,
            id: "legacy.overrides".into(),
            name: "Live token overrides".into(),
            base: fallback.label().into(),
            appearance: Default::default(),
            colors: BTreeMap::new(),
            geometry: Geometry::default(),
            typography: Typography::default(),
        };
        for (key, value) in object {
            let radius = match key.as_str() {
                "radius_sm" => Some(&mut pack.geometry.radius_small),
                "radius" => Some(&mut pack.geometry.radius),
                "radius_lg" => Some(&mut pack.geometry.radius_large),
                _ => None,
            };
            if let Some(radius) = radius {
                *radius = Some(value.as_f64().ok_or_else(|| format!("{key}: expected a number"))? as f32);
            } else {
                pack.colors.insert(key.clone(), value.as_str().ok_or_else(|| format!("{key}: expected a color string"))?.into());
            }
        }
        pack.resolve()?;
        Ok(pack)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_THEME_BYTES {
            return Err(format!("theme exceeds {MAX_THEME_BYTES} bytes"));
        }
        let pack: Self = serde_json::from_str(text).map_err(|e| format!("theme JSON: {e}"))?;
        pack.resolve()?;
        Ok(pack)
    }

    pub fn resolve(&self) -> Result<ResolvedTheme, String> {
        if self.version != 1 {
            return Err(format!("unsupported theme version {} (expected 1)", self.version));
        }
        if self.id.is_empty() || self.id.len() > 64 || !self.id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')) {
            return Err("theme id must contain 1–64 ASCII letters, digits, dots, underscores or hyphens".into());
        }
        if self.name.trim().is_empty() || self.name.len() > 128 || self.name.chars().any(char::is_control) {
            return Err("theme name must contain 1–128 bytes of printable text".into());
        }
        let kind = ThemeKind::from_name(&self.base).ok_or_else(|| format!("unknown theme base `{}`", self.base))?;
        let mut tokens = Tokens::for_kind(kind);
        tokens.appearance = self.appearance;
        if self.appearance != crate::theme::Appearance::Modern {
            tokens.pro = false;
            tokens.bevel = self.appearance != crate::theme::Appearance::System6;
        }
        for (key, value) in &self.colors {
            set_color(&mut tokens, key, parse_color(value).ok_or_else(|| format!("colors.{key}: expected #RRGGBB or #RRGGBBAA"))?)?;
        }
        let g = &self.geometry;
        validate_number("bevelWidth", g.bevel_width, 1.0, 2.0)?;
        for (name, value) in [("radiusSmall", g.radius_small), ("radius", g.radius), ("radiusLarge", g.radius_large)] {
            validate_number(name, value, 0.0, 32.0)?;
        }
        for (name, value) in [("itemSpacing", g.item_spacing), ("buttonPadding", g.button_padding)] {
            validate_pair(name, value, 0.0, 64.0)?;
        }
        validate_pair("controlSize", g.control_size, 8.0, 128.0)?;
        for (name, value) in [("sliderWidth", g.slider_width), ("dropdownWidth", g.dropdown_width)] {
            validate_number(name, value, 16.0, 1024.0)?;
        }
        validate_number("iconWidth", g.icon_width, 8.0, 64.0)?;
        validate_number("scrollBarWidth", g.scroll_bar_width, 12.0, 32.0)?;
        validate_number("layerRowHeight", g.layer_row_height, 24.0, 96.0)?;
        validate_number("layerThumbnailSize", g.layer_thumbnail_size, 8.0, 64.0)?;
        validate_number("swatchSize", g.swatch_size, 8.0, 48.0)?;
        validate_number("colorPanelHeight", g.color_panel_height, 80.0, 600.0)?;
        for (name, value) in [
            ("buttonHeight", g.button_height),
            ("fieldHeight", g.field_height),
            ("tabHeight", g.tab_height),
            ("menuBarHeight", g.menu_bar_height),
            ("documentTabHeight", g.document_tab_height),
            ("titleBarHeight", g.title_bar_height),
            ("optionsBarHeight", g.options_bar_height),
            ("statusBarHeight", g.status_bar_height),
            ("toolButtonSize", g.tool_button_size),
            ("railButtonSize", g.rail_button_size),
        ] {
            validate_number(name, value, 16.0, 96.0)?;
        }
        for (name, value) in [
            ("panelPadding", g.panel_padding),
            ("panelGap", g.panel_gap),
            ("rowGap", g.row_gap),
            ("toolbarPadding", g.toolbar_padding),
            ("menuMargin", g.menu_margin),
            ("dialogMargin", g.dialog_margin),
        ] {
            validate_number(name, value, 0.0, 32.0)?;
        }
        validate_pair("preferenceSpacing", g.preference_spacing, 0.0, 32.0)?;
        let ty = &self.typography;
        if let Some(family) = ty.family.as_deref()
            && !crate::theme_fonts::FAMILIES.contains(&family)
        {
            return Err(format!("unsupported UI font family `{family}`"));
        }
        for (name, value) in [("small", ty.small), ("body", ty.body), ("button", ty.button), ("heading", ty.heading), ("monospace", ty.monospace)] {
            validate_number(name, value, 6.0, 64.0)?;
        }
        if let Some(v) = g.radius_small {
            tokens.radius_sm = v;
        }
        if let Some(v) = g.bevel_width {
            tokens.bevel_width = v;
        }
        if let Some(v) = g.radius {
            tokens.radius = v;
        }
        if let Some(v) = g.radius_large {
            tokens.radius_lg = v;
        }
        Ok(ResolvedTheme { tokens, geometry: g.clone(), typography: ty.clone() })
    }
}

impl ResolvedTheme {
    pub fn apply(&self, ctx: &egui::Context) {
        crate::theme::apply_tokens(ctx, self.tokens);
        let g = &self.geometry;
        let mut layout = crate::theme::LayoutTokens::for_kind(self.tokens.kind);
        if self.tokens.appearance != crate::theme::Appearance::Modern {
            layout.scroll_bar_width = 16.0;
        }
        macro_rules! apply_layout {
            ($($field:ident),*) => { $(if let Some(v) = g.$field { layout.$field = v; })* };
        }
        apply_layout!(
            button_height,
            field_height,
            scroll_bar_width,
            tab_height,
            panel_padding,
            panel_gap,
            row_gap,
            menu_bar_height,
            document_tab_height,
            layer_row_height,
            layer_thumbnail_size,
            swatch_size,
            color_panel_height,
            title_bar_height,
            options_bar_height,
            status_bar_height,
            tool_button_size,
            toolbar_padding,
            rail_button_size,
            preference_spacing
        );
        if let Some([x, _]) = g.button_padding {
            layout.button_padding_x = x;
        }
        if let Some(v) = self.typography.button {
            layout.button_font_size = v;
        }
        if let Some(v) = self.typography.monospace {
            layout.field_font_size = v;
        }
        if let Some(v) = self.typography.body {
            layout.tab_font_size = v;
        }
        // Text still fits if a user chooses a large type size with compact geometry.
        layout.button_height = layout.button_height.max(layout.button_font_size + 6.0);
        layout.field_height = layout.field_height.max(layout.field_font_size + 8.0);
        layout.tab_height = layout.tab_height.max(layout.tab_font_size + 6.0);
        layout.document_tab_height = layout.document_tab_height.max(layout.tab_font_size + 6.0);
        layout.layer_row_height = layout.layer_row_height.max(layout.tab_font_size + 6.0);
        layout.layer_thumbnail_size = layout.layer_thumbnail_size.min(layout.layer_row_height - 4.0);
        if self.tokens.appearance != crate::theme::Appearance::Modern {
            layout.menu_bar_height = layout.menu_bar_height.max(layout.tab_font_size + 6.0);
            layout.options_bar_height = layout.options_bar_height.max(layout.field_height + 2.0).max(layout.tab_font_size + 6.0);
            layout.status_bar_height = layout.status_bar_height.max(layout.field_height + 2.0).max(layout.tab_font_size + 4.0);
        }
        layout.apply(ctx);
        crate::theme_fonts::set(ctx, self.typography.family.as_deref());
        ctx.global_style_mut(|s| {
            let g = &self.geometry;
            if let Some([x, y]) = g.item_spacing {
                s.spacing.item_spacing = egui::vec2(x, y);
            }
            if let Some([x, y]) = g.button_padding {
                s.spacing.button_padding = egui::vec2(x, y);
            }
            if let Some([x, y]) = g.control_size {
                s.spacing.interact_size = egui::vec2(x, y);
            }
            if let Some(v) = g.slider_width {
                s.spacing.slider_width = v;
            }
            if let Some(v) = g.dropdown_width {
                s.spacing.combo_width = v;
            }
            if let Some(v) = g.icon_width {
                s.spacing.icon_width = v;
            }
            if let Some(v) = g.menu_margin {
                s.spacing.menu_margin = egui::Margin::same(v.round() as i8);
            }
            if let Some(v) = g.dialog_margin {
                s.spacing.window_margin = egui::Margin::same(v.round() as i8);
            }
            let ty = &self.typography;
            if let Some(family) = &ty.family {
                for font in s.text_styles.values_mut() {
                    font.family = egui::FontFamily::Name(family.clone().into());
                }
            }
            if g.scroll_bar_width.is_some() || self.tokens.appearance != crate::theme::Appearance::Modern {
                s.spacing.scroll.bar_width = layout.scroll_bar_width;
            }
            if self.tokens.appearance != crate::theme::Appearance::Modern {
                s.spacing.scroll.handle_min_length = layout.scroll_bar_width;
                s.spacing.scroll.bar_inner_margin = 0.0;
                s.spacing.scroll.bar_outer_margin = 0.0;
                s.spacing.scroll.fade.strength = 0.0;
            }
            for (style, size) in [
                (TextStyle::Small, ty.small),
                (TextStyle::Body, ty.body),
                (TextStyle::Button, ty.button),
                (TextStyle::Heading, ty.heading),
                (TextStyle::Monospace, ty.monospace),
            ] {
                if let Some(size) = size {
                    let font = s.text_styles.get(&style).cloned().unwrap_or_else(|| FontId::proportional(size));
                    s.text_styles.insert(style, FontId::new(size, font.family));
                }
            }
        });
        ctx.request_repaint();
    }
}

fn validate_number(name: &str, value: Option<f32>, min: f32, max: f32) -> Result<(), String> {
    if value.is_some_and(|v| !v.is_finite() || !(min..=max).contains(&v)) {
        return Err(format!("{name}: expected a finite number in {min}..={max}"));
    }
    Ok(())
}

fn validate_pair(name: &str, value: Option<[f32; 2]>, min: f32, max: f32) -> Result<(), String> {
    if let Some([x, y]) = value {
        validate_number(name, Some(x), min, max)?;
        validate_number(name, Some(y), min, max)?;
    }
    Ok(())
}

fn parse_color(value: &str) -> Option<Color32> {
    let hex = value.strip_prefix('#')?;
    let component = |i: usize| u8::from_str_radix(hex.get(i..i.checked_add(2)?)?, 16).ok();
    match hex.len() {
        6 => Some(Color32::from_rgb(component(0)?, component(2)?, component(4)?)),
        8 => Some(Color32::from_rgba_unmultiplied(component(0)?, component(2)?, component(4)?, component(6)?)),
        _ => None,
    }
}

fn set_color(tokens: &mut Tokens, key: &str, color: Color32) -> Result<(), String> {
    macro_rules! fields {
        ($($field:ident),* $(,)?) => {
            match key {
                $(stringify!($field) => tokens.$field = color,)*
                _ => return Err(format!("unknown theme color `{key}`")),
            }
        };
    }
    fields!(
        chrome,
        canvas,
        canvas_dot,
        dock,
        card,
        card_border,
        field,
        field_border,
        hover,
        pressed,
        text,
        text_dim,
        text_faint,
        icon,
        accent,
        accent_soft,
        accent_border,
        accent_text,
        separator,
        shadow,
        primary_bg,
        primary_text,
        danger,
        warning,
        tab_strip,
        row_selected,
        bevel_highlight,
        bevel_light,
        bevel_shadow,
        bevel_dark
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ThemePack {
        ThemePack {
            version: 1,
            id: "custom.silver".into(),
            name: "Custom Silver".into(),
            base: "classic".into(),
            appearance: Default::default(),
            colors: BTreeMap::new(),
            geometry: Geometry::default(),
            typography: Typography::default(),
        }
    }

    #[test]
    fn chrome_metrics_round_trip_validate_and_restore_modern_defaults() {
        let ctx = egui::Context::default();
        for id in crate::historical::IDS {
            let mut pack = crate::historical::preset(id).unwrap();
            pack.geometry.menu_bar_height = Some(17.0);
            pack.geometry.document_tab_height = Some(19.0);
            pack.geometry.layer_row_height = Some(26.0);
            pack.geometry.layer_thumbnail_size = Some(64.0);
            let text = serde_json::to_string(&pack).unwrap();
            let restored = ThemePack::parse(&text).unwrap();
            assert_eq!(restored, pack);
            restored.resolve().unwrap().apply(&ctx);
            let layout = crate::theme::LayoutTokens::get(&ctx);
            assert_eq!(layout.menu_bar_height, 17.0_f32.max(layout.tab_font_size + 6.0));
            assert_eq!(layout.document_tab_height, 19.0);
            assert_eq!(layout.layer_row_height, 26.0);
            assert_eq!(layout.layer_thumbnail_size, 22.0);
            pack.geometry.layer_row_height = Some(23.0);
            assert!(pack.resolve().is_err());
            pack.geometry.layer_row_height = Some(26.0);
            pack.geometry.document_tab_height = Some(f32::NAN);
            assert!(pack.resolve().is_err());
        }
        crate::theme::apply(&ctx, ThemeKind::Classic);
        let layout = crate::theme::LayoutTokens::get(&ctx);
        assert_eq!(layout.document_tab_height, 26.0);
        assert_eq!(layout.layer_row_height, 46.0);
        assert_eq!(layout.layer_thumbnail_size, 34.0);
    }
    #[test]
    fn updates_stock_controls_and_custom_tokens_together() {
        let mut pack = fixture();
        pack.colors.insert("card".into(), "#112233".into());
        pack.colors.insert("field".into(), "#ddeeff".into());
        pack.geometry.radius = Some(7.0);
        pack.geometry.control_size = Some([18.0, 19.0]);
        pack.geometry.bevel_width = Some(2.0);
        pack.colors.insert("bevel_shadow".into(), "#804020".into());
        pack.typography.body = Some(11.0);
        let ctx = egui::Context::default();
        pack.resolve().unwrap().apply(&ctx);
        assert_eq!(Tokens::get(&ctx).card, Color32::from_rgb(17, 34, 51));
        let style = ctx.global_style();
        assert_eq!(style.visuals.window_fill, Tokens::get(&ctx).card);
        assert_eq!(style.visuals.extreme_bg_color, Tokens::get(&ctx).field);
        assert_eq!(style.visuals.menu_corner_radius, egui::CornerRadius::same(7));
        assert_eq!(style.spacing.interact_size, egui::vec2(18.0, 19.0));
        assert_eq!(style.text_styles[&TextStyle::Body].size, 11.0);
        assert_eq!(Tokens::get(&ctx).bevel_width, 2.0);
        assert_eq!(style.visuals.widgets.inactive.bg_stroke.color, Tokens::get(&ctx).bevel_shadow);
    }

    #[test]
    fn runtime_geometry_reaches_drawn_controls_and_resets_when_switching_themes() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let measured = Rc::new(RefCell::new(Vec::new()));
        let capture = measured.clone();
        let mut initialized = false;
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(400.0, 300.0)).build_ui(move |ui| {
            if !initialized {
                crate::PhotocraftApp::setup_context(ui.ctx(), ThemeKind::Classic);
                initialized = true;
                return;
            }
            let mut value = 5.0;
            let field = crate::widgets::value_field(ui, &mut value, 0.0..=10.0, "px", 80.0);
            let button = crate::widgets::secondary_button(ui, "Apply", 0.0);
            *capture.borrow_mut() = vec![field.rect, button.rect];
        });
        let mut pack = fixture();
        pack.geometry.field_height = Some(20.0);
        pack.geometry.button_height = Some(23.0);
        pack.geometry.button_padding = Some([8.0, 2.0]);
        pack.typography.monospace = Some(11.0);
        pack.typography.button = Some(11.0);
        for scale in [1.0, 1.5, 2.0] {
            harness.ctx.set_pixels_per_point(scale);
            pack.resolve().unwrap().apply(&harness.ctx);
            harness.run_steps(3);
            let rects = measured.borrow();
            // Numeric interaction is inside its painted field and successive controls don't overlap.
            assert!(rects[0].height() <= 20.0);
            assert!((rects[1].height() - 23.0).abs() < 0.1);
            assert!(rects[0].bottom() < rects[1].top());
        }
        crate::theme::apply(&harness.ctx, ThemeKind::Classic);
        harness.run_steps(3);
        assert!((measured.borrow()[1].height() - 30.0).abs() < 0.1);
    }

    #[test]
    fn geometry_limits_and_large_text_do_not_allow_clipped_custom_controls() {
        for field in [
            "buttonHeight",
            "fieldHeight",
            "scrollBarWidth",
            "tabHeight",
            "panelPadding",
            "panelGap",
            "rowGap",
            "titleBarHeight",
            "optionsBarHeight",
            "statusBarHeight",
            "toolButtonSize",
            "toolbarPadding",
            "railButtonSize",
            "menuMargin",
            "dialogMargin",
        ] {
            for value in [-1.0, 200.0] {
                let text = serde_json::json!({"version":1,"id":"test","name":"Test","base":"classic","geometry":{field:value}}).to_string();
                assert!(ThemePack::parse(&text).is_err(), "{field}={value}");
            }
        }
        let mut pack = fixture();
        pack.geometry.button_height = Some(16.0);
        pack.geometry.field_height = Some(16.0);
        pack.typography.button = Some(24.0);
        pack.typography.monospace = Some(24.0);
        let ctx = egui::Context::default();
        pack.resolve().unwrap().apply(&ctx);
        let layout = crate::theme::LayoutTokens::get(&ctx);
        assert!(layout.button_height >= 30.0 && layout.field_height >= 32.0);
    }

    #[test]
    fn rejects_invalid_input_before_modifying_context() {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx, ThemeKind::Studio);
        let before = Tokens::get(&ctx);
        let mut pack = fixture();
        pack.colors.insert("card".into(), "#112233".into());
        pack.geometry.radius = Some(f32::NAN);
        assert!(pack.resolve().is_err());
        assert_eq!(Tokens::get(&ctx), before);
        pack.geometry.radius = Some(-1.0);
        assert!(pack.resolve().is_err());
        pack.geometry.radius = None;
        pack.colors.insert("typo".into(), "#ffffff".into());
        assert!(pack.resolve().is_err());
        assert!(ThemePack::parse(&" ".repeat(MAX_THEME_BYTES + 1)).is_err());
        assert!(ThemePack::parse(r#"{"version":2,"id":"x","name":"X","base":"classic"}"#).is_err());
        assert!(ThemePack::parse(r#"{"version":1,"id":"x","name":"X","base":"classic","typo":true}"#).is_err());
    }

    #[test]
    fn colors_are_strict_and_unicode_safe() {
        for text in ["ffffff", "#fff", "#ééé", "#12zz56", "#1122334455"] {
            assert!(parse_color(text).is_none(), "{text}");
        }
        assert_eq!(parse_color("#123456"), Some(Color32::from_rgb(18, 52, 86)));
        assert_eq!(parse_color("#12345680"), Some(Color32::from_rgba_unmultiplied(18, 52, 86, 128)));
    }

    #[test]
    fn serialized_packs_round_trip_and_keep_base_defaults() {
        let pack = fixture();
        let restored = ThemePack::parse(&serde_json::to_string(&pack).unwrap()).unwrap();
        assert_eq!(restored, pack);
        assert_eq!(restored.resolve().unwrap().tokens, Tokens::for_kind(ThemeKind::Classic));
    }

    #[test]
    fn legacy_files_use_the_same_validated_pipeline() {
        let pack = ThemePack::parse_file(r##"{"card":"#102030","radius":9}"##, ThemeKind::Studio).unwrap();
        assert_eq!(pack.resolve().unwrap().tokens.card, Color32::from_rgb(16, 32, 48));
        assert_eq!(pack.resolve().unwrap().tokens.radius, 9.0);
        assert!(ThemePack::parse_file(r#"{"radius":-1}"#, ThemeKind::Studio).is_err());
        assert!(ThemePack::parse_file(r#"{"unknown":true}"#, ThemeKind::Studio).is_err());
        assert!(ThemePack::parse_file("[]", ThemeKind::Studio).is_err());
    }
}
