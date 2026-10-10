//! Runtime UI family selection. Installed fonts are read locally, never copied into this repo.
use egui::{Context, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

include!(concat!(env!("OUT_DIR"), "/theme_fonts.rs"));

fn embedded_font(family: &str) -> Option<FontData> {
    let (_, bytes, _) = EMBEDDED_THEME_FONTS.iter().find(|(name, _, _)| *name == family)?;
    checked_font(bytes.to_vec(), family)
}

pub const FAMILIES: [&str; 8] = ["Inter", "JetBrains Mono", "Microsoft Sans Serif", "Geneva", "Chicago", "Charcoal", "Chicago Kare", "W95FA"];

#[derive(Clone, Debug)]
struct Families {
    resolved: Arc<Vec<(String, String)>>,
}

pub(crate) fn install(ctx: &Context, fonts: &mut FontDefinitions) {
    let mut resolved: Vec<(String, String)> = vec![("Inter".into(), "Inter".into()), ("JetBrains Mono".into(), "JetBrainsMono".into())];
    for family in ["Microsoft Sans Serif", "Geneva", "Chicago Kare", "W95FA"] {
        let loaded = load(family);
        if let Some(data) = loaded {
            fonts.font_data.insert(family.into(), Arc::new(data));
        }
        let actual = if fonts.font_data.contains_key(family) {
            family
        } else if family == "W95FA" && fonts.font_data.contains_key("Microsoft Sans Serif") {
            "Microsoft Sans Serif"
        } else {
            "Inter"
        };
        resolved.push((family.into(), actual.into()));
    }
    if fonts.font_data.contains_key("W95FA") {
        for (requested, actual) in &mut resolved {
            if requested == "Microsoft Sans Serif" && actual == "Inter" {
                *actual = "W95FA".into();
            }
        }
    }
    let mac = if fonts.font_data.contains_key("Geneva") { "Geneva" } else { "Inter" };
    // The supplied machine may not contain these historical faces. Never silently claim them.
    for family in ["Chicago", "Charcoal"] {
        if let Some(data) = load(family) {
            fonts.font_data.insert(family.into(), Arc::new(data));
        }
        let actual = if fonts.font_data.contains_key(family) {
            family
        } else if family == "Chicago" && fonts.font_data.contains_key("Chicago Kare") {
            "Chicago Kare"
        } else {
            mac
        };
        resolved.push((family.into(), actual.into()));
    }
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (requested, actual) in &resolved {
        let mut stack = vec![actual.clone()];
        stack.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(requested.clone().into()), stack);
    }
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("theme-font-families"), Families { resolved: Arc::new(resolved) }));
}

#[cfg(not(target_arch = "wasm32"))]
fn load(family: &str) -> Option<FontData> {
    use std::sync::OnceLock;
    static WINDOWS: OnceLock<Option<FontData>> = OnceLock::new();
    static GENEVA: OnceLock<Option<FontData>> = OnceLock::new();
    static CHICAGO: OnceLock<Option<FontData>> = OnceLock::new();
    static CHARCOAL: OnceLock<Option<FontData>> = OnceLock::new();
    static CHICAGO_KARE: OnceLock<Option<FontData>> = OnceLock::new();
    static W95FA: OnceLock<Option<FontData>> = OnceLock::new();
    let cache = match family {
        "Microsoft Sans Serif" => &WINDOWS,
        "Geneva" => &GENEVA,
        "Chicago" => &CHICAGO,
        "Charcoal" => &CHARCOAL,
        "Chicago Kare" => &CHICAGO_KARE,
        "W95FA" => &W95FA,
        _ => return None,
    };
    cache.get_or_init(|| read_font(family).or_else(|| embedded_font(family))).clone()
}

#[cfg(not(target_arch = "wasm32"))]
fn read_font(family: &str) -> Option<FontData> {
    use std::path::PathBuf;
    // Known installed-font locations only; theme JSON cannot request arbitrary file access.
    let mut candidates = vec![
        PathBuf::from(format!("/System/Library/Fonts/{family}.ttf")),
        PathBuf::from(format!("/System/Library/Fonts/Supplemental/{family}.ttf")),
        PathBuf::from(format!("/Library/Fonts/{family}.ttf")),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let root = PathBuf::from(home);
        if cfg!(target_os = "macos") {
            candidates.push(root.join("Library/Fonts").join(format!("{family}.ttf")));
        }
        if cfg!(target_os = "linux") {
            candidates.push(root.join(".local/share/fonts").join(format!("{family}.ttf")));
        }
    }
    if family == "Microsoft Sans Serif"
        && let Some(root) = std::env::var_os("WINDIR")
    {
        candidates.push(PathBuf::from(root).join("Fonts/micross.ttf"));
    }
    if family == "Chicago Kare" {
        if let Some(root) = std::env::var_os("PHOTOCRAFT_THEME_FONTS_DIR") {
            candidates.insert(0, PathBuf::from(root).join("fonts/chicago-kare/ChicagoKare-Regular.ttf"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            let root = PathBuf::from(home);
            candidates.push(root.join(if cfg!(target_os = "macos") { "Library/Fonts" } else { ".local/share/fonts" }).join("ChicagoKare-Regular.ttf"));
        }
    }
    if family == "W95FA" {
        if let Some(root) = std::env::var_os("PHOTOCRAFT_THEME_FONTS_DIR") {
            candidates.insert(0, PathBuf::from(root).join("fonts/w95fa/W95FA-Regular.otf"));
        }
        let mut directories =
            vec![PathBuf::from("/System/Library/Fonts"), PathBuf::from("/System/Library/Fonts/Supplemental"), PathBuf::from("/Library/Fonts")];
        if let Some(home) = std::env::var_os("HOME") {
            directories.push(PathBuf::from(home).join(if cfg!(target_os = "macos") { "Library/Fonts" } else { ".local/share/fonts" }));
        }
        if let Some(root) = std::env::var_os("WINDIR") {
            directories.push(PathBuf::from(root).join("Fonts"));
        }
        if let Some(root) = std::env::var_os("LOCALAPPDATA") {
            directories.push(PathBuf::from(root).join("Microsoft/Windows/Fonts"));
        }
        for directory in directories {
            for file in ["W95FA-Regular.otf", "W95FA.otf"] {
                candidates.push(directory.join(file));
            }
        }
    }
    candidates.iter().find_map(|path| read_installed_font(path, family))
}

#[cfg(not(target_arch = "wasm32"))]
fn read_installed_font(path: &std::path::Path, family: &str) -> Option<FontData> {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else { return None };
    let Ok(meta) = file.metadata() else { return None };
    const LIMIT: u64 = 8 * 1024 * 1024;
    if !meta.is_file() || meta.len() == 0 || meta.len() > LIMIT {
        return None;
    }
    let mut bytes = Vec::new();
    if file.take(LIMIT + 1).read_to_end(&mut bytes).is_err() || bytes.len() > LIMIT as usize {
        return None;
    }
    // Use the shared font parser before passing installed bytes to egui.
    checked_font(bytes, family)
}

fn checked_font(bytes: Vec<u8>, family: &str) -> Option<FontData> {
    if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
        return None;
    }
    if !photocraft_text::fonts::FontDb::new().register_font_data(bytes.clone()).iter().any(|name| name.eq_ignore_ascii_case(family)) {
        return None;
    }
    Some(FontData::from_owned(bytes))
}
#[cfg(target_arch = "wasm32")]
fn load(family: &str) -> Option<FontData> {
    use std::sync::OnceLock;
    static KARE: OnceLock<Option<FontData>> = OnceLock::new();
    static WINDOWS: OnceLock<Option<FontData>> = OnceLock::new();
    let cache = match family {
        "Chicago Kare" => &KARE,
        "W95FA" => &WINDOWS,
        _ => return None,
    };
    cache.get_or_init(|| embedded_font(family)).clone()
}

/// License notices travel inside binaries using the optional historical UI subset.
pub(crate) fn license_notices(ui: &mut egui::Ui) {
    if EMBEDDED_THEME_FONTS.is_empty() {
        return;
    }
    ui.collapsing("Historical UI font licenses", |ui| {
        for (family, _, license) in EMBEDDED_THEME_FONTS {
            ui.collapsing(*family, |ui| {
                egui::ScrollArea::vertical().id_salt(("theme-font-license", family)).max_height(180.0).show(ui, |ui| {
                    ui.add(egui::Label::new(*license).wrap().selectable(true));
                });
            });
        }
    });
}

pub fn status(ctx: &Context) -> Option<(String, String)> {
    let requested: String = ctx.data(|d| d.get_temp(egui::Id::new("theme-font-family")))?;
    let actual = ctx.data(|d| d.get_temp::<Families>(egui::Id::new("theme-font-families")))?.resolved.iter().find(|(name, _)| name == &requested)?.1.clone();
    Some((requested, actual))
}

pub(crate) fn set(ctx: &Context, family: Option<&str>) {
    ctx.data_mut(|d| {
        let id = egui::Id::new("theme-font-family");
        if let Some(name) = family {
            d.insert_temp(id, name.to_string());
        } else {
            d.remove::<String>(id);
        }
    });
}

/// Chicago Kare's 64-unit grid in a 1024-unit em needs 16 points for one
/// logical pixel per grid cell. Only the Chicago compatibility alias uses this
/// calibration; explicitly selected Chicago Kare keeps its raw point sizes.
fn chicago_recreation_alias(ctx: &Context, font: &FontId) -> bool {
    matches!(&font.family, FontFamily::Name(name) if name.as_ref() == "Chicago")
        && ctx
            .data(|d| d.get_temp::<Families>(egui::Id::new("theme-font-families")))
            .is_some_and(|families| families.resolved.iter().any(|(requested, actual)| requested == "Chicago" && actual == "Chicago Kare"))
}

/// W95FA's approximately 80-unit cells in its 1000-unit em fit one logical
/// pixel at 12.5 points. Correct the 11-point compatibility default only when
/// it resolves to the recreation; explicit W95FA sizes remain raw.
fn windows_recreation_alias(ctx: &Context, font: &FontId) -> bool {
    matches!(&font.family, FontFamily::Name(name) if name.as_ref() == "Microsoft Sans Serif")
        && ctx
            .data(|d| d.get_temp::<Families>(egui::Id::new("theme-font-families")))
            .is_some_and(|families| families.resolved.iter().any(|(requested, actual)| requested == "Microsoft Sans Serif" && actual == "W95FA"))
}

pub(crate) fn calibrated(ctx: &Context, mut font: FontId) -> FontId {
    if chicago_recreation_alias(ctx, &font) {
        font.size *= 4.0 / 3.0;
    } else if windows_recreation_alias(ctx, &font) {
        font.size *= 12.5 / 11.0;
    }
    font
}

/// Single-line chrome uses the recreation's em box rather than its extra 0.2-em
/// interline gap. Glyph advances and raster size still use the calibrated font.
pub(crate) fn chrome_line_height(ctx: &Context, font: &FontId) -> Option<f32> {
    chicago_recreation_alias(ctx, font).then_some(font.size)
}

pub(crate) fn control(ctx: &Context, size: f32, bold: bool) -> FontId {
    if let Some(name) = ctx.data(|d| d.get_temp::<String>(egui::Id::new("theme-font-family"))) {
        calibrated(ctx, FontId::new(size, FontFamily::Name(name.into())))
    } else if bold {
        crate::theme::semibold(size)
    } else {
        crate::theme::medium(size)
    }
}

pub(crate) fn number(ctx: &Context, size: f32) -> FontId {
    if ctx.data(|d| d.get_temp::<String>(egui::Id::new("theme-font-family"))).is_some() { control(ctx, size, false) } else { crate::theme::mono(size) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires PHOTOCRAFT_THEME_FONTS_BUILD_DIR at build time"]
    fn bundled_license_notices_can_be_opened_and_stay_in_the_about_dialog() {
        use egui_kittest::kittest::Queryable;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1280.0, 900.0)).build_eframe(|cc| {
            crate::PhotocraftApp::setup_context(&cc.egui_ctx, crate::theme::ThemeKind::Classic);
            crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default())
        });
        h.state_mut().ui.open_dialog(crate::state::DialogKind::About, Default::default());
        h.run_steps(4);
        h.get_by_label("Historical UI font licenses").click();
        h.run_steps(3);
        h.get_by_label("W95FA").click();
        h.run_steps(3);
        let ok = h.get_by_label("OK").rect();
        assert!(ok.bottom() <= 900.0 && ok.right() <= 1280.0, "{ok:?}");
        assert!(h.output().shapes.iter().any(|s| match &s.shape {
            egui::epaint::Shape::Text(t) => t.galley.job.text.contains("SIL OPEN FONT LICENSE Version 1.1"),
            _ => false,
        }));
    }

    #[test]
    #[ignore = "requires PHOTOCRAFT_THEME_FONTS_BUILD_DIR at build time"]
    fn bundled_historical_faces_and_notices_are_available_without_runtime_checkout() {
        assert_eq!(EMBEDDED_THEME_FONTS.len(), 2);
        assert_eq!(EMBEDDED_THEME_FONTS.iter().map(|(_, bytes, _)| bytes.len()).sum::<usize>(), 62_892);
        for (family, bytes, license) in EMBEDDED_THEME_FONTS {
            assert!(checked_font(bytes.to_vec(), family).is_some());
            assert!(license.len() > 100);
        }
        let ctx = Context::default();
        crate::theme::install_fonts(&ctx);
        for family in ["Chicago Kare", "W95FA"] {
            set(&ctx, Some(family));
            assert_eq!(status(&ctx), Some((family.into(), family.into())));
        }
    }

    #[test]
    fn chicago_recreation_calibrates_alias_styles_without_double_scaling() {
        let ctx = Context::default();
        ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new("theme-font-families"),
                Families { resolved: Arc::new(vec![("Chicago".into(), "Chicago Kare".into()), ("Chicago Kare".into(), "Chicago Kare".into())]) },
            )
        });
        let pack = crate::theme_pack::ThemePack::parse(include_str!("../../../assets/themes/system-6.json")).unwrap().resolve().unwrap();
        for _ in 0..3 {
            pack.apply(&ctx);
            assert_eq!(control(&ctx, 12.0, false).size, 16.0);
            assert_eq!(number(&ctx, 12.0).size, 16.0);
            assert_eq!(chrome_line_height(&ctx, &control(&ctx, 12.0, false)), Some(16.0));
            assert_eq!(ctx.global_style().text_styles[&egui::TextStyle::Body].size, 16.0);
            assert_eq!(crate::theme::LayoutTokens::get(&ctx).caption_font_size, 12.0);
        }
        set(&ctx, Some("Chicago Kare"));
        assert_eq!(control(&ctx, 12.0, false).size, 12.0);
        assert_eq!(chrome_line_height(&ctx, &control(&ctx, 12.0, false)), None);
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("theme-font-families"), Families { resolved: Arc::new(vec![("Chicago".into(), "Geneva".into())]) }));
        pack.apply(&ctx);
        assert_eq!(control(&ctx, 12.0, false).size, 12.0);
        assert_eq!(ctx.global_style().text_styles[&egui::TextStyle::Body].size, 12.0);
        crate::theme::apply(&ctx, crate::theme::ThemeKind::Classic);
        assert_eq!(ctx.global_style().text_styles[&egui::TextStyle::Body].size, 12.5);
    }

    #[test]
    fn windows_recreation_calibrates_only_the_compatibility_alias() {
        let ctx = Context::default();
        let pack = crate::theme_pack::ThemePack::parse(include_str!("../../../assets/themes/windows-98-se.json")).unwrap().resolve().unwrap();
        let set_actual = |actual: &str| {
            ctx.data_mut(|d| {
                d.insert_temp(
                    egui::Id::new("theme-font-families"),
                    Families { resolved: Arc::new(vec![("Microsoft Sans Serif".into(), actual.into()), ("W95FA".into(), "W95FA".into())]) },
                );
            });
        };
        set_actual("W95FA");
        for _ in 0..3 {
            pack.apply(&ctx);
            assert!((control(&ctx, 11.0, false).size - 12.5).abs() < 0.001);
            assert!((number(&ctx, 11.0).size - 12.5).abs() < 0.001);
            assert!((ctx.global_style().text_styles[&egui::TextStyle::Body].size - 12.5).abs() < 0.001);
            assert_eq!(crate::theme::LayoutTokens::get(&ctx).menu_bar_height, 18.0);
        }
        set(&ctx, Some("W95FA"));
        assert_eq!(control(&ctx, 11.0, false).size, 11.0);
        for actual in ["Microsoft Sans Serif", "Inter"] {
            set_actual(actual);
            pack.apply(&ctx);
            assert_eq!(control(&ctx, 11.0, false).size, 11.0);
            assert_eq!(ctx.global_style().text_styles[&egui::TextStyle::Body].size, 11.0);
        }
        crate::theme::apply(&ctx, crate::theme::ThemeKind::Classic);
        assert_eq!(ctx.global_style().text_styles[&egui::TextStyle::Body].size, 12.5);
    }

    #[test]
    #[ignore = "requires bundled W95FA; optional PHOTOCRAFT_THEME_FONT_EVIDENCE captures PNGs"]
    fn windows_recreation_alias_renders_compact_chrome_at_three_scales() {
        for scale in [1.0, 1.5, 2.0] {
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1280.0, 900.0)).with_pixels_per_point(scale).wgpu().build_eframe(|cc| {
                crate::PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default())
            });
            h.run_steps(4);
            let ctx = h.ctx.clone();
            let mut fonts = ctx.fonts(|f| f.definitions().clone());
            assert!(fonts.font_data.contains_key("W95FA"));
            // Model WASM's missing-Microsoft-font resolution on this native host.
            fonts.font_data.remove("Microsoft Sans Serif");
            fonts.families.insert(FontFamily::Name("Microsoft Sans Serif".into()), vec!["W95FA".into(), "Inter".into()]);
            ctx.set_fonts(fonts);
            ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new("theme-font-families"), Families { resolved: Arc::new(vec![("Microsoft Sans Serif".into(), "W95FA".into())]) });
            });
            h.run_steps(4);
            for (method, params) in [
                ("engine.execute", serde_json::json!({"command":"file.new","params":{"width":640,"height":480,"mode":"rgb","depth":8,"background":"white"}})),
                ("ui.theme.select", serde_json::json!({"id":"builtin.windows-98-se"})),
            ] {
                let (request, _) = crate::control::ControlRequest::new(method, params);
                let outcome = crate::control::handle(h.state_mut(), &ctx, &request);
                assert!(matches!(outcome, crate::control::Outcome::Done(ref v) if v.get("ok") == Some(&serde_json::Value::Bool(true))));
                h.run_steps(4);
            }
            assert_eq!(status(&ctx), Some(("Microsoft Sans Serif".into(), "W95FA".into())));
            assert!((control(&ctx, 11.0, false).size - 12.5).abs() < 0.001);
            let layout = crate::theme::LayoutTokens::get(&ctx);
            assert_eq!(layout.title_bar_height, 18.0);
            assert_eq!(layout.menu_bar_height, 18.0);
            assert_eq!(layout.options_bar_height, 24.0);
            if let Some(directory) = std::env::var_os("PHOTOCRAFT_THEME_FONT_EVIDENCE") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                h.render().unwrap().save(directory.join(format!("alias-{scale}x.png"))).unwrap();
            }
        }
    }

    #[test]
    fn actual_font_identity_is_required_even_for_recreations() {
        let bytes = photocraft_text::fonts::INTER_REGULAR.to_vec();
        assert!(checked_font(bytes.clone(), "Inter").is_some());
        assert!(checked_font(bytes, "Chicago Kare").is_none());
        assert!(checked_font(vec![0; 64], "Chicago Kare").is_none());
        assert!(checked_font(Vec::new(), "Chicago Kare").is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore = "requires licensed Chicago Kare in PHOTOCRAFT_THEME_FONTS_DIR"]
    fn configured_chicago_recreation_keeps_its_actual_identity() {
        let ctx = Context::default();
        crate::theme::install_fonts(&ctx);
        set(&ctx, Some("Chicago"));
        assert_eq!(status(&ctx), Some(("Chicago".into(), "Chicago Kare".into())));
        assert_eq!(control(&ctx, 12.0, false).size, 16.0);
        set(&ctx, Some("Chicago Kare"));
        assert_eq!(status(&ctx), Some(("Chicago Kare".into(), "Chicago Kare".into())));
        assert_eq!(control(&ctx, 12.0, false).size, 12.0);
    }

    #[test]
    fn explicit_w95fa_theme_family_renders_with_the_available_provider_or_fallback() {
        let mut pack = crate::theme_pack::ThemePack::parse(include_str!("../../../assets/themes/windows-98-se.json")).unwrap();
        pack.typography.family = Some("W95FA".into());
        let pack = pack.resolve().unwrap();
        let mut initialized = false;
        let mut h = egui_kittest::Harness::builder().build_ui(|ui| {
            if !initialized {
                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::Classic);
                initialized = true;
                return;
            }
            ui.label("100° … File Edit");
        });
        pack.apply(&h.ctx);
        h.run_steps(3);
        let (requested, actual) = status(&h.ctx).unwrap();
        assert_eq!(requested, "W95FA");
        assert!(["W95FA", "Microsoft Sans Serif", "Inter"].contains(&actual.as_str()));
        let font = control(&h.ctx, 11.0, false);
        assert_eq!(font.size, 11.0);
        assert!(h.ctx.fonts_mut(|fonts| fonts.has_glyphs(&font, "100° … File Edit")));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore = "requires W95FA in PHOTOCRAFT_THEME_FONTS_DIR"]
    fn configured_windows_recreation_keeps_its_identity_and_symbol_fallback() {
        let mut initialized = false;
        let mut h = egui_kittest::Harness::builder().build_ui(|ui| {
            if !initialized {
                crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::Classic);
                initialized = true;
                return;
            }
            set(ui.ctx(), Some("W95FA"));
            ui.label(egui::RichText::new("100° … File Edit").font(control(ui.ctx(), 11.0, false)));
        });
        h.run_steps(3);
        assert_eq!(status(&h.ctx), Some(("W95FA".into(), "W95FA".into())));
        assert_eq!(control(&h.ctx, 11.0, false).size, 11.0);
        let font = control(&h.ctx, 11.0, false);
        assert!(h.ctx.fonts_mut(|fonts| fonts.has_glyphs(&font, "100° … File Edit")));
        set(&h.ctx, Some("Microsoft Sans Serif"));
        let expected = if load("Microsoft Sans Serif").is_some() { "Microsoft Sans Serif" } else { "W95FA" };
        assert_eq!(status(&h.ctx), Some(("Microsoft Sans Serif".into(), expected.into())));
        let shape = h
            .output()
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) if t.galley.job.text == "100° … File Edit" => Some(t),
                _ => None,
            })
            .unwrap();
        assert!(shape.galley.rows.iter().flat_map(|r| &r.glyphs).all(|g| g.advance_width > 0.0));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn missing_font_file_is_a_fallback_not_an_error() {
        assert!(read_installed_font(std::path::Path::new("/photocraft-nonexistent-font.ttf"), "Chicago Kare").is_none());
    }
}
