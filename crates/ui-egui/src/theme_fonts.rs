//! Runtime UI family selection. Installed fonts are read locally, never copied into this repo.
use egui::{Context, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

pub const FAMILIES: [&str; 7] = ["Inter", "JetBrains Mono", "Microsoft Sans Serif", "Geneva", "Chicago", "Charcoal", "Chicago Kare"];

#[derive(Clone, Debug)]
struct Families {
    resolved: Vec<(String, String)>,
}

pub(crate) fn install(ctx: &Context, fonts: &mut FontDefinitions) {
    let mut resolved: Vec<(String, String)> = vec![("Inter".into(), "Inter".into()), ("JetBrains Mono".into(), "JetBrainsMono".into())];
    for family in ["Microsoft Sans Serif", "Geneva", "Chicago Kare"] {
        let loaded = load(family);
        if let Some(data) = loaded {
            fonts.font_data.insert(family.into(), Arc::new(data));
        }
        let actual = if fonts.font_data.contains_key(family) { family } else { "Inter" };
        resolved.push((family.into(), actual.into()));
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
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("theme-font-families"), Families { resolved }));
}

#[cfg(not(target_arch = "wasm32"))]
fn load(family: &str) -> Option<FontData> {
    use std::sync::OnceLock;
    static WINDOWS: OnceLock<Option<FontData>> = OnceLock::new();
    static GENEVA: OnceLock<Option<FontData>> = OnceLock::new();
    static CHICAGO: OnceLock<Option<FontData>> = OnceLock::new();
    static CHARCOAL: OnceLock<Option<FontData>> = OnceLock::new();
    static CHICAGO_KARE: OnceLock<Option<FontData>> = OnceLock::new();
    let cache = match family {
        "Microsoft Sans Serif" => &WINDOWS,
        "Geneva" => &GENEVA,
        "Chicago" => &CHICAGO,
        "Charcoal" => &CHARCOAL,
        "Chicago Kare" => &CHICAGO_KARE,
        _ => return None,
    };
    cache.get_or_init(|| read_font(family)).clone()
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

#[cfg(any(not(target_arch = "wasm32"), test))]
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
fn load(_: &str) -> Option<FontData> {
    None
}

pub fn status(ctx: &Context) -> Option<(String, String)> {
    let requested: String = ctx.data(|d| d.get_temp(egui::Id::new("theme-font-family")))?;
    let actual = ctx.data(|d| d.get_temp::<Families>(egui::Id::new("theme-font-families")))?.resolved.into_iter().find(|(name, _)| name == &requested)?.1;
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

pub(crate) fn control(ctx: &Context, size: f32, bold: bool) -> FontId {
    if let Some(name) = ctx.data(|d| d.get_temp::<String>(egui::Id::new("theme-font-family"))) {
        FontId::new(size, FontFamily::Name(name.into()))
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
        set(&ctx, Some("Chicago Kare"));
        assert_eq!(status(&ctx), Some(("Chicago Kare".into(), "Chicago Kare".into())));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn missing_font_file_is_a_fallback_not_an_error() {
        assert!(read_installed_font(std::path::Path::new("/photocraft-nonexistent-font.ttf"), "Chicago Kare").is_none());
    }
}
