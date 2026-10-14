//! Import oracle for the hot PSD path: all samples, crossed tile boundaries, cropped masks,
//! negative layer origins, transparency, hierarchy and adjustment survive map/public import.
mod common;
use common::*;
use photocraft_color::{BlendMode, ColorMode, SampleType};
use photocraft_doc::{Adjustment, Document, Layer, LayerContent, LayerMask};
use photocraft_geom::{Rect, Size};
use photocraft_io::{ExportOptions, export, import, psd_to_document};
use photocraft_psd::PsdFile;

#[test]
fn textured_psd_import_matches_source_and_roundtrip_at_every_depth() {
    for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
        let mut original = Document::new("textured oracle", Size::new(513, 259), ColorMode::Rgb, depth);
        let fmt = original.pixel_format();
        original.layers.push(raster("full nonuniform background", fmt, original.bounds(), 19, false));
        let r = Rect::new(-7, 17, 283, 267);
        let mut masked = raster("negative cropped alpha", fmt, r, 71, true);
        masked.blend = BlendMode::Multiply;
        masked.opacity = 128.0 / 255.0;
        masked.mask =
            Some(LayerMask { surface: mask_surface(depth, Rect::new(9, 29, 267, 253), 0.0, 29), enabled: true, linked: true, density: 1.0, feather: 0.0 });
        let mut grouped = raster("cross tile boundary", fmt, Rect::new(249, 241, 520, 270), 151, true);
        grouped.blend = BlendMode::Screen;
        let mut group = Layer::group("detail group", vec![masked, grouped]);
        group.blend = BlendMode::Normal;
        original.layers.push(group);
        original.layers.push(Layer::new("exposure", LayerContent::Adjustment(Adjustment::Exposure { exposure: 0.25, offset: 0.0, gamma: 1.0 })));
        let bytes = export(&original, "oracle.psd", &ExportOptions::default()).expect("generate fixture").bytes;
        let parsed = PsdFile::from_bytes(&bytes).expect("parse fixture");
        let (mapped, _) = psd_to_document(&parsed);
        let imported = import("oracle.psd", &bytes).expect("public import").document;
        assert_docs_eq(&mapped, &imported);
        // Compare every raster/mask sample, including pixels outside the canvas, against the
        // independently generated source rather than treating two import paths as the oracle.
        assert_docs_eq(&original, &imported);
        let expected = photocraft_compose::flatten(&original);
        let actual = photocraft_compose::flatten(&imported);
        assert_eq!(expected.rect, actual.rect);
        assert!(max_diff(&expected.px, &actual.px) <= 1.0 / 255.0 + 1e-5, "{depth:?} composite differs");
        let again = export(&imported, "oracle.psd", &ExportOptions::default()).expect("export imported fixture").bytes;
        let second = import("oracle.psd", &again).expect("second roundtrip").document;
        assert_docs_eq(&imported, &second);
    }
}

/// Native import parallelizes planes at 65,536 pixels. The old damaged-channel test is 4x4;
/// this case crosses that threshold and checks that completion order cannot reorder warnings.
#[test]
fn concurrent_large_channel_failures_keep_warning_order_and_empty_layer() {
    use photocraft_psd::{Compression, LayerSpec, PixelData, PsdBuilder, Version};
    const SIDE: u32 = 257;
    for version in [Version::Psd, Version::Psb] {
        let mut builder = PsdBuilder::new(SIDE, SIDE).version(version).compression(Compression::Rle);
        builder.push_layer(LayerSpec::new("damaged large layer", 0, 0, SIDE, SIDE, PixelData::Rgba8(vec![200; (SIDE * SIDE * 4) as usize])));
        let mut file = builder.build().expect("generate layer channels");
        for channel in &mut file.layers_mut()[0].channels {
            if [0, 2, -1].contains(&channel.id) {
                channel.data.truncate(1);
            }
        }
        let mut first_warnings = None;
        for _ in 0..4 {
            let (document, warnings) = psd_to_document(&file);
            let decode_warnings: Vec<_> = warnings.iter().filter(|w| w.contains("could not be decoded")).collect();
            assert_eq!(decode_warnings.len(), 3, "{version:?}: {warnings:?}");
            for (warning, channel_id) in decode_warnings.iter().zip([0, 2, -1]) {
                assert!(warning.contains(&format!("channel {channel_id} ")), "{version:?}: warning order {warnings:?}");
                assert!(warning.contains("treated as empty"));
            }
            if let Some(first) = &first_warnings {
                assert_eq!(first, &warnings, "parallel warning order must be deterministic");
            } else {
                first_warnings = Some(warnings);
            }
            let surface = document.layers[0].surface().expect("raster layer remains present");
            assert_eq!(surface.tile_count(), 0, "failed decode must not allocate opaque pixels");
            assert_eq!(surface.pixel(128, 128)[3], 0.0);
        }
    }
}

/// An absent channel has defined fill semantics, unlike a corrupt present channel. Exercise
/// both alpha=opaque and red=zero through the concurrent decoder and direct RGBA8 tiles.
#[test]
fn concurrent_large_missing_channels_fill_without_decode_warnings() {
    use photocraft_psd::{Compression, LayerSpec, PixelData, PsdBuilder, Version};
    const SIDE: u32 = 257;
    let pixels: Vec<u8> = (0..SIDE * SIDE).flat_map(|i| [(i % 251) as u8, ((i * 17) % 253) as u8, ((i * 31) % 255) as u8, 93]).collect();
    for version in [Version::Psd, Version::Psb] {
        for missing_red in [false, true] {
            let mut builder = PsdBuilder::new(SIDE, SIDE).version(version).compression(Compression::Rle);
            builder.push_layer(LayerSpec::new("missing channels", 0, 0, SIDE, SIDE, PixelData::Rgba8(pixels.clone())));
            let mut file = builder.build().expect("generate layer channels");
            file.layers_mut()[0].channels.retain(|channel| channel.id != -1 && !(missing_red && channel.id == 0));
            let (document, warnings) = psd_to_document(&file);
            assert!(!warnings.iter().any(|w| w.contains("could not be decoded") || w.contains("treated as empty")), "{version:?}: {warnings:?}");
            let surface = document.layers[0].surface().expect("raster layer");
            for (i, source) in pixels.chunks_exact(4).enumerate() {
                let actual = surface.pixel((i % SIDE as usize) as i32, (i / SIDE as usize) as i32);
                let expected = [if missing_red { 0 } else { source[0] }, source[1], source[2], 255];
                for (channel, (&value, &want)) in actual.iter().zip(&expected).enumerate() {
                    assert_eq!((value * 255.0).round() as u8, want, "{version:?}, missing_red={missing_red}, pixel {i}, channel {channel}");
                }
            }
        }
    }
}
