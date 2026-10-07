//! HDR display settings, diagnostics and an explicit SDR export (original pixels unchanged).
use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};
use photocraft_cms::{Builtin, Intent, Transform};
use photocraft_color::hdr::HdrWorkbench;
use photocraft_compose::Buffer;
use photocraft_doc::Document;
use rayon::prelude::*;
use serde_json::{Value, json};

pub fn srgb_profile_bytes() -> std::sync::Arc<Vec<u8>> {
    Builtin::Srgb.profile().to_bytes()
}
/// Final CPU view encoding after the common SDR transform, for an untagged custom monitor.
pub fn to_monitor(b: &mut Buffer, display: Option<&crate::display_color::CanvasDisplay>) -> Result<()> {
    let Some(display) = display else { return Ok(()) };
    if display.monitor.to_bytes() == srgb_profile_bytes() {
        return Ok(());
    }
    let t = Transform::new(Builtin::Srgb.profile(), &display.monitor, Intent::RelativeColorimetric, true).map_err(|e| EngineError::Other(e.to_string()))?;
    b.px.par_chunks_mut(4096).for_each(|chunk| {
        for p in chunk {
            let mut rgb = [0.0; 3];
            t.eval(&p[..3], &mut rgb);
            p[0] = rgb[0];
            p[1] = rgb[1];
            p[2] = rgb[2];
        }
    });
    Ok(())
}
fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}
fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or("no document open".into())
}

pub fn settings(s: &Session, doc: &Document) -> HdrWorkbench {
    s.color.workbench.get(&doc.id).copied().unwrap_or_default().sanitized()
}
fn configure(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "view.hdrWorkbench";
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    let mut v = serde_json::to_value(settings(s, doc)).map_err(|e| bad(CMD, e.to_string()))?;
    let obj = p.as_object().ok_or_else(|| bad(CMD, "expected object"))?;
    for (key, value) in obj {
        match key.as_str() {
            "method" => {
                if !matches!(value.as_str(), Some("natural" | "filmic" | "clip")) {
                    return Err(bad(CMD, "method: natural|filmic|clip"));
                }
            }
            "split" | "clipping" => {
                if !value.is_boolean() {
                    return Err(bad(CMD, format!("{key}: boolean required")));
                }
            }
            "exposure" | "shoulder" | "contrast" | "saturation" => {
                let x = value.as_f64().ok_or_else(|| bad(CMD, format!("{key}: number required")))?;
                let (lo, hi) = match key.as_str() {
                    "exposure" => (-20.0, 20.0),
                    "shoulder" => (0.1, 0.95),
                    "contrast" => (0.25, 2.0),
                    _ => (0.0, 2.0),
                };
                if !x.is_finite() || !(lo..=hi).contains(&x) {
                    return Err(bad(CMD, format!("{key}: must be {lo}..{hi}")));
                }
            }
            _ => return Err(bad(CMD, format!("unknown field {key}"))),
        }
        v[key] = value.clone();
    }
    let setting: HdrWorkbench = serde_json::from_value(v.clone()).map_err(|e| bad(CMD, e.to_string()))?;
    let id = doc.id;
    s.color.workbench.insert(id, setting);
    Ok(v)
}

/// Composite numbers → linear sRGB. HDR is normalized before an ICC transform and its
/// radiance restored afterwards, matching the canvas's extended LUT convention.
pub fn linear_srgb(doc: &Document, buf: &Buffer) -> Result<Buffer> {
    let source = crate::color_cmds::composite_profile(doc);
    let linear_identity = source.to_bytes() == Builtin::LinearSrgb.profile().to_bytes();
    let srgb_identity = source.to_bytes() == Builtin::Srgb.profile().to_bytes();
    if linear_identity || srgb_identity {
        let mut out = buf.clone();
        out.px.par_chunks_mut(4096).for_each(|chunk| {
            for p in chunk {
                for v in &mut p[..3] {
                    let value = if v.is_finite() { v.clamp(0.0, 65504.0) } else { 0.0 };
                    *v = if linear_identity { value } else { photocraft_color::convert::srgb_to_linear(value).min(65504.0) };
                }
                p[3] = if p[3].is_finite() { p[3].clamp(0.0, 1.0) } else { 0.0 };
            }
        });
        return Ok(out);
    }

    let color = crate::color_cmds::ColorState::default();
    let display = color.canvas_display(doc)?;
    let encoded = display.texture_buffer(buf);
    let t = Transform::new(&display.source, Builtin::Srgb.profile(), Intent::RelativeColorimetric, true).map_err(|e| EngineError::Other(e.to_string()))?;
    let mut out = buf.clone();
    out.px.par_chunks_mut(4096).zip(encoded.px.par_chunks(4096)).for_each(|(dst_chunk, src_chunk)| {
        for (dst, src) in dst_chunk.iter_mut().zip(src_chunk) {
            let lin = [src[0], src[1], src[2]].map(|v| photocraft_color::convert::srgb_to_linear(if v.is_finite() { v.max(0.0) } else { 0.0 }));
            let scale = lin[0].max(lin[1]).max(lin[2]).clamp(1.0, 65504.0);
            let input = lin.map(|v| photocraft_color::convert::linear_to_srgb(v / scale));
            let mut rgb = [0.0; 3];
            t.eval(&input, &mut rgb);
            *dst = [
                photocraft_color::convert::srgb_to_linear(rgb[0].clamp(0.0, 1.0)) * scale,
                photocraft_color::convert::srgb_to_linear(rgb[1].clamp(0.0, 1.0)) * scale,
                photocraft_color::convert::srgb_to_linear(rgb[2].clamp(0.0, 1.0)) * scale,
                if src[3].is_finite() { src[3].clamp(0.0, 1.0) } else { 0.0 },
            ];
        }
    });
    Ok(out)
}
/// Explicit SDR preview pixels, also used by export and CPU fallback. Alpha stays straight.
pub fn sdr_buffer(doc: &Document, buf: &Buffer, setting: HdrWorkbench) -> Result<Buffer> {
    let mut b = linear_srgb(doc, buf)?;
    map_sdr(&mut b, setting, false);
    Ok(b)
}
fn map_sdr(b: &mut Buffer, setting: HdrWorkbench, overlay: bool) {
    let width = b.rect.width().max(1) as usize;
    b.px.par_chunks_mut(4096).enumerate().for_each(|(chunk_index, chunk)| {
        for (j, p) in chunk.iter_mut().enumerate() {
            let source = [p[0], p[1], p[2]];
            let i = chunk_index * 4096 + j;
            let rgb = if overlay && setting.clipped(source, 1.0) && ((i % width + i / width) % 12) < 6 { [1.0, 0.0, 0.5] } else { setting.apply(source, 1.0) }
                .map(photocraft_color::convert::linear_to_srgb);
            p[0] = rgb[0];
            p[1] = rgb[1];
            p[2] = rgb[2];
        }
    });
}
/// CPU view fallback: legacy view exposure/gamma followed by the workbench. Overlays are
/// explicitly preview-only and never included in `sdr_png`.
pub fn preview_buffer(doc: &Document, buf: &Buffer, setting: HdrWorkbench, legacy: Option<crate::proof_sim::HdrPreview>) -> Result<Buffer> {
    let mut input = std::borrow::Cow::Borrowed(buf);
    if let Some(h) = legacy {
        let input = input.to_mut();
        let display = crate::color_cmds::ColorState::default().canvas_display(doc)?;
        for p in &mut input.px {
            for v in &mut p[..3] {
                let linear = if display.encode_srgb { *v } else { photocraft_color::convert::srgb_to_linear(*v) };
                let adjusted = (linear.max(0.0) * 2.0f32.powf(h.exposure.clamp(-20.0, 20.0))).max(1e-12).powf(1.0 / h.gamma.max(0.1)).min(65504.0);
                *v = if display.encode_srgb { adjusted } else { photocraft_color::convert::linear_to_srgb(adjusted) };
            }
        }
    }
    let mut b = linear_srgb(doc, &input)?;
    map_sdr(&mut b, setting, setting.clipping);
    Ok(b)
}
/// A bounded proxy histogram for UI diagnostics, computed only when requested.
pub fn diagnostics(doc: &Document) -> Result<Value> {
    let longest = doc.size.width.max(doc.size.height).max(1);
    let k = longest.div_ceil(512).max(1);
    let proxy = photocraft_compose::proxy::proxy_document(doc, k);
    let b = photocraft_compose::render(&proxy, proxy.bounds());
    let b = linear_srgb(doc, &b)?;
    let bins = photocraft_color::hdr::histogram(b.px.iter().copied());
    let peak = b.px.iter().filter(|p| p[3] > 0.0).flat_map(|p| p[..3].iter().copied()).fold(0.0f32, f32::max);
    Ok(json!({"bins":bins.as_slice(),"minEv":-8,"maxEv":8,"whiteEv":0,"peak":peak,"sampled":true,"colorSpace":"linear sRGB"}))
}
pub fn sdr_png(s: &Session) -> Result<Vec<u8>> {
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    if crate::proof_sim::hdr_active(&s.color, doc) || s.color.proof(doc.id).enabled {
        return Err(EngineError::Other(
            "Matching SDR export requires Proof Colors off and legacy 32-bit Preview Options reset; use HDR Workbench exposure instead".into(),
        ));
    }
    let pixels = u64::from(doc.size.width) * u64::from(doc.size.height);
    if pixels > 36_000_000 {
        return Err(EngineError::Other("SDR preview export is limited to 36 MP; original HDR export remains available".into()));
    }
    let b = photocraft_compose::render(doc, doc.bounds());
    let b = sdr_buffer(doc, &b, settings(s, doc))?;
    let img = b.to_rgba8();
    let image = photocraft_codecs::Image::from_u8(img.width, img.height, photocraft_codecs::ChannelLayout::Rgba, img.pixels)
        .map_err(|e| EngineError::Other(e.to_string()))?
        .with_icc(Some(Builtin::Srgb.profile().to_bytes().to_vec()));
    photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(|e| EngineError::Other(e.to_string()))
}
fn export(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "file.export.sdrPreview";
    let path = p.get("path").and_then(Value::as_str).filter(|p| !p.is_empty()).ok_or_else(|| bad(CMD, "path required"))?;
    if !path.to_ascii_lowercase().ends_with(".png") {
        return Err(bad(CMD, "SDR preview export requires .png"));
    }
    let bytes = sdr_png(s)?;
    crate::file_cmds::write_file(path, &bytes)?;
    Ok(json!({"path":path,"bytes":bytes.len(),"colorSpace":"sRGB","toneMap":settings(s,&s.active().ok_or(EngineError::NoDocument)?.doc)}))
}
pub fn validate_swatches(colors: &[[f32; 4]]) -> std::result::Result<(), String> {
    if colors.len() > 32
        || colors.iter().any(|p| p[..3].iter().any(|v| !v.is_finite() || v.abs() > 65504.0) || !p[3].is_finite() || !(0.0..=1.0).contains(&p[3]))
    {
        return Err("HDR swatches require at most 32 finite linear-sRGB colors, RGB ±65504 and alpha 0..1".into());
    }
    Ok(())
}
/// Convert persisted linear-sRGB swatches into this document's composite encoding.
pub fn swatch_colors(s: &Session, doc: &Document) -> Result<Vec<[f32; 4]>> {
    validate_swatches(&s.prefs().hdr_swatches).map_err(|e| bad("tools.hdrSwatches", e))?;
    let dst = crate::color_cmds::composite_profile(doc);
    let t = Transform::new(Builtin::LinearSrgb.profile(), &dst, Intent::RelativeColorimetric, true).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(s.prefs()
        .hdr_swatches
        .iter()
        .take(32)
        .map(|p| {
            let mut rgb = [0.0; 3];
            t.eval(&p[..3], &mut rgb);
            [rgb[0].clamp(0.0, 65504.0), rgb[1].clamp(0.0, 65504.0), rgb[2].clamp(0.0, 65504.0), p[3].clamp(0.0, 1.0)]
        })
        .collect())
}
fn swatches(s: &mut Session, p: &Value) -> Result<Value> {
    if !p.is_object() {
        return Err(bad("tools.hdrSwatches", "expected object"));
    }
    if p.get("action").is_some_and(|v| !v.is_string()) {
        return Err(bad("tools.hdrSwatches", "action must be a string"));
    }
    const CMD: &str = "tools.hdrSwatches";
    match p.get("action").and_then(Value::as_str) {
        Some("save") => {
            if s.prefs().hdr_swatches.len() >= 32 {
                return Err(bad(CMD, "32 swatch limit reached"));
            }
            let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
            let src = crate::color_cmds::composite_profile(doc);
            let t = Transform::new(&src, Builtin::LinearSrgb.profile(), Intent::RelativeColorimetric, true).map_err(|e| EngineError::Other(e.to_string()))?;
            let color = s.tools.foreground;
            let mut rgb = [0.0; 3];
            t.eval(&color[..3], &mut rgb);
            if rgb.iter().any(|v| !v.is_finite() || *v < -65504.0 || *v > 65504.0) {
                return Err(bad(CMD, "color is outside the supported linear-sRGB range"));
            }
            let c = [rgb[0], rgb[1], rgb[2], color[3]];
            s.edit_prefs(|prefs| prefs.hdr_swatches.push(c));
        }
        Some("recall") => {
            let index = p.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(CMD, "index required"))?;
            let colors = swatch_colors(s, &s.active().ok_or(EngineError::NoDocument)?.doc)?;
            let color = colors.get(index).copied().ok_or_else(|| bad(CMD, "index out of range"))?;
            s.tools.foreground = color;
        }
        Some("clear") => {
            s.edit_prefs(|prefs| prefs.hdr_swatches.clear());
        }
        None | Some("list") => {}
        _ => return Err(bad(CMD, "action: save|recall|clear|list")),
    }
    Ok(json!({"colors":s.prefs().hdr_swatches,"colorSpace":"linear sRGB"}))
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "tools.hdrSwatches",
            label: "HDR Swatches",
            menu: &[],
            shortcut: None,
            params: r#"{"action":"save|recall|clear|list","index":0..31}"#,
            enabled: has_doc,
            run: swatches,
            journal: false,
        },
        CommandSpec {
            id: "view.hdrWorkbench",
            label: "HDR Workbench Settings",
            menu: &[],
            shortcut: None,
            params: r#"{method:natural|filmic|clip,exposure:-20..20,shoulder:0.1..0.95,contrast:0.25..2,saturation:0..2,split:bool,clipping:bool} (all optional; {} queries)"#,
            enabled: has_doc,
            run: configure,
            journal: false,
        },
        CommandSpec {
            id: "view.hdrDiagnostics",
            label: "HDR Diagnostics",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: has_doc,
            run: |s, _| diagnostics(&s.active().ok_or(EngineError::NoDocument)?.doc),
            journal: false,
        },
        CommandSpec {
            id: "file.export.sdrPreview",
            label: "Export SDR Preview…",
            menu: &["File", "Export"],
            shortcut: None,
            params: r#"{"path":"output.png"}"#,
            enabled: crate::file_cmds::native_doc,
            run: export,
            journal: false,
        },
    ]
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paint_pick_save_swatches_and_sdr_export_preserve_intensity() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":16,"height":16,"depth":32})).unwrap();
        s.active_mut().unwrap().doc = std::sync::Arc::new({
            let mut d = (*s.active().unwrap().doc).clone();
            d.icc_profile = Some(Builtin::LinearSrgb.profile().to_bytes());
            d
        });
        s.execute("tools.setColors", json!({"foreground":[4.0,2.0,1.0,1.0]})).unwrap();
        s.execute("paint.stroke", json!({"points":[[8,8,1]],"size":8,"hardness":1,"opacity":1,"flow":1})).unwrap();
        let pixel = s.execute("document.pixel", json!({"x":8,"y":8})).unwrap();
        assert!(pixel[0].as_f64().unwrap() > 3.9, "{pixel}");
        s.execute("tools.hdrSwatches", json!({"action":"save"})).unwrap();
        let prefs = serde_json::to_vec(s.prefs()).unwrap();
        let restored: crate::prefs::Preferences = serde_json::from_slice(&prefs).unwrap();
        assert!(restored.hdr_swatches[0][0] > 3.9);
        s.execute("tools.defaultColors", json!({})).unwrap();
        s.execute("tools.hdrSwatches", json!({"action":"recall","index":0})).unwrap();
        assert!((s.tools.foreground[0] - 4.0).abs() < 0.02);
        let doc = &s.active().unwrap().doc;
        let bundle = photocraft_format::save_to_bytes(doc, &Default::default()).unwrap();
        let loaded = photocraft_format::load_from_bytes(&bundle).unwrap();
        let b = photocraft_compose::render(&loaded, loaded.bounds());
        assert!(b.px.iter().any(|p| p[0] > 3.9));
        let png = sdr_png(&s).unwrap();
        let image = photocraft_codecs::decode(&png).unwrap();
        assert!(image.icc.is_some());
        let pixels = image.to_rgba_f32();
        let expected = settings(&s, doc).apply([4.0, 2.0, 1.0], 1.0).map(photocraft_color::convert::linear_to_srgb);
        let offset = (8 * 16 + 8) * 4;
        for (channel, value) in expected.into_iter().enumerate() {
            assert!((pixels[offset + channel] - value).abs() < 2.0 / 255.0);
        }

        assert!(s.execute("tools.hdrSwatches", json!({"action":"recall","index":1000})).is_err());
    }
    #[test]
    fn profiles_depths_alpha_and_preview_overlays() {
        for depth in [photocraft_color::SampleType::U8, photocraft_color::SampleType::U16, photocraft_color::SampleType::F32] {
            for profile in [Builtin::Srgb, Builtin::DisplayP3, Builtin::AdobeRgbCompat, Builtin::LinearSrgb] {
                let mut doc = Document::new("ICC", photocraft_doc::Size::new(2, 1), photocraft_color::ColorMode::Rgb, depth);
                doc.icc_profile = Some(profile.profile().to_bytes());
                let b = Buffer { rect: doc.bounds(), px: vec![[4.0, 2.0, 1.0, 0.5], [f32::NAN, f32::INFINITY, -1.0, 0.0]] };
                let mapped = sdr_buffer(&doc, &b, HdrWorkbench::default()).unwrap();
                assert_eq!(mapped.px[0][3], 0.5);
                assert_eq!(mapped.px[1][3], 0.0);
                assert!(mapped.px.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
            }
        }
        let doc = Document::new("overlay", photocraft_doc::Size::new(2, 1), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::F32);
        let b = Buffer::filled(doc.bounds(), [4.0, 4.0, 4.0, 1.0]);
        let setting = HdrWorkbench { clipping: true, ..Default::default() };
        let preview = preview_buffer(&doc, &b, setting, None).unwrap();
        let export = sdr_buffer(&doc, &b, setting).unwrap();
        assert_eq!(preview.px[0][1], 0.0);
        assert!(export.px[0][1] > 0.9);
    }
    #[test]
    fn malformed_swatch_preferences_are_rejected_and_closed_views_are_released() {
        let mut s = Session::new();
        assert!(s.load_prefs_json(r#"{"hdrSwatches":[[1e300,0,0,1]]}"#).is_err());
        assert!(s.execute("prefs.set", json!({"path":"hdrSwatches","value":[[0,0,0,2]]})).is_err());
        s.execute("file.new", json!({"width":8,"height":8,"depth":32})).unwrap();
        s.execute("view.hdrWorkbench", json!({"exposure":1})).unwrap();
        let id = s.active().unwrap().doc.id;
        assert!(s.color.workbench.contains_key(&id));
        assert!(s.close(0).is_some());
        assert!(!s.color.workbench.contains_key(&id));
    }
    #[test]
    fn color_validation_does_not_partially_change_tools() {
        let mut s = Session::new();
        let original = s.tools.foreground;
        for p in [
            json!({"foreground":[4,2,1],"background":[0,0,"wrong"]}),
            json!({"foreground":[1e300,0,0]}),
            json!({"foreground":"#0é000"}),
            json!({"foreground":[1,2]}),
            json!({"foreground":[1,2,3,2]}),
        ] {
            assert!(s.execute("tools.setColors", p).is_err());
            assert_eq!(s.tools.foreground, original);
        }
    }
    #[test]
    fn settings_are_atomic_and_validate_types() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":8,"height":8,"depth":32})).unwrap();
        s.execute("view.hdrWorkbench", json!({"exposure":2,"method":"filmic"})).unwrap();
        for p in [json!({"exposure":99}), json!({"method":"aces"}), json!({"split":"yes"}), json!({"shoulder":1}), json!({"contrast":1e300}), json!([])] {
            assert!(s.execute("view.hdrWorkbench", p).is_err());
            assert_eq!(s.execute("view.hdrWorkbench", json!({})).unwrap()["exposure"].as_f64(), Some(2.0));
        }
        assert!(s.execute("file.export.sdrPreview", json!({})).is_err());
    }
}
