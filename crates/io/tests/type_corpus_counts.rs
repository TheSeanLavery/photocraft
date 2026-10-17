//! Opt-in classification check requested on #4771; no rendering, export or corpus download.

use std::path::PathBuf;

use photocraft_doc::LayerContent;
use photocraft_psd::{Compression, LayerRecord, PsdFile, Version, testgen};

#[test]
#[ignore = "requires PHOTOCRAFT_TYPE_CORPUS pointing to an existing local PSD corpus"]
fn safe_corpus_type_blocks_stay_editable() {
    let root = PathBuf::from(std::env::var_os("PHOTOCRAFT_TYPE_CORPUS").expect("set PHOTOCRAFT_TYPE_CORPUS"));
    let mut dirs = vec![root];
    let (mut files, mut rejected_files, mut types, mut fallback_types) = (0usize, 0usize, 0usize, 0usize);
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                dirs.push(path);
                continue;
            }
            if !kind.is_file() || !path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("psd") || e.eq_ignore_ascii_case("psb")) {
                continue;
            }
            let Ok(file) = PsdFile::from_bytes(&std::fs::read(&path).unwrap()) else {
                rejected_files += 1;
                continue;
            };
            files += 1;
            for block in file.layers().iter().filter_map(|layer| layer.block(b"TySh")) {
                if block.is_foreign_order() || block.data.len() < 52 {
                    continue;
                }
                let Some(bytes) = block.data.get(2..50) else { continue };
                let mut matrix = [0.0; 6];
                for (value, bytes) in matrix.iter_mut().zip(bytes.as_chunks::<8>().0) {
                    *value = f64::from_be_bytes(*bytes);
                }
                if !photocraft_text::psd::usable_tysh_transform(&matrix) {
                    continue;
                }
                // Isolate the real type block: this checks classification, not image pixels.
                let mut probe = testgen::small(Version::Psd, Compression::Raw);
                probe.layers_mut().clear();
                probe.layers_mut().push(LayerRecord { blocks: vec![block.clone()], ..Default::default() });
                let (document, _) = photocraft_io::psd_to_document(&probe);
                assert!(matches!(document.layers[0].content, LayerContent::Text(_)), "{} lost editable type", path.display());
                types += 1;
                fallback_types += usize::from(photocraft_text::psd::text_layer_from_tysh(&block.data, 72.0).is_none());
            }
        }
    }
    assert!(types > 0, "no usable type blocks were checked");
    eprintln!(
        "{files} parsed PSD/PSB files; {rejected_files} rejected files; {types} safe type blocks remain editable, including {fallback_types} legacy fallbacks"
    );
}
