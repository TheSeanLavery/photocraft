#[path = "build_support/theme_fonts.rs"]
mod theme_fonts;

fn main() {
    use std::fmt::Write as _;
    use std::path::PathBuf;
    println!("cargo::rerun-if-env-changed=PHOTOCRAFT_THEME_FONTS_BUILD_DIR");
    println!("cargo::rerun-if-env-changed=PHOTOCRAFT_THEME_FONTS_REQUIRED");
    let root = std::env::var_os("PHOTOCRAFT_THEME_FONTS_BUILD_DIR").filter(|v| !v.is_empty()).map(PathBuf::from);
    let required = std::env::var("PHOTOCRAFT_THEME_FONTS_REQUIRED").is_ok_and(|v| v == "1");
    let result = match root {
        Some(root) => {
            println!("cargo::rerun-if-changed={}", root.join("fonts/manifest.txt").display());
            theme_fonts::entries(&root).and_then(|entries| {
                theme_fonts::validate_faces(&entries)?;
                Ok(entries)
            })
        }
        None if required => Err("PHOTOCRAFT_THEME_FONTS_BUILD_DIR is required".into()),
        None => Ok(Vec::new()),
    };
    let mut source = String::from("static EMBEDDED_THEME_FONTS: &[(&str, &[u8], &str)] = &[\n");
    match result {
        Ok(entries) => {
            for entry in entries {
                println!("cargo::rerun-if-changed={}", entry.file.display());
                println!("cargo::rerun-if-changed={}", entry.license.display());
                let _ = writeln!(source, "({:?}, include_bytes!({:?}), include_str!({:?})),", entry.family, entry.file, entry.license);
            }
        }
        Err(e) if required => println!("cargo::error=historical theme fonts: {e}"),
        Err(e) => println!("cargo::warning=building without historical theme fonts: {e}"),
    }
    source.push_str("];\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("theme_fonts.rs");
    if let Err(e) = std::fs::write(out, source) {
        println!("cargo::error=writing historical font table: {e}");
    }
}
