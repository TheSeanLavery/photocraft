//! Runtime UI family selection. Installed fonts are read locally, never copied into this repo.
use egui::{Context, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

include!(concat!(env!("OUT_DIR"), "/theme_fonts.rs"));

fn embedded_font(family: &str) -> Option<FontData> {
    let (_, bytes, _) = EMBEDDED_THEME_FONTS.iter().find(|(name, _, _)| *name == family)?;
    checked_font(bytes.to_vec(), family)
}

pub const FAMILIES: [&str; 9] = ["Inter", "JetBrains Mono", "MS Sans Serif", "Microsoft Sans Serif", "Geneva", "Chicago", "Charcoal", "Chicago Kare", "W95FA"];

#[derive(Clone, Debug)]
struct Families {
    resolved: Arc<Vec<(String, String)>>,
}

#[derive(Clone, Default, Debug)]
struct Caption {
    family: Option<String>,
    bold: Option<bool>,
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
    resolved.push(("MS Sans Serif".into(), historical_windows_face(fonts).into()));
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

/// A distinct historical request prefers the licensed raster-style recreation;
/// Microsoft's later scalable compatibility face retains its existing priority.
fn historical_windows_face(fonts: &FontDefinitions) -> &'static str {
    if fonts.font_data.contains_key("W95FA") {
        "W95FA"
    } else if fonts.font_data.contains_key("Microsoft Sans Serif") {
        "Microsoft Sans Serif"
    } else {
        "Inter"
    }
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
        d.remove::<Caption>(egui::Id::new("theme-caption-font"));
        if let Some(name) = family {
            d.insert_temp(id, name.to_string());
        } else {
            d.remove::<String>(id);
        }
    });
}

pub(crate) fn set_caption(ctx: &Context, family: Option<&str>, bold: Option<bool>) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("theme-caption-font"), Caption { family: family.map(str::to_owned), bold }));
}

fn caption_role(ctx: &Context) -> (Option<String>, bool) {
    let role = ctx.data(|d| d.get_temp::<Caption>(egui::Id::new("theme-caption-font"))).unwrap_or_default();
    let family = role.family.or_else(|| ctx.data(|d| d.get_temp::<String>(egui::Id::new("theme-font-family"))));
    let bold = role.bold.unwrap_or(crate::theme::Tokens::get(ctx).appearance == crate::theme::Appearance::Windows98);
    (family, bold)
}

pub(crate) fn caption(ctx: &Context, size: f32) -> FontId {
    let (family, bold) = caption_role(ctx);
    if family.as_deref().is_none_or(|name| caption_actual(ctx, name) == "Inter") {
        return if bold { crate::theme::semibold(size) } else { crate::theme::medium(size) };
    }
    match family.as_deref() {
        Some("Inter") | None => {
            if bold {
                crate::theme::semibold(size)
            } else {
                crate::theme::medium(size)
            }
        }
        Some(name) => calibrated(ctx, FontId::new(size, FontFamily::Name(name.to_owned().into()))),
    }
}

/// Classic raster Windows captions embolden the regular face horizontally. Keep
/// the naturally heavy Chicago faces intact and use Inter's real semibold face.
pub(crate) fn caption_embolden(ctx: &Context) -> bool {
    let (family, bold) = caption_role(ctx);
    bold && family.is_some_and(|name| !matches!(caption_actual(ctx, &name).as_str(), "Inter" | "Chicago" | "Chicago Kare"))
}

fn caption_actual(ctx: &Context, requested: &str) -> String {
    ctx.data(|d| d.get_temp::<Families>(egui::Id::new("theme-font-families")))
        .and_then(|families| families.resolved.iter().find(|(name, _)| name == requested).map(|(_, actual)| actual.clone()))
        .unwrap_or_else(|| "Inter".into())
}

pub(crate) fn caption_status(ctx: &Context) -> Option<(String, String, bool, bool)> {
    if crate::theme::Tokens::get(ctx).appearance == crate::theme::Appearance::Modern {
        return None;
    }
    let (family, bold) = caption_role(ctx);
    let requested = family.unwrap_or_else(|| "Inter".into());
    Some((requested.clone(), caption_actual(ctx, &requested), bold, caption_embolden(ctx)))
}

pub(crate) fn caption_galley(ui: &egui::Ui, text: &str, size: f32, color: egui::Color32, max_width: f32) -> Arc<egui::Galley> {
    let font = caption(ui.ctx(), size);
    let line_height = chrome_line_height(ui.ctx(), &font);
    let embolden = caption_embolden(ui.ctx());
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    for section in &mut job.sections {
        section.format.line_height = line_height;
        section.format.extra_letter_spacing = if embolden { 1.0 } else { 0.0 };
    }
    job.wrap = egui::text::TextWrapping {
        max_width: (max_width - if embolden { 1.0 } else { 0.0 }).max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

pub(crate) fn paint_caption(ui: &egui::Ui, at: egui::Pos2, galley: Arc<egui::Galley>, color: egui::Color32) {
    if caption_embolden(ui.ctx()) {
        ui.painter().galley(at + egui::vec2(1.0, 0.0), galley.clone(), color);
    }
    ui.painter().galley(at, galley, color);
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
    let FontFamily::Name(name) = &font.family else { return false };
    matches!(name.as_ref(), "MS Sans Serif" | "Microsoft Sans Serif")
        && ctx
            .data(|d| d.get_temp::<Families>(egui::Id::new("theme-font-families")))
            .is_some_and(|families| families.resolved.iter().any(|(requested, actual)| requested == name.as_ref() && actual == "W95FA"))
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
    fn historical_windows_alias_prefers_recreation_then_compatible_face_then_inter() {
        let mut fonts = FontDefinitions::default();
        assert_eq!(historical_windows_face(&fonts), "Inter");
        fonts.font_data.insert("Microsoft Sans Serif".into(), Arc::new(FontData::from_static(photocraft_text::fonts::INTER_REGULAR)));
        assert_eq!(historical_windows_face(&fonts), "Microsoft Sans Serif");
        fonts.font_data.insert("W95FA".into(), Arc::new(FontData::from_static(photocraft_text::fonts::INTER_REGULAR)));
        assert_eq!(historical_windows_face(&fonts), "W95FA");
        // Verify the installed registry too: legacy Microsoft requests keep
        // their original preference even when a W95 recreation is present.
        let ctx = Context::default();
        let mut definitions = FontDefinitions::default();
        definitions.font_data.insert("Inter".into(), Arc::new(FontData::from_static(photocraft_text::fonts::INTER_REGULAR)));
        install(&ctx, &mut definitions);
        let actual = historical_windows_face(&definitions);
        let registry = ctx.data(|d| d.get_temp::<Families>(egui::Id::new("theme-font-families"))).unwrap();
        assert!(registry.resolved.contains(&("MS Sans Serif".into(), actual.into())));
        assert_eq!(definitions.families[&FontFamily::Name("MS Sans Serif".into())][0], actual);
        if definitions.font_data.contains_key("Microsoft Sans Serif") {
            assert!(registry.resolved.contains(&("Microsoft Sans Serif".into(), "Microsoft Sans Serif".into())));
        }
    }

    #[test]
    fn caption_roles_round_trip_remain_independent_and_reset() {
        let ctx = Context::default();
        crate::PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Classic);
        let mut pack = crate::theme_pack::ThemePack::parse(include_str!("../../../assets/themes/windows-98-se.json")).unwrap();
        pack.typography.caption_family = Some("Inter".into());
        pack.typography.caption_bold = Some(true);
        let restored = crate::theme_pack::ThemePack::parse(&serde_json::to_string(&pack).unwrap()).unwrap();
        assert_eq!(restored, pack);
        restored.resolve().unwrap().apply(&ctx);
        let body = control(&ctx, 11.0, false);
        assert_eq!(body.family, FontFamily::Name("MS Sans Serif".into()));
        assert_eq!(caption(&ctx, 11.0), crate::theme::semibold(11.0));
        assert!(!caption_embolden(&ctx));
        assert_eq!(caption_status(&ctx), Some(("Inter".into(), "Inter".into(), true, false)));
        pack.typography.caption_bold = Some(false);
        pack.resolve().unwrap().apply(&ctx);
        assert_eq!(caption(&ctx, 11.0), crate::theme::medium(11.0));
        assert_eq!(control(&ctx, 11.0, false), body);
        pack.typography.caption_family = Some("../../arbitrary-font.ttf".into());
        assert!(pack.resolve().is_err());
        crate::theme::apply(&ctx, crate::theme::ThemeKind::Classic);
        assert!(caption_status(&ctx).is_none());
        assert_eq!(caption(&ctx, 12.0), crate::theme::medium(12.0));
    }

    #[test]
    fn caption_weight_tracks_the_resolved_face_including_fallbacks() {
        let ctx = Context::default();
        crate::PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Classic);
        let mut pack = crate::theme_pack::ThemePack::parse(include_str!("../../../assets/themes/windows-98-se.json")).unwrap();
        pack.typography.family = Some("Microsoft Sans Serif".into());
        pack.resolve().unwrap().apply(&ctx);
        // Provider resolution is explicit, so unavailable faces use genuine
        // Inter semibold; recreated Chicago must not acquire a second stroke.
        for (actual, synthesized) in [("W95FA", true), ("Microsoft Sans Serif", true), ("Chicago Kare", false), ("Inter", false)] {
            ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new("theme-font-families"), Families { resolved: Arc::new(vec![("Microsoft Sans Serif".into(), actual.into())]) })
            });
            assert_eq!(caption_embolden(&ctx), synthesized);
            assert_eq!(caption_status(&ctx), Some(("Microsoft Sans Serif".into(), actual.into(), true, synthesized)));
            if actual == "Inter" {
                assert_eq!(caption(&ctx, 11.0), crate::theme::semibold(11.0));
            }
        }
    }

    #[test]
    fn caption_painting_preserves_letter_spacing_and_bounds_at_all_scales() {
        for scale in [1.0, 1.5, 2.0] {
            let mut initialized = false;
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(300.0, 180.0)).with_pixels_per_point(scale).build_ui_state(
                |ui, measured| {
                    if !initialized {
                        crate::PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::Classic);
                        crate::historical::preset("builtin.windows-98-se").unwrap().resolve().unwrap().apply(ui.ctx());
                        initialized = true;
                        return;
                    }
                    let galley = caption_galley(ui, "PhotoCraft caption with a long document name", 11.0, egui::Color32::WHITE, 100.0);
                    *measured = (galley.size().x, galley.rows.len(), galley.job.sections[0].format.extra_letter_spacing);
                    paint_caption(ui, egui::pos2(10.0, 20.0), galley, egui::Color32::WHITE);
                },
                (0.0_f32, 0_usize, 0.0_f32),
            );
            h.run_steps(4);
            for bold in [false, true] {
                set_caption(&h.ctx, None, Some(bold));
                h.run_steps(4);
                let synthetic = caption_embolden(&h.ctx);
                let (width, rows, spacing) = *h.state();
                assert_eq!(rows, 1);
                assert!(width + if synthetic { 1.0 } else { 0.0 } <= 100.01);
                assert_eq!(spacing, if synthetic { 1.0 } else { 0.0 });
                let texts: Vec<_> = h
                    .output()
                    .shapes
                    .iter()
                    .filter_map(|s| match &s.shape {
                        egui::Shape::Text(text) => Some(text),
                        _ => None,
                    })
                    .collect();
                assert_eq!(texts.len(), if synthetic { 2 } else { 1 });
                if synthetic {
                    assert_eq!((texts[0].pos - texts[1].pos).x.abs(), 1.0);
                    assert_eq!(texts[0].pos.y, texts[1].pos.y);
                }
            }
        }
    }

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
        let base = crate::theme_pack::ThemePack::parse(include_str!("../../../assets/themes/windows-98-se.json")).unwrap();
        let set_actual = |actual: &str| {
            ctx.data_mut(|d| {
                d.insert_temp(
                    egui::Id::new("theme-font-families"),
                    Families {
                        resolved: Arc::new(vec![
                            ("MS Sans Serif".into(), actual.into()),
                            ("Microsoft Sans Serif".into(), actual.into()),
                            ("W95FA".into(), "W95FA".into()),
                        ]),
                    },
                );
            });
        };
        for requested in ["MS Sans Serif", "Microsoft Sans Serif"] {
            let mut pack = base.clone();
            pack.typography.family = Some(requested.into());
            let pack = pack.resolve().unwrap();
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
                d.insert_temp(
                    egui::Id::new("theme-font-families"),
                    Families { resolved: Arc::new(vec![("MS Sans Serif".into(), "W95FA".into()), ("Microsoft Sans Serif".into(), "W95FA".into())]) },
                );
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
            assert_eq!(status(&ctx), Some(("MS Sans Serif".into(), "W95FA".into())));
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
