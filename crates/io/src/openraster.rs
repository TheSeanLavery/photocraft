//! OpenRaster 0.0.6: bounded ZIP/XML reads and editable baseline layer stacks.
//! Source specification: https://www.openraster.org/baseline/
use crate::{ExportOptions, ExportResult, IoError};
use photocraft_color::BlendMode as B;
use photocraft_doc::{Document, Layer, LayerContent};
use std::io::{Cursor, Write};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
const BUDGET: u64 = 1 << 30;
const MAX_ENTRIES: usize = 4096;
fn bad(e: impl std::fmt::Display) -> IoError {
    IoError::Unsupported(format!("OpenRaster: {e}"))
}
pub(super) fn xml(s: &str) -> Result<String, IoError> {
    if s.len() > 1 << 20 || s.chars().any(|c| !matches!(c,'\t'|'\n'|'\r'|' '..='\u{d7ff}'|'\u{e000}'..='\u{fffd}'|'\u{10000}'..='\u{10ffff}')) {
        return Err(bad("layer name contains invalid XML characters or exceeds name budget"));
    }
    Ok(s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;"))
}
fn blend(b: B) -> Option<String> {
    // Share the richer upstream SVG/Krita catalog with baseline detection and the ICC writer.
    crate::ora::op_name(b)
}

fn add(zip: &mut ZipWriter<Cursor<Vec<u8>>>, name: &str, bytes: &[u8]) -> Result<(), IoError> {
    let current = zip.get_ref().ok_or_else(|| bad("ZIP writer is closed"))?.get_ref().len() as u64;
    if (bytes.len() as u64).saturating_add(current) > BUDGET {
        return Err(bad("output archive exceeds one GiB budget"));
    }
    // PNGs are compressed already. Stored entries keep memory/CPU costs predictable.
    zip.start_file(name, SimpleFileOptions::default().compression_method(CompressionMethod::Stored).large_file(bytes.len() as u64 > u64::from(u32::MAX)))
        .map_err(bad)?;
    zip.write_all(bytes).map_err(bad)
}
pub(super) fn baseline(layers: &[Layer], depth: usize) -> bool {
    if depth > photocraft_doc::MAX_GROUP_DEPTH {
        return false;
    }
    layers.iter().all(|l| {
        blend(l.blend).is_some()
            && !l.clipped
            && l.excluded_channels == 0
            && l.blend_if == Default::default()
            && l.advanced.is_default()
            && l.effects.items.is_empty()
            && l.effects.psd_raw.is_none()
            && l.vector_mask.is_none()
            && l.mask.as_ref().is_none_or(|mask| mask.feather <= 0.0)
            && match &l.content {
                LayerContent::Adjustment(_) => false,
                LayerContent::Group(g) => {
                    g.artboard.is_none() && l.mask.is_none() && l.vector_mask.is_none() && l.effects.items.is_empty() && baseline(&g.children, depth + 1)
                }
                _ => true,
            }
    })
}
pub(crate) fn export(doc: &Document, opts: &ExportOptions) -> Result<ExportResult, IoError> {
    check_size(doc.size.width, doc.size.height)?;
    let mut pending = vec![(&doc.layers, 0usize)];
    let mut count = 0usize;
    let mut unsupported_blend = false;
    while let Some((layers, depth)) = pending.pop() {
        if depth > photocraft_doc::MAX_GROUP_DEPTH {
            return Err(bad("group depth exceeds limit"));
        }
        count = count.saturating_add(layers.len());
        if count > MAX_ENTRIES - 4 {
            return Err(bad("too many layers"));
        }
        for layer in layers {
            unsupported_blend |= blend(layer.blend).is_none();
            if let LayerContent::Group(group) = &layer.content {
                pending.push((&group.children, depth + 1));
            }
        }
    }
    if !doc.resolution_dpi.is_finite() || doc.resolution_dpi < 1.0 || doc.resolution_dpi > u32::MAX as f32 {
        return Err(bad("invalid document resolution"));
    }
    let mut warnings = Vec::new();
    if doc.resolution_dpi.fract() != 0.0 {
        warnings.push("OpenRaster resolution rounded to an integer ppi".into());
    }
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    add(&mut zip, "mimetype", b"image/openraster")?;
    let mut stack = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<image version=\"0.0.6\" w=\"{}\" h=\"{}\" xres=\"{}\" yres=\"{}\"><stack>",
        doc.size.width,
        doc.size.height,
        doc.resolution_dpi.round(),
        doc.resolution_dpi.round()
    );
    let merged = crate::flat::export_flat(doc, photocraft_codecs::Format::Png, opts)?;
    if baseline(&doc.layers, 0) {
        let mut state = LayerExport { id: 0, warnings: &mut warnings };
        export_layers(doc, &doc.layers, &mut zip, &mut stack, opts, &mut state, 0)?;
    } else {
        if unsupported_blend {
            warnings.push("A blend mode has no OpenRaster equivalent; its appearance is kept in the flattened composite.".into());
        }
        warnings.push("OpenRaster cannot represent these clipping, adjustment, artboard or advanced blend settings; output is a flattened composite. Use PhotoCraft or PSD to retain editability.".into());
        warnings.extend(merged.warnings.clone());
        add(&mut zip, "data/layer0.png", &merged.bytes)?;
        stack.push_str("<layer name=\"Composite\" src=\"data/layer0.png\" composite-op=\"svg:src-over\"/>");
    }
    stack.push_str("</stack></image>");
    add(&mut zip, "stack.xml", stack.as_bytes())?;
    add(&mut zip, "mergedimage.png", &merged.bytes)?;
    let thumb = photocraft_compose::thumbnail(doc, 256);
    let thumbnail = photocraft_codecs::Image::from_u8(thumb.width, thumb.height, photocraft_codecs::ChannelLayout::Rgba, thumb.pixels)?;
    add(&mut zip, "Thumbnails/thumbnail.png", &photocraft_codecs::encode(&thumbnail, photocraft_codecs::Format::Png, &opts.encode)?)?;
    warnings.push("OpenRaster keeps baseline raster layers/groups; editable masks, text, shapes, smart objects and effects are baked into PNG pixels, and content outside the canvas is cropped. Use PhotoCraft or PSD for full fidelity.".into());
    warnings.sort();
    warnings.dedup();
    Ok(ExportResult { bytes: zip.finish().map_err(bad)?.into_inner(), warnings })
}
struct LayerExport<'a> {
    id: usize,
    warnings: &'a mut Vec<String>,
}
fn export_layers(
    doc: &Document,
    layers: &[Layer],
    zip: &mut ZipWriter<Cursor<Vec<u8>>>,
    stack: &mut String,
    opts: &ExportOptions,
    state: &mut LayerExport<'_>,
    depth: usize,
) -> Result<(), IoError> {
    if depth > photocraft_doc::MAX_GROUP_DEPTH {
        return Err(bad("group depth exceeds document limit"));
    }
    for layer in layers.iter().rev() {
        // ORA is top-first; the document model is bottom-first.
        if !layer.opacity.is_finite() || !(0.0..=1.0).contains(&layer.opacity) {
            return Err(bad("invalid layer opacity"));
        }
        let attrs = format!(
            "name=\"{}\" opacity=\"{}\" visibility=\"{}\" composite-op=\"{}\"",
            xml(&layer.name)?,
            layer.opacity,
            if layer.visible { "visible" } else { "hidden" },
            blend(layer.blend).ok_or_else(|| bad("unsupported blend mode"))?
        );
        if let LayerContent::Group(group) = &layer.content {
            stack.push_str(&format!("<stack {attrs} isolation=\"{}\">", if layer.blend == B::PassThrough { "auto" } else { "isolate" }));
            export_layers(doc, &group.children, zip, stack, opts, state, depth + 1)?;
            stack.push_str("</stack>");
        } else {
            let mut isolated = doc.clone();
            let mut copy = layer.clone();
            copy.opacity = 1.0;
            copy.blend = B::Normal;
            copy.visible = true;
            isolated.layers = vec![copy];
            let png = crate::flat::export_flat(&isolated, photocraft_codecs::Format::Png, opts)?;
            state.warnings.extend(png.warnings.into_iter().filter(|w| !w.contains("layer(s) flattened")));
            let path = format!("data/layer{}.png", state.id);
            state.id = state.id.saturating_add(1);
            add(zip, &path, &png.bytes)?;
            stack.push_str(&format!("<layer {attrs} src=\"{path}\" x=\"0\" y=\"0\"/>"));
        }
        if stack.len() > 4 << 20 {
            return Err(bad("layer metadata exceeds XML budget"));
        }
    }
    Ok(())
}
fn check_size(w: u32, h: u32) -> Result<(), IoError> {
    photocraft_codecs::Limits { max_alloc: 256 << 20, max_width: 32768, max_height: 32768, ..Default::default() }.check(
        w,
        h,
        photocraft_codecs::ChannelLayout::Rgba,
        photocraft_codecs::SampleType::U16,
    )?;
    Ok(())
}
