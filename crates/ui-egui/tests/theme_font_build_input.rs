#[path = "../build_support/theme_fonts.rs"]
mod input;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "photocraft-font-build-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(root.join("fonts/chicago-kare")).unwrap();
        std::fs::create_dir_all(root.join("fonts/w95fa")).unwrap();
        let mut manifest = String::new();
        for (name, path, license, text) in [
            ("Chicago Kare", "fonts/chicago-kare/ChicagoKare-Regular.ttf", "MIT", "Permission is hereby granted"),
            ("W95FA", "fonts/w95fa/W95FA-Regular.otf", "OFL-1.1", "SIL OPEN FONT LICENSE Version 1.1"),
        ] {
            let data = name.as_bytes();
            std::fs::write(root.join(path), data).unwrap();
            let license_path = format!("{}/LICENSE", PathBuf::from(path).parent().unwrap().display());
            std::fs::write(root.join(&license_path), text).unwrap();
            let hash: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
            manifest.push_str(&format!("{name} | Regular | {path} | Latn | {license} | {license_path} | {hash} | https://example.invalid/fonts\n"));
        }
        std::fs::write(root.join("fonts/manifest.txt"), manifest).unwrap();
        Self(root.canonicalize().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn subset_checks_hashes_licenses_and_only_selected_files() {
    let f = Fixture::new();
    let entries = input::entries(&f.0).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry.file.starts_with(&f.0) && entry.license.starts_with(&f.0)));
    std::fs::write(f.0.join("fonts/w95fa/W95FA-Regular.otf"), b"corrupt").unwrap();
    assert!(input::entries(&f.0).unwrap_err().contains("checksum"));
}

#[test]
fn missing_license_and_missing_family_fail_the_build_input() {
    let f = Fixture::new();
    std::fs::write(f.0.join("fonts/w95fa/LICENSE"), b"not a license").unwrap();
    assert!(input::entries(&f.0).unwrap_err().contains("license text"));
    std::fs::write(f.0.join("fonts/manifest.txt"), "# no historical faces\n").unwrap();
    assert!(input::entries(&f.0).unwrap_err().contains("missing Chicago Kare"));
}

#[test]
fn oversized_inputs_are_rejected_before_embedding() {
    let f = Fixture::new();
    std::fs::write(f.0.join("fonts/manifest.txt"), vec![b' '; 65_537]).unwrap();
    assert!(input::entries(&f.0).unwrap_err().contains("regular file"));
}

#[test]
fn a_matching_hash_does_not_make_invalid_font_data_acceptable() {
    let f = Fixture::new();
    let entries = input::entries(&f.0).unwrap();
    assert!(input::validate_faces(&entries).unwrap_err().contains("font data"));
}

#[test]
fn license_paths_cannot_escape_the_checkout() {
    let f = Fixture::new();
    let outside = Fixture::new();
    let manifest = f.0.join("fonts/manifest.txt");
    let text = std::fs::read_to_string(&manifest).unwrap().replace("fonts/w95fa/LICENSE", &outside.0.join("fonts/w95fa/LICENSE").display().to_string());
    std::fs::write(manifest, text).unwrap();
    assert!(input::entries(&f.0).unwrap_err().contains("outside font checkout"));
}
