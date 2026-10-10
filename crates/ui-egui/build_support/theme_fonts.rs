use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Entry {
    pub family: String,
    pub file: PathBuf,
    pub license: PathBuf,
}

/// Deliberately small subset: these two historical UI faces, not the document/CJK library.
pub fn entries(root: &Path) -> Result<Vec<Entry>, String> {
    let root = root.canonicalize().map_err(|e| format!("font checkout: {e}"))?;
    let manifest = root.join("fonts/manifest.txt");
    let text = read(&manifest, 65_536)?;
    let text = String::from_utf8(text).map_err(|e| format!("font manifest is not UTF-8: {e}"))?;
    let mut result = Vec::new();
    for (family, expected, license_kind) in
        [("Chicago Kare", "fonts/chicago-kare/ChicagoKare-Regular.ttf", "MIT"), ("W95FA", "fonts/w95fa/W95FA-Regular.otf", "OFL-1.1")]
    {
        let fields = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .find_map(|line| {
                let fields: Vec<&str> = line.split(" | ").map(str::trim).collect();
                matches!(fields.as_slice(), [name, "Regular", ..] if *name == family).then_some(fields)
            })
            .ok_or_else(|| format!("missing {family} Regular in font manifest"))?;
        let [_, _, file, _, license, license_file, hash, _] = fields.as_slice() else {
            return Err(format!("malformed {family} manifest entry"));
        };
        if *file != expected || *license != license_kind {
            return Err(format!("unexpected file or license for {family}"));
        }
        let file = within(&root, file)?;
        let license = within(&root, license_file)?;
        let bytes = read(&file, 1024 * 1024)?;
        let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if actual != *hash {
            return Err(format!("{family} checksum does not match the font manifest"));
        }
        let license_text = String::from_utf8(read(&license, 65_536)?).map_err(|e| format!("{family} license is not UTF-8: {e}"))?;
        let marker = if license_kind == "MIT" { "Permission is hereby granted" } else { "SIL OPEN FONT LICENSE Version 1.1" };
        if !license_text.contains(marker) {
            return Err(format!("{family} license text is missing"));
        }
        result.push(Entry { family: family.to_owned(), file, license });
    }
    Ok(result)
}

fn within(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = root.join(relative).canonicalize().map_err(|e| format!("{relative}: {e}"))?;
    if !path.starts_with(root) {
        return Err(format!("{relative}: outside font checkout"));
    }
    Ok(path)
}

fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > limit {
        return Err(format!("{}: expected a nonempty regular file <= {limit} bytes", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("font input grew past its size limit".into());
    }
    Ok(bytes)
}

pub fn validate_faces(entries: &[Entry]) -> Result<(), String> {
    use skrifa::{FontRef, MetadataProvider, string::StringId};
    for entry in entries {
        let bytes = read(&entry.file, 1024 * 1024)?;
        let font = FontRef::new(&bytes).map_err(|e| format!("{} font data: {e}", entry.family))?;
        let matches = [StringId::FAMILY_NAME, StringId::TYPOGRAPHIC_FAMILY_NAME]
            .into_iter()
            .any(|id| font.localized_strings(id).any(|name| name.to_string() == entry.family));
        if !matches {
            return Err(format!("{} font has a different actual family", entry.family));
        }
    }
    Ok(())
}
