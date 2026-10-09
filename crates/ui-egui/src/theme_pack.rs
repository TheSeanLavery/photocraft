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
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Typography {
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
        let ty = &self.typography;
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
            let ty = &self.typography;
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
            colors: BTreeMap::new(),
            geometry: Geometry::default(),
            typography: Typography::default(),
        }
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
